use std::process::Stdio;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::Value;
use tokio::process::Command;

use crate::config::{Config, HookAction};
use crate::payload::Payload;

static VARIABLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{\{\s*([A-Za-z0-9_.\-]+)\s*\}\}").unwrap());

/// Glob match where `*` matches any sequence (including empty).
pub fn matches(pattern: &str, hook_type: &str) -> bool {
    let regex = format!(
        "^{}$",
        pattern
            .split('*')
            .map(regex::escape)
            .collect::<Vec<_>>()
            .join(".*")
    );
    Regex::new(&regex).is_ok_and(|r| r.is_match(hook_type))
}

/// Hook actions whose HookType matches the payload, in config order.
pub fn select<'a>(config: &'a Config, payload: &Payload) -> Vec<&'a HookAction> {
    let hook_type = payload.hook_type();
    config
        .hooks
        .iter()
        .filter(|(pattern, _)| matches(pattern, &hook_type))
        .flat_map(|(_, actions)| actions)
        .collect()
}

/// Replace `${{ name.path.to.value }}` with built-in variables.
pub fn expand(template: &str, payload: &Payload) -> String {
    VARIABLE
        .replace_all(template, |caps: &Captures| {
            lookup(&caps[1], payload).unwrap_or_default()
        })
        .into_owned()
}

fn lookup(path: &str, payload: &Payload) -> Option<String> {
    let mut keys = path.split('.');
    let root = match keys.next()? {
        "event_name" => Value::String(payload.event_name.clone()),
        "event" => payload.event.clone(),
        _ => return None,
    };
    let value = keys.try_fold(root, |v, key| match v {
        Value::Object(mut map) => map.remove(key),
        Value::Array(mut arr) => {
            let i: usize = key.parse().ok()?;
            (i < arr.len()).then(|| arr.swap_remove(i))
        }
        _ => None,
    })?;
    Some(match value {
        Value::String(s) => s,
        v => v.to_string(),
    })
}

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub status: Option<i32>,
}

pub async fn run(action: &HookAction, payload: &Payload) -> CommandOutput {
    let HookAction::Command { command } = action;
    let command = expand(command, payload);
    let output = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .stdin(Stdio::null())
        .output()
        .await;
    match output {
        Ok(o) => CommandOutput {
            command,
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            status: o.status.code(),
        },
        Err(e) => CommandOutput {
            command,
            stdout: String::new(),
            stderr: e.to_string(),
            status: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Payload {
        Payload {
            repository: "owner/repo".into(),
            event_name: "issues".into(),
            event: json!({
                "action": "opened",
                "issue": { "id": 123, "number": 3, "title": "hello", "labels": [{ "name": "bug" }] }
            }),
        }
    }

    #[test]
    fn match_patterns() {
        assert!(matches("issues:opened", "issues:opened"));
        assert!(matches("issues:*", "issues:opened"));
        assert!(matches("*", "issue_comment:created"));
        assert!(matches("*:opened", "issues:opened"));
        assert!(!matches("issues:*", "issue_comment:created"));
        assert!(!matches("issues:opened", "issues:closed"));
        assert!(!matches("issues.x", "issuesyx"));
    }

    #[test]
    fn expand_variables() {
        let p = payload();
        assert_eq!(expand("echo ${{ event.issue.id }}", &p), "echo 123");
        assert_eq!(
            expand("${{event_name}}:${{ event.action }}", &p),
            "issues:opened"
        );
        assert_eq!(expand("${{ event.issue.title }}", &p), "hello");
        assert_eq!(expand("${{ event.issue.labels.0.name }}", &p), "bug");
        assert_eq!(expand("[${{ event.nothing }}]", &p), "[]");
        assert_eq!(
            expand("${{ event.issue.labels }}", &p),
            r#"[{"name":"bug"}]"#
        );
    }

    #[test]
    fn select_hooks() {
        let config = Config::parse(
            r#"{
                "listen": "wss://x",
                "hooks": {
                    "issues:*": [{ "type": "command", "command": "a" }],
                    "issues:closed": [{ "type": "command", "command": "b" }],
                    "issue_comment:*": [{ "type": "command", "command": "c" }]
                }
            }"#,
        )
        .unwrap();
        let selected = select(&config, &payload());
        assert_eq!(
            selected,
            vec![&HookAction::Command {
                command: "a".into()
            }]
        );
    }

    #[tokio::test]
    async fn run_command() {
        let out = run(
            &HookAction::Command {
                command: "echo ${{ event.issue.number }}; echo err >&2; exit 2".into(),
            },
            &payload(),
        )
        .await;
        assert_eq!(out.command, "echo 3; echo err >&2; exit 2");
        assert_eq!(out.stdout, "3\n");
        assert_eq!(out.stderr, "err\n");
        assert_eq!(out.status, Some(2));
    }
}
