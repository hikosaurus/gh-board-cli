use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Config file names in lookup order.
pub const CONFIG_FILE_NAMES: [&str; 2] = [".ghboard.yaml", ".ghboard.yml"];

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// WebSocket URL to subscribe (optional)
    #[serde(default)]
    pub listen: Option<String>,
    /// Pusher channel to subscribe after connection (optional)
    #[serde(default)]
    pub channel: Option<String>,
    /// Repositories (`{owner}/{repo}`) to handle. Empty means all repositories.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Project URLs to watch with the browser (optional)
    #[serde(default)]
    pub projects: Vec<String>,
    /// Hook actions in config order
    #[serde(default)]
    pub hooks: Vec<HookAction>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct HookAction {
    /// HookType (`{event_name}:{event_action}`) patterns to run on
    pub on: Vec<String>,
    /// Issue statuses to run on. Empty means any status.
    #[serde(default)]
    pub status: Vec<String>,
    pub command: String,
}

impl HookAction {
    /// Whether the action runs for an issue with the given project statuses.
    pub fn accepts(&self, issue_statuses: &[String]) -> bool {
        self.status.is_empty() || issue_statuses.iter().any(|s| self.status.contains(s))
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("failed to parse {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = serde_yaml_ng::from_str(text)?;
        if config.listen.is_none() && config.projects.is_empty() {
            bail!("either `listen` or `projects` is required");
        }
        for url in &config.projects {
            if !is_project_url(url) {
                bail!("invalid project URL: {url}");
            }
        }
        Ok(config)
    }

    pub fn is_target(&self, repository: &str) -> bool {
        self.targets.is_empty()
            || self
                .targets
                .iter()
                .any(|t| t.eq_ignore_ascii_case(repository))
    }

    /// `.ghboard.yaml` (or `.ghboard.yml`) in the current directory,
    /// falling back to the home directory.
    pub fn find() -> Option<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        [Some(PathBuf::new()), home]
            .into_iter()
            .flatten()
            .flat_map(|dir| CONFIG_FILE_NAMES.map(|name| dir.join(name)))
            .find(|path| path.is_file())
    }
}

/// `https://github.com/{users|orgs}/{owner}/projects/{number}` (optionally
/// followed by a view path or query)
pub fn is_project_url(url: &str) -> bool {
    let Some(path) = url.strip_prefix("https://github.com/") else {
        return false;
    };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let segments: Vec<&str> = path.split('/').collect();
    matches!(
        segments[..],
        ["users" | "orgs", owner, "projects", number, ..]
            if !owner.is_empty() && number.parse::<u64>().is_ok()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_example_config() {
        let config = Config::parse(
            r#"
listen: wss://example.com/app/key
hooks:
  - on:
      - "issues:*"
      - "issue_comment:*"
    status:
      - 'In Progress'
      - 'In Review'
    command: |
      echo ${{ event.issue.id }}
"#,
        )
        .unwrap();
        assert_eq!(config.listen.as_deref(), Some("wss://example.com/app/key"));
        assert_eq!(config.channel, None);
        assert_eq!(
            config.hooks,
            vec![HookAction {
                on: vec!["issues:*".into(), "issue_comment:*".into()],
                status: vec!["In Progress".into(), "In Review".into()],
                command: "echo ${{ event.issue.id }}\n".into()
            }]
        );
    }

    #[test]
    fn on_is_required() {
        assert!(Config::parse("listen: ws://x\nhooks:\n  - command: a").is_err());
    }

    #[test]
    fn action_status() {
        let config = Config::parse(
            r#"
listen: ws://x
hooks:
  - on: ["issues:*"]
    status: [In Progress, In Review]
    command: a
  - on: ["issues:*"]
    command: b
"#,
        )
        .unwrap();
        let [some, any] = &config.hooks[..] else {
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
        let all = Config::parse("listen: ws://x").unwrap();
        assert!(all.is_target("any/repo"));

        let some = Config::parse("listen: ws://x\ntargets:\n  - Owner/Repo01").unwrap();
        assert!(some.is_target("owner/repo01"));
        assert!(!some.is_target("owner/repo02"));
    }

    #[test]
    fn parse_readme_example() {
        let readme = include_str!("../README.md");
        let example = readme
            .split("```yaml\n")
            .nth(1)
            .and_then(|s| s.split("```").next())
            .unwrap();
        let config = Config::parse(example).unwrap();
        let [first, second] = &config.hooks[..] else {
            panic!()
        };
        assert_eq!(first.on, ["issues:*", "issue_comment:*"]);
        assert_eq!(first.status, ["In Progress", "In Review"]);
        assert_eq!(
            second,
            &HookAction {
                on: vec!["issue_comment:created".into()],
                status: vec![],
                command: r#"echo "${{ event_name }}:${{ event_action }} #${{ issue_number }}""#
                    .into()
            }
        );
    }

    #[test]
    fn listen_or_projects_is_required() {
        assert!(Config::parse("hooks: []").is_err());
        let config =
            Config::parse("projects:\n  - https://github.com/users/me/projects/6").unwrap();
        assert_eq!(config.listen, None);
        assert_eq!(config.projects, ["https://github.com/users/me/projects/6"]);
        assert!(Config::parse("projects:\n  - https://github.com/me/repo").is_err());
    }

    #[test]
    fn project_url() {
        assert!(is_project_url("https://github.com/users/me/projects/6"));
        assert!(is_project_url(
            "https://github.com/orgs/org/projects/1/views/2"
        ));
        assert!(is_project_url(
            "https://github.com/orgs/org/projects/1?pane=info"
        ));
        assert!(!is_project_url("https://github.com/users/me/projects/x"));
        assert!(!is_project_url("https://github.com/users/me/projects"));
        assert!(!is_project_url("https://github.com/me/repo/projects/1"));
        assert!(!is_project_url("http://github.com/users/me/projects/6"));
    }

    #[test]
    fn parse_json_style() {
        // YAML is a superset of JSON, so JSON config also parses.
        let config = Config::parse(
            r#"{ "listen": "ws://x", "hooks": [{ "on": ["issues:*"], "command": "a" }] }"#,
        )
        .unwrap();
        assert_eq!(config.hooks.len(), 1);
    }
}
