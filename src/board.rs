use anyhow::{Context, Result, bail};
use futures_util::future::try_join_all;
use serde_json::Value;
use tokio::process::Command;

const NO_STATUS: &str = "No Status";
const DONE: &str = "Done";
/// Max issues shown in the Done column (recently updated first)
const DONE_LIMIT: usize = 10;

const QUERY: &str = r#"
query($id: ID!, $after: String) {
  node(id: $id) {
    ... on ProjectV2 {
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
            ... on Issue { number title updatedAt repository { nameWithOwner } }
          }
        }
      }
    }
  }
}
"#;

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    /// `{owner}/{repo}`
    pub repository: String,
    pub number: u64,
    pub title: String,
    /// ISO 8601 (sortable as string)
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub issues: Vec<Issue>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Project {
    pub id: String,
    pub owner: String,
    pub title: String,
    pub columns: Vec<Column>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Board {
    pub projects: Vec<Project>,
}

/// Fetch open projects (`gh project list`) and their issues (`gh api graphql`).
pub async fn fetch() -> Result<Board> {
    let list = gh(&["project", "list", "--format", "json", "--limit", "100"]).await?;
    let projects = parse_projects(&list);
    let projects = try_join_all(projects.into_iter().map(fetch_items)).await?;
    Ok(Board { projects })
}

/// Open projects in `gh project list --format json`.
fn parse_projects(list: &Value) -> Vec<Project> {
    list["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["closed"].as_bool() == Some(false))
        .map(|p| Project {
            id: p["id"].as_str().unwrap_or_default().to_string(),
            owner: p["owner"]["login"].as_str().unwrap_or_default().to_string(),
            title: p["title"].as_str().unwrap_or_default().to_string(),
            columns: vec![],
        })
        .collect()
}

async fn fetch_items(mut project: Project) -> Result<Project> {
    let mut after: Option<String> = None;
    loop {
        let mut args = vec![
            "api".to_string(),
            "graphql".into(),
            "-f".into(),
            format!("query={QUERY}"),
            "-F".into(),
            format!("id={}", project.id),
        ];
        if let Some(after) = &after {
            args.extend(["-F".into(), format!("after={after}")]);
        }
        let page = gh(&args).await?;
        let node = &page["data"]["node"];
        if project.columns.is_empty() {
            project.columns = new_columns(node);
        }
        add_items(&mut project, node);

        let page_info = &node["items"]["pageInfo"];
        if page_info["hasNextPage"].as_bool() != Some(true) {
            break;
        }
        after = page_info["endCursor"].as_str().map(str::to_string);
    }
    project
        .columns
        .retain(|c| c.name != NO_STATUS || !c.issues.is_empty());
    limit_done(&mut project);
    Ok(project)
}

async fn gh<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Result<Value> {
    let output = Command::new("gh")
        .args(args)
        .output()
        .await
        .context("failed to run gh")?;
    if !output.status.success() {
        bail!("gh: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn new_columns(node: &Value) -> Vec<Column> {
    let mut columns: Vec<Column> = node["field"]["options"]
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
    columns
}

fn add_items(project: &mut Project, node: &Value) {
    let items = node["items"]["nodes"].as_array().into_iter().flatten();
    for item in items {
        let content = &item["content"];
        let (Some(number), Some(title)) = (content["number"].as_u64(), content["title"].as_str())
        else {
            continue; // not an issue (draft / pull request)
        };
        let status = item["fieldValueByName"]["name"]
            .as_str()
            .unwrap_or(NO_STATUS);
        let column = match project.columns.iter().position(|c| c.name == status) {
            Some(i) => &mut project.columns[i],
            None => {
                project.columns.push(Column {
                    name: status.to_string(),
                    issues: vec![],
                });
                project.columns.last_mut().unwrap()
            }
        };
        column.issues.push(Issue {
            repository: content["repository"]["nameWithOwner"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            number,
            title: title.to_string(),
            updated_at: content["updatedAt"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        });
    }
}

fn limit_done(project: &mut Project) {
    for column in project.columns.iter_mut().filter(|c| c.name == DONE) {
        column
            .issues
            .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        column.issues.truncate(DONE_LIMIT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_open_projects() {
        let list = json!({ "projects": [
            { "closed": false, "id": "P1", "title": "a", "owner": { "login": "me" } },
            { "closed": true, "id": "P2", "title": "b", "owner": { "login": "me" } },
            { "closed": false, "id": "P3", "title": "c", "owner": { "login": "org" } }
        ] });
        let projects: Vec<_> = parse_projects(&list)
            .into_iter()
            .map(|p| (p.id, p.owner, p.title))
            .collect();
        assert_eq!(
            projects,
            vec![
                ("P1".into(), "me".into(), "a".into()),
                ("P3".into(), "org".into(), "c".into())
            ]
        );
    }

    #[test]
    fn build_columns_from_response() {
        let node = json!({
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
        let mut project = Project {
            columns: new_columns(&node),
            ..Default::default()
        };
        add_items(&mut project, &node);
        let summary: Vec<_> = project
            .columns
            .iter()
            .map(|c| {
                (
                    c.name.as_str(),
                    c.issues
                        .iter()
                        .map(|i| (i.repository.as_str(), i.number))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("Todo", vec![("other/repo", 9)]),
                ("In Progress", vec![("owner/repo", 3)]),
                (NO_STATUS, vec![("owner/repo", 4)])
            ]
        );
    }

    #[test]
    fn limit_done_to_recent_issues() {
        let issues = (1..=12)
            .map(|n| Issue {
                repository: "o/r".into(),
                number: n,
                title: format!("i{n}"),
                updated_at: format!("2026-10-{n:02}T00:00:00Z"),
            })
            .collect::<Vec<_>>();
        let mut project = Project {
            columns: vec![
                Column {
                    name: "Todo".into(),
                    issues: issues.clone(),
                },
                Column {
                    name: DONE.into(),
                    issues,
                },
            ],
            ..Default::default()
        };
        limit_done(&mut project);
        assert_eq!(project.columns[0].issues.len(), 12);
        let done: Vec<_> = project.columns[1].issues.iter().map(|i| i.number).collect();
        assert_eq!(done, (3..=12).rev().collect::<Vec<_>>());
    }
}
