use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::app::AppEvent;
use crate::payload::Payload;

const INTERVAL: Duration = Duration::from_secs(10);
const RETRY_DELAY: Duration = Duration::from_secs(10);
const LOGIN_CHECK_INTERVAL: Duration = Duration::from_secs(2);
const GITHUB_URL: &str = "https://github.com/";
const LOGIN_URL: &str = "https://github.com/login";

/// Fetch a project page in the browser and read the data embedded for the
/// page: the Status field (`memex-columns-data`) and the URL of the items
/// endpoint used by the page (`memex-paginated-items-get-api-data`).
/// Then read all items from the endpoint.
///
/// Returns `{ columns, nodes }` or `{ error }`.
const FETCH_PROJECT_JS: &str = r#"
(async (url) => {
  const res = await fetch(url, { credentials: 'include', headers: { Accept: 'text/html' } });
  if (!res.ok) return { error: `${res.status} ${res.statusText}` };
  if (new URL(res.url).pathname === '/login') return { error: 'login required' };
  const doc = new DOMParser().parseFromString(await res.text(), 'text/html');
  const json = (id) => {
    const e = doc.getElementById(id);
    return e ? JSON.parse(e.textContent) : null;
  };
  const columns = json('memex-columns-data');
  const items = json('memex-paginated-items-get-api-data');
  if (!columns || !items) return { error: 'project data not found' };
  const nodes = [];
  let after = null;
  do {
    const page = new URL(items.url, location.origin);
    if (after) page.searchParams.set('after', after);
    const r = await fetch(page, { credentials: 'include', headers: { Accept: 'application/json' } });
    if (!r.ok) return { error: `items: ${r.status} ${r.statusText}` };
    const data = await r.json();
    nodes.push(...data.nodes);
    after = data.pageInfo?.hasNextPage ? data.pageInfo.endCursor : null;
  } while (after);
  return { columns, nodes };
})
"#;

/// Issue item of a project
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// `{owner}/{repo}`
    pub repository: String,
    pub number: u64,
    /// Status name (`None` if not set)
    pub status: Option<String>,
}

/// Project item id -> issue item
pub type Snapshot = HashMap<u64, Item>;

/// Watch the projects with the browser forever, sending detected changes as
/// `AppEvent::Watched`. Restarts the browser on failure.
pub async fn watch(projects: Vec<String>, tx: UnboundedSender<AppEvent>) {
    // Project URL -> (last snapshot, time when the fetch of it started)
    let mut snapshots: HashMap<String, (Snapshot, Instant)> = HashMap::new();
    loop {
        if let Err(e) = session(&projects, &mut snapshots, &tx).await {
            status(&tx, format!("{e:#}"));
        }
        if tx.is_closed() {
            return;
        }
        tokio::time::sleep(RETRY_DELAY).await;
    }
}

/// Poll the projects with a headless browser until login is required (then
/// log in with a visible browser and return) or an error occurs.
async fn session(
    projects: &[String],
    snapshots: &mut HashMap<String, (Snapshot, Instant)>,
    tx: &UnboundedSender<AppEvent>,
) -> Result<()> {
    let mut browser = Chrome::launch(true).await?;
    let page = browser.browser.new_page(GITHUB_URL).await?;
    loop {
        for url in projects {
            let started = Instant::now();
            let snapshot = match fetch_project(&page, url).await {
                Ok(snapshot) => snapshot,
                Err(e) => {
                    status(tx, format!("{url}: {e:#}"));
                    if !logged_in(&page).await? {
                        browser.close().await;
                        return login(tx).await;
                    }
                    continue;
                }
            };
            match snapshots.get(url) {
                Some((prev, since)) => {
                    for payload in diff(url, prev, &snapshot) {
                        let _ = tx.send(AppEvent::Watched(payload, *since));
                    }
                }
                None => status(tx, format!("watching {} issues in {url}", snapshot.len())),
            }
            snapshots.insert(url.clone(), (snapshot, started));
        }
        if tx.is_closed() {
            browser.close().await;
            return Ok(());
        }
        tokio::time::sleep(INTERVAL).await;
    }
}

/// Open a visible browser with the login page and wait until logged in.
async fn login(tx: &UnboundedSender<AppEvent>) -> Result<()> {
    status(tx, "log in to GitHub in the opened browser".into());
    let mut browser = Chrome::launch(false).await?;
    let page = browser.browser.new_page(LOGIN_URL).await?;
    let result = async {
        while !logged_in(&page).await? {
            tokio::time::sleep(LOGIN_CHECK_INTERVAL).await;
        }
        Ok(())
    }
    .await;
    // Close gracefully so that the session cookie is saved to the profile.
    browser.close().await;
    if result.is_ok() {
        status(tx, "logged in".into());
    }
    result
}

