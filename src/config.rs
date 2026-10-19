use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

pub const CONFIG_FILE_NAME: &str = ".ghboard.json";

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// WebSocket URL to subscribe
    pub listen: String,
    /// Pusher channel to subscribe after connection (optional)
    #[serde(default)]
    pub channel: Option<String>,
    /// Repositories (`{owner}/{repo}`) to handle. Empty means all repositories.
    #[serde(default)]
    pub targets: Vec<String>,
    /// HookType (`{event_name}:{event_action}`) -> HookActions
    #[serde(default)]
    pub hooks: BTreeMap<String, Vec<HookAction>>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookAction {
    Command {
        /// Issue statuses to run on. Empty means any status.
        #[serde(default)]
        status: Vec<String>,
        command: String,
    },
}

impl HookAction {
    pub fn statuses(&self) -> &[String] {
        let HookAction::Command { status, .. } = self;
        status
    }

    /// Whether the action runs for an issue with the given project statuses.
    pub fn accepts(&self, issue_statuses: &[String]) -> bool {
        let statuses = self.statuses();
        statuses.is_empty() || issue_statuses.iter().any(|s| statuses.contains(s))
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("failed to parse {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(serde_json::from_str(text)?)
    }

    pub fn is_target(&self, repository: &str) -> bool {
        self.targets.is_empty()
            || self
                .targets
                .iter()
                .any(|t| t.eq_ignore_ascii_case(repository))
    }

    /// `.ghboard.json` in the current directory, falling back to the home directory.
    pub fn find() -> Option<PathBuf> {
        let local = PathBuf::from(CONFIG_FILE_NAME);
        if local.is_file() {
            return Some(local);
        }
        let home = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(CONFIG_FILE_NAME))?;
        home.is_file().then_some(home)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_example_config() {
        let config = Config::parse(
            r#"{
                "listen": "wss://example.com/app/key",
                "hooks": {
                    "issues:*": [
                        { "type": "command", "command": "echo ${{ event.issue.id }}" }
                    ]
                }
            }"#,
        )
        .unwrap();
        assert_eq!(config.listen, "wss://example.com/app/key");
        assert_eq!(config.channel, None);
        assert_eq!(
            config.hooks["issues:*"],
            vec![HookAction::Command {
                status: vec![],
                command: "echo ${{ event.issue.id }}".into()
            }]
        );
    }

    #[test]
    fn action_status() {
        let config = Config::parse(
            r#"{
                "listen": "ws://x",
                "hooks": {
                    "issues:*": [
                        { "type": "command", "status": ["In Progress", "In Review"], "command": "a" },
                        { "type": "command", "status": [], "command": "b" }
                    ]
                }
            }"#,
        )
        .unwrap();
        let [some, any] = &config.hooks["issues:*"][..] else {
            panic!()
        };
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(some.accepts(&s(&["In Review"])));
        assert!(some.accepts(&s(&["Todo", "In Progress"])));
        assert!(!some.accepts(&s(&["Done"])));
        assert!(!some.accepts(&s(&["in review"])));
        assert!(!some.accepts(&[]));
        assert!(any.accepts(&[]));
        assert!(any.accepts(&s(&["Done"])));
    }

    #[test]
    fn targets() {
        let all = Config::parse(r#"{ "listen": "ws://x" }"#).unwrap();
        assert!(all.is_target("any/repo"));

        let some = Config::parse(r#"{ "listen": "ws://x", "targets": ["Owner/Repo01"] }"#).unwrap();
        assert!(some.is_target("owner/repo01"));
        assert!(!some.is_target("owner/repo02"));
    }
}
