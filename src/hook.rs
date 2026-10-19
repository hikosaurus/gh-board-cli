use std::collections::HashMap;
use std::process::Stdio;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
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

/// Replace `${{ name }}` (or `${{ name.path.to.value }}`) with payload fields.
pub fn expand(template: &str, payload: &Payload) -> String {
    VARIABLE
        .replace_all(template, |caps: &Captures| {
            lookup(&caps[1], payload).unwrap_or_default()
        })
        .into_owned()
}

fn lookup(path: &str, payload: &Payload) -> Option<String> {
    let mut keys = path.split('.');
    let root = payload.fields.get(keys.next()?)?.clone();
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

/// Last issue comment `(id, body)`; `None` when the issue has no comments.
pub type Comment = Option<(String, String)>;

/// Issue state used to decide whether hook actions run.
#[derive(Debug, Clone, PartialEq)]
pub struct IssueState {
    pub comment: Comment,
    /// Status of the issue in each project it belongs to.
    pub statuses: Vec<String>,
}

const ISSUE_STATE_JQ: &str =
    "{comment: .comments[-1], statuses: [.projectItems[].status.name | select(. != null)]}";

/// Fetch the last comment and project statuses of the issue with
/// `gh issue view <n> -R <repo> --json comments,projectItems`.
pub async fn issue_state(repository: &str, number: u64) -> Result<IssueState, String> {
    let output = Command::new("gh")
        .args(["issue", "view", &number.to_string(), "-R", repository])
        .args(["--json", "comments,projectItems", "--jq", ISSUE_STATE_JQ])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("gh: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "gh: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_issue_state(&String::from_utf8_lossy(&output.stdout))
}

fn parse_issue_state(text: &str) -> Result<IssueState, String> {
    let value: Value = serde_json::from_str(text.trim()).map_err(|e| e.to_string())?;
    let comment = match &value["comment"] {
        Value::Null => None,
        c => {
            let field = |key: &str| c[key].as_str().unwrap_or_default().to_string();
            Some((field("id"), field("body")))
        }
    };
    let statuses = value["statuses"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect();
    Ok(IssueState { comment, statuses })
}

/// Remembers the last comment of each issue to detect new comments.
#[derive(Debug, Default)]
pub struct CommentTracker {
    last: HashMap<(String, u64), Comment>,
}

impl CommentTracker {
    /// Store the comment and return whether it differs from the previous one
    /// (the first observation counts as changed).
    pub fn update(&mut self, repository: &str, number: u64, comment: Comment) -> bool {
        let key = (repository.to_ascii_lowercase(), number);
        self.last.insert(key, comment.clone()) != Some(comment)
    }
}

/// Progress of a running hook action.
#[derive(Debug, Clone, PartialEq)]
pub enum HookOutput {
    /// The command (with variables expanded) was started
    Started(String),
    Stdout(String),
    Stderr(String),
}

/// Run the action and report its output line by line as it arrives.
/// Returns the exit code (`None` when killed by a signal or failed to start).
pub async fn run(
    action: &HookAction,
    payload: &Payload,
    on_output: impl Fn(HookOutput),
) -> Option<i32> {
    let HookAction::Command { command, .. } = action;
    let command = expand(command, payload);
    on_output(HookOutput::Started(command.clone()));
    let child = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            on_output(HookOutput::Stderr(e.to_string()));
            return None;
        }
    };
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    tokio::join!(
        read_lines(stdout, |line| on_output(HookOutput::Stdout(line))),
        read_lines(stderr, |line| on_output(HookOutput::Stderr(line))),
    );
    match child.wait().await {
        Ok(status) => status.code(),
        Err(e) => {
            on_output(HookOutput::Stderr(e.to_string()));
            None
        }
    }
}

/// Call `on_line` for each line (lossy UTF-8, without the line break) until EOF.
async fn read_lines(reader: impl AsyncRead + Unpin, on_line: impl Fn(String)) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf);
                on_line(line.trim_end_matches(['\n', '\r']).to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Payload {
        Payload::from_value(json!({
            "repository": "owner/repo",
            "event_name": "issues",
            "event_action": "opened",
            "issue_number": 3,
            "issue": { "title": "hello", "labels": [{ "name": "bug" }] }
        }))
        .unwrap()
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
        assert_eq!(expand("echo ${{ issue_number }}", &p), "echo 3");
        assert_eq!(
            expand("${{event_name}}:${{ event_action }}", &p),
            "issues:opened"
        );
        assert_eq!(expand("${{ repository }}", &p), "owner/repo");
        // extra fields and nested values
        assert_eq!(expand("${{ issue.title }}", &p), "hello");
        assert_eq!(expand("${{ issue.labels.0.name }}", &p), "bug");
        assert_eq!(expand("${{ issue.labels }}", &p), r#"[{"name":"bug"}]"#);
        assert_eq!(expand("[${{ nothing }}]", &p), "[]");
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
                status: vec![],
                command: "a".into()
            }]
        );
    }

    fn command(command: &str) -> HookAction {
        HookAction::Command {
            status: vec![],
            command: command.into(),
        }
    }

    #[tokio::test]
    async fn run_command() {
        let outputs = std::sync::Mutex::new(vec![]);
        let status = run(
            &command("echo ${{ issue_number }}; echo err >&2; printf 'a\\nb'; exit 2"),
            &payload(),
            |o| outputs.lock().unwrap().push(o),
        )
        .await;
        assert_eq!(status, Some(2));
        let outputs = outputs.into_inner().unwrap();
        assert_eq!(
            outputs[0],
            HookOutput::Started("echo 3; echo err >&2; printf 'a\\nb'; exit 2".into())
        );
        let stdout: Vec<_> = outputs
            .iter()
            .filter_map(|o| match o {
                HookOutput::Stdout(line) => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(stdout, ["3", "a", "b"]);
        assert!(outputs.contains(&HookOutput::Stderr("err".into())));
    }

    #[tokio::test]
    async fn output_is_reported_before_exit() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            run(
                &command("echo first; sleep 1; echo last"),
                &payload(),
                |o| {
                    let _ = tx.send(o);
                },
            )
            .await
        });
        assert!(matches!(rx.recv().await, Some(HookOutput::Started(_))));
        let first = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await;
        assert_eq!(first.unwrap(), Some(HookOutput::Stdout("first".into())));
        assert!(!task.is_finished());
        assert_eq!(task.await.unwrap(), Some(0));
    }

    #[test]
    fn parse_state() {
        assert_eq!(
            parse_issue_state(r#"{"comment":null,"statuses":[]}"#),
            Ok(IssueState {
                comment: None,
                statuses: vec![]
            })
        );
        assert_eq!(
            parse_issue_state(
                r#"{"comment":{"id":"IC_1","body":"hi","author":{"login":"me"}},"statuses":["Todo","Done"]}"#
            ),
            Ok(IssueState {
                comment: Some(("IC_1".into(), "hi".into())),
                statuses: vec!["Todo".into(), "Done".into()]
            })
        );
        assert!(parse_issue_state("{").is_err());
    }

    #[test]
    fn track_comment_changes() {
        let c = |id: &str, body: &str| Some((id.to_string(), body.to_string()));
        let mut tracker = CommentTracker::default();
        // first observation
        assert!(tracker.update("o/r", 1, None));
        assert!(!tracker.update("o/r", 1, None));
        assert!(tracker.update("o/r", 1, c("1", "a")));
        assert!(!tracker.update("O/R", 1, c("1", "a")));
        // edited body
        assert!(tracker.update("o/r", 1, c("1", "b")));
        // other issue is tracked separately
        assert!(tracker.update("o/r", 2, c("1", "b")));
    }
}