async fn fetch_project(page: &Page, url: &str) -> Result<Snapshot> {
    let expression = format!("{FETCH_PROJECT_JS}({})", json!(url));
    let data: Value = page.evaluate_expression(expression).await?.into_value()?;
    parse_project(&data)
}

async fn logged_in(page: &Page) -> Result<bool> {
    let cookies = page.get_cookies().await?;
    Ok(cookies
        .iter()
        .any(|c| c.name == "logged_in" && c.value == "yes"))
}

fn status(tx: &UnboundedSender<AppEvent>, text: String) {
    let _ = tx.send(AppEvent::Status(format!("browser: {text}")));
}

/// Chrome with the profile dedicated to this CLI
struct Chrome {
    browser: Browser,
    handler: JoinHandle<()>,
}

impl Chrome {
    async fn launch(headless: bool) -> Result<Self> {
        let mut config = BrowserConfig::builder().user_data_dir(profile_dir()?);
        if !headless {
            config = config.with_head().viewport(None);
        }
        let config = config.build().map_err(anyhow::Error::msg)?;
        let (browser, mut handler) = Browser::launch(config)
            .await
            .context("failed to launch Chrome")?;
        let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });
        Ok(Self { browser, handler })
    }

    async fn close(&mut self) {
        let _ = self.browser.close().await;
        let _ = self.browser.wait().await;
        self.handler.abort();
    }
}

/// `~/.ghboard/browser`
fn profile_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".ghboard").join("browser"))
}

