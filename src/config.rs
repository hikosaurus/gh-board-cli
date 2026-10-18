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
    /// HookType (`{event_name}:{event.action}`) -> HookActions
    #[serde(default)]
    pub hooks: BTreeMap<String, Vec<HookAction>>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookAction {
    Command { command: String },
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
                command: "echo ${{ event.issue.id }}".into()
            }]
        );
    }
}
