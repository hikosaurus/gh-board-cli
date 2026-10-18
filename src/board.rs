use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::process::Command;

const NO_STATUS: &str = "No Status";

const QUERY: &str = r#"
query($owner: String!, $repo: String!, $after: String) {
  repository(owner: $owner, name: $repo) {
    projectsV2(first: 1) {
      nodes {
        title
        owner { ... on User { login } ... on Organization { login } }
        field(name: "Status") {
          ... on ProjectV2SingleSelectField { options { name } }
        }
        items(first: 100, after: $after) {
          pageInfo { hasNextPage endCursor }
          nodes {
            fieldValueByName(name: "Status") {
              ... on ProjectV2ItemFieldSingleSelectValue { name }
            }
            content {
              ... on Issue { number title repository { nameWithOwner } }
            }
          }
        }
      }
    }
  }
}
"#;

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub issues: Vec<Issue>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Board {
    pub owner: String,
    pub title: String,
    pub columns: Vec<Column>,
}

/// Fetch the first GitHub Project linked to the repository via `gh api graphql`.
pub async fn fetch(repository: &str) -> Result<Board> {
    let (owner, repo) = repository
        .split_once('/')
        .context("repository must be <owner>/<repo>")?;

    let mut board: Option<Board> = None;
    let mut after: Option<String> = None;
    loop {
        let page = query(owner, repo, after.as_deref()).await?;
        let project = page
            .pointer("/data/repository/projectsV2/nodes/0")
            .context("no GitHub Project is linked to the repository")?;
        let board = board.get_or_insert_with(|| new_board(project));
        add_items(board, repository, project);

        let page_info = &project["items"]["pageInfo"];
        if page_info["hasNextPage"].as_bool() != Some(true) {
            break;
        }
        after = page_info["endCursor"].as_str().map(str::to_string);
    }
    let mut board = board.unwrap_or_default();
    board
        .columns
        .retain(|c| c.name != NO_STATUS || !c.issues.is_empty());
    Ok(board)
}

async fn query(owner: &str, repo: &str, after: Option<&str>) -> Result<Value> {
    let mut cmd = Command::new("gh");
    cmd.args(["api", "graphql"])
        .args(["-f", &format!("query={QUERY}")])
        .args(["-F", &format!("owner={owner}")])
        .args(["-F", &format!("repo={repo}")]);
    if let Some(after) = after {
        cmd.args(["-F", &format!("after={after}")]);
    }
    let output = cmd.output().await.context("failed to run gh")?;
    if !output.status.success() {
        bail!(
            "gh api graphql: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn new_board(project: &Value) -> Board {
    let mut columns: Vec<Column> = project["field"]["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o["name"].as_str())
        .map(|name| Column {
            name: name.to_string(),
            issues: vec![],
        })
        .collect();
    columns.push(Column {
        name: NO_STATUS.to_string(),
        issues: vec![],
    });
    Board {
        owner: project["owner"]["login"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        title: project["title"].as_str().unwrap_or_default().to_string(),
        columns,
    }
}

fn add_items(board: &mut Board, repository: &str, project: &Value) {
    let items = project["items"]["nodes"].as_array().into_iter().flatten();
    for item in items {
        let content = &item["content"];
        let (Some(number), Some(title)) = (content["number"].as_u64(), content["title"].as_str())
        else {
            continue; // not an issue (draft / pull request)
        };
        // Project may contain issues from other repositories
        if content["repository"]["nameWithOwner"]
            .as_str()
            .is_some_and(|r| !r.eq_ignore_ascii_case(repository))
        {
            continue;
        }
        let status = item["fieldValueByName"]["name"]
            .as_str()
            .unwrap_or(NO_STATUS);
        let column = match board.columns.iter().position(|c| c.name == status) {
            Some(i) => &mut board.columns[i],
            None => {
                board.columns.push(Column {
                    name: status.to_string(),
                    issues: vec![],
                });
                board.columns.last_mut().unwrap()
            }
        };
        column.issues.push(Issue {
            number,
            title: title.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_board_from_response() {
        let project = json!({
            "title": "proj",
            "owner": { "login": "owner" },
            "field": { "options": [{ "name": "Todo" }, { "name": "In Progress" }] },
            "items": { "nodes": [
                { "fieldValueByName": { "name": "In Progress" },
                  "content": { "number": 3, "title": "c", "repository": { "nameWithOwner": "owner/repo" } } },
                { "fieldValueByName": null,
                  "content": { "number": 4, "title": "d", "repository": { "nameWithOwner": "owner/repo" } } },
                { "fieldValueByName": { "name": "Todo" },
                  "content": { "number": 9, "title": "x", "repository": { "nameWithOwner": "other/repo" } } },
                { "fieldValueByName": { "name": "Todo" }, "content": {} }
            ] }
        });
        let mut board = new_board(&project);
        add_items(&mut board, "owner/repo", &project);
        assert_eq!(board.owner, "owner");
        assert_eq!(board.title, "proj");
        let summary: Vec<_> = board
            .columns
            .iter()
            .map(|c| {
                (
                    c.name.as_str(),
                    c.issues.iter().map(|i| i.number).collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("Todo", vec![]),
                ("In Progress", vec![3]),
                (NO_STATUS, vec![4])
            ]
        );
    }
}