/// Issue items in `{ columns, nodes }` returned by `FETCH_PROJECT_JS`.
fn parse_project(data: &Value) -> Result<Snapshot> {
    if let Some(error) = data["error"].as_str() {
        bail!("{error}");
    }
    let Some(columns) = data["columns"].as_array() else {
        bail!("project data not found");
    };
    let options: HashMap<&str, &str> = columns
        .iter()
        .find(|c| c["id"] == "Status")
        .and_then(|c| c["settings"]["options"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|o| Some((o["id"].as_str()?, o["name"].as_str()?)))
        .collect();

    let mut snapshot = Snapshot::new();
    for node in data["nodes"].as_array().into_iter().flatten() {
        if node["contentType"] != "Issue" {
            continue;
        }
        let value = |column: &str| {
            node["memexProjectColumnValues"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|v| v["memexProjectColumnId"] == column)
                .map(|v| &v["value"])
        };
        let url = node["content"]["url"].as_str().unwrap_or_default();
        let repository = value("Repository")
            .and_then(|v| v["nameWithOwner"].as_str())
            .map(str::to_string)
            .or_else(|| repository_from_url(url));
        let number = value("Title").and_then(|v| v["number"].as_u64());
        let (Some(id), Some(repository), Some(number)) = (node["id"].as_u64(), repository, number)
        else {
            continue;
        };
        let status = value("Status")
            .and_then(|v| v["id"].as_str())
            .and_then(|id| options.get(id))
            .map(|name| name.to_string());
        snapshot.insert(
            id,
            Item {
                repository,
                number,
                status,
            },
        );
    }
    Ok(snapshot)
}

/// `{owner}/{repo}` of `https://github.com/{owner}/{repo}/issues/{number}`
fn repository_from_url(url: &str) -> Option<String> {
    let path = url.strip_prefix("https://github.com/")?;
    let [owner, repo, "issues", _] = path.split('/').collect::<Vec<_>>()[..] else {
        return None;
    };
    Some(format!("{owner}/{repo}"))
}

/// Payloads for the changes from `prev` to `next`: `issues:opened` for added
/// items and `issues:edited` for status changes (in item id order).
fn diff(project: &str, prev: &Snapshot, next: &Snapshot) -> Vec<Payload> {
    let mut ids: Vec<&u64> = next.keys().collect();
    ids.sort();
    ids.into_iter()
        .filter_map(|id| {
            let item = &next[id];
            let (action, previous) = match prev.get(id) {
                None => ("opened", None),
                Some(p) if p.status != item.status => ("edited", Some(&p.status)),
                Some(_) => return None,
            };
            let mut payload = json!({
                "repository": item.repository,
                "event_name": "issues",
                "event_action": action,
                "issue_number": item.number,
                "status": item.status.clone().unwrap_or_default(),
                "project": project,
            });
            if let Some(previous) = previous {
                payload["previous_status"] = json!(previous.clone().unwrap_or_default());
            }
            Payload::from_value(payload)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u64, repository: Option<&str>, number: u64, status: Option<&str>) -> Value {
        let mut values = vec![json!({
            "memexProjectColumnId": "Title",
            "value": { "title": { "raw": "t" }, "number": number }
        })];
        if let Some(repository) = repository {
            values.push(json!({
                "memexProjectColumnId": "Repository",
                "value": { "nameWithOwner": repository }
            }));
        }
        if let Some(status) = status {
            values.push(json!({ "memexProjectColumnId": "Status", "value": { "id": status } }));
        }
        json!({
            "id": id,
            "contentType": "Issue",
            "content": { "url": format!("https://github.com/from/url/issues/{number}") },
            "memexProjectColumnValues": values
        })
    }

    fn columns() -> Value {
        json!([
            { "id": "Title" },
            { "id": "Status", "settings": { "options": [
                { "id": "a1", "name": "Todo" },
                { "id": "b2", "name": "In Progress" }
            ] } }
        ])
    }

    fn item(repository: &str, number: u64, status: Option<&str>) -> Item {
        Item {
            repository: repository.into(),
            number,
            status: status.map(str::to_string),
        }
    }

    #[test]
    fn parse_project_items() {
        let data = json!({
            "columns": columns(),
            "nodes": [
                node(1, Some("owner/repo"), 3, Some("a1")),
                node(2, None, 4, Some("b2")),
                node(3, Some("owner/repo"), 5, None),
                node(4, Some("owner/repo"), 6, Some("unknown")),
                { "id": 5, "contentType": "DraftIssue", "memexProjectColumnValues": [] },
                { "id": 6, "contentType": "PullRequest", "memexProjectColumnValues": [] }
            ]
        });
        let snapshot = parse_project(&data).unwrap();
        assert_eq!(snapshot.len(), 4);
        assert_eq!(snapshot[&1], item("owner/repo", 3, Some("Todo")));
        assert_eq!(snapshot[&2], item("from/url", 4, Some("In Progress")));
        assert_eq!(snapshot[&3], item("owner/repo", 5, None));
        assert_eq!(snapshot[&4], item("owner/repo", 6, None));
    }

    #[test]
    fn parse_project_error() {
        let e = parse_project(&json!({ "error": "404 Not Found" })).unwrap_err();
        assert_eq!(e.to_string(), "404 Not Found");
        assert!(parse_project(&json!({})).is_err());
    }

    #[test]
    fn repository_of_issue_url() {
        assert_eq!(
            repository_from_url("https://github.com/o/r/issues/1").as_deref(),
            Some("o/r")
        );
        assert_eq!(repository_from_url("https://github.com/o/r/pull/1"), None);
        assert_eq!(
            repository_from_url("https://example.com/o/r/issues/1"),
            None
        );
    }

    #[test]
    fn diff_snapshots() {
        let prev = Snapshot::from([
            (1, item("o/r", 1, Some("Todo"))),
            (2, item("o/r", 2, Some("Todo"))),
            (3, item("o/r", 3, None)),
            (9, item("o/r", 9, Some("Done"))),
        ]);
        let next = Snapshot::from([
            (1, item("o/r", 1, Some("Todo"))),
            (2, item("o/r", 2, Some("In Progress"))),
            (3, item("o/r", 3, Some("Todo"))),
            (4, item("o/x", 4, None)),
        ]);
        let payloads = diff("https://github.com/users/o/projects/1", &prev, &next);
        let summary: Vec<_> = payloads
            .iter()
            .map(|p| {
                (
                    p.hook_type(),
                    p.repository.as_str(),
                    p.issue_number,
                    p.fields["status"].as_str().unwrap(),
                    p.fields.get("previous_status").and_then(Value::as_str),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    "issues:edited".into(),
                    "o/r",
                    Some(2),
                    "In Progress",
                    Some("Todo")
                ),
                ("issues:edited".into(), "o/r", Some(3), "Todo", Some("")),
                ("issues:opened".into(), "o/x", Some(4), "", None),
            ]
        );
        assert_eq!(
            payloads[0].fields["project"],
            "https://github.com/users/o/projects/1"
        );
        assert!(diff("p", &next, &next).is_empty());
    }
}
