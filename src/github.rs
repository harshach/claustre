//! GitHub data access for issue/PR cache hydration and Sprint Board snapshots.
//!
//! Claustre prefers a GitHub App user token when one is available and falls
//! back to the local `gh` CLI token otherwise. The Sprint Board is sourced from
//! GitHub Projects v2 via GraphQL, while some legacy REST helpers remain for
//! issue, pull request, and milestone compatibility paths.

use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use crate::{
    config, github_app,
    store::{GitHubItemKind, GitHubProjectV2Cache, Store},
};

/// Matches `Closes #123`, `Fixes #456`, `Resolves #789` (case-insensitive).
static CLOSING_REF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s+#(\d+)")
        .expect("closing ref regex is valid")
});

const GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_API_VERSION: &str = "2022-11-28";
const PROJECTS_V2_DISCOVERY_QUERY: &str = r"
query($owner: String!, $after: String) {
  organization(login: $owner) {
    projectsV2(first: 50, after: $after, orderBy: { field: UPDATED_AT, direction: DESC }) {
      nodes {
        number
        title
        url
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
  user(login: $owner) {
    projectsV2(first: 50, after: $after, orderBy: { field: UPDATED_AT, direction: DESC }) {
      nodes {
        number
        title
        url
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
";
const PROJECT_V2_ITEMS_QUERY: &str = r"
query($owner: String!, $number: Int!, $after: String) {
  organization(login: $owner) {
    projectV2(number: $number) {
      id
      title
      url
      items(first: 100, after: $after) {
        pageInfo {
          hasNextPage
          endCursor
        }
        nodes {
          fieldValues(first: 20) {
            nodes {
              __typename
              ... on ProjectV2ItemFieldTextValue {
                text
                field {
                  ... on ProjectV2FieldCommon {
                    name
                  }
                }
              }
              ... on ProjectV2ItemFieldDateValue {
                date
                field {
                  ... on ProjectV2FieldCommon {
                    name
                  }
                }
              }
              ... on ProjectV2ItemFieldNumberValue {
                number
                field {
                  ... on ProjectV2FieldCommon {
                    name
                  }
                }
              }
              ... on ProjectV2ItemFieldSingleSelectValue {
                name
                field {
                  ... on ProjectV2FieldCommon {
                    name
                  }
                }
              }
              ... on ProjectV2ItemFieldIterationValue {
                title
                field {
                  ... on ProjectV2FieldCommon {
                    name
                  }
                }
              }
            }
          }
          content {
            __typename
            ... on Issue {
              id
              number
              title
              body
              bodyText
              bodyHTML
              state
              url
              updatedAt
              assignees(first: 20) {
                nodes {
                  login
                }
              }
              labels(first: 50) {
                nodes {
                  name
                  color
                }
              }
              author {
                login
              }
            }
            ... on PullRequest {
              id
              number
              title
              body
              bodyText
              bodyHTML
              state
              url
              updatedAt
              isDraft
              baseRefName
              headRefName
              reviewDecision
              mergeStateStatus
              closingIssuesReferences(first: 10) {
                nodes {
                  number
                }
              }
              assignees(first: 20) {
                nodes {
                  login
                }
              }
              labels(first: 50) {
                nodes {
                  name
                  color
                }
              }
              author {
                login
              }
            }
          }
        }
      }
    }
  }
  user(login: $owner) {
    projectV2(number: $number) {
      id
      title
      url
      items(first: 100, after: $after) {
        pageInfo {
          hasNextPage
          endCursor
        }
        nodes {
          fieldValues(first: 20) {
            nodes {
              __typename
              ... on ProjectV2ItemFieldTextValue {
                text
                field {
                  ... on ProjectV2FieldCommon {
                    name
                  }
                }
              }
              ... on ProjectV2ItemFieldDateValue {
                date
                field {
                  ... on ProjectV2FieldCommon {
                    name
                  }
                }
              }
              ... on ProjectV2ItemFieldNumberValue {
                number
                field {
                  ... on ProjectV2FieldCommon {
                    name
                  }
                }
              }
              ... on ProjectV2ItemFieldSingleSelectValue {
                name
                field {
                  ... on ProjectV2FieldCommon {
                    name
                  }
                }
              }
              ... on ProjectV2ItemFieldIterationValue {
                title
                field {
                  ... on ProjectV2FieldCommon {
                    name
                  }
                }
              }
            }
          }
          content {
            __typename
            ... on Issue {
              id
              number
              title
              body
              bodyText
              bodyHTML
              state
              url
              updatedAt
              assignees(first: 20) {
                nodes {
                  login
                }
              }
              labels(first: 50) {
                nodes {
                  name
                  color
                }
              }
              author {
                login
              }
            }
            ... on PullRequest {
              id
              number
              title
              body
              bodyText
              bodyHTML
              state
              url
              updatedAt
              isDraft
              baseRefName
              headRefName
              reviewDecision
              mergeStateStatus
              closingIssuesReferences(first: 10) {
                nodes {
                  number
                }
              }
              assignees(first: 20) {
                nodes {
                  login
                }
              }
              labels(first: 50) {
                nodes {
                  name
                  color
                }
              }
              author {
                login
              }
            }
          }
        }
      }
    }
  }
}
";

const PROJECT_V2_FIELDS_QUERY: &str = r"
query($projectId: ID!) {
  node(id: $projectId) {
    ... on ProjectV2 {
      fields(first: 50) {
        nodes {
          ... on ProjectV2FieldCommon {
            id
            name
            dataType
          }
          ... on ProjectV2SingleSelectField {
            id
            name
            dataType
            options {
              id
              name
            }
          }
          ... on ProjectV2IterationField {
            id
            name
            dataType
            configuration {
              iterations {
                id
                title
                startDate
              }
            }
          }
        }
      }
    }
  }
}
";

const UPDATE_PROJECT_V2_ITEM_FIELD_MUTATION: &str = r"
mutation($projectId: ID!, $itemId: ID!, $fieldId: ID!, $value: ProjectV2FieldValue!) {
  updateProjectV2ItemFieldValue(input: {
    projectId: $projectId
    itemId: $itemId
    fieldId: $fieldId
    value: $value
  }) {
    projectV2Item {
      id
    }
  }
}
";

/// A GitHub issue label.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubLabel {
    pub name: String,
    pub color: Option<String>,
}

/// A GitHub milestone (used as sprint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubMilestone {
    pub number: i64,
    pub title: String,
    pub state: String,
    #[serde(rename = "dueOn")]
    pub due_on: Option<String>,
}

/// A GitHub user (assignee).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubUser {
    pub login: String,
}

/// A GitHub issue fetched via `gh`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubIssue {
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub url: String,
    pub labels: Vec<GitHubLabel>,
    pub assignees: Vec<GitHubUser>,
    pub milestone: Option<GitHubMilestone>,
    #[serde(rename = "createdAt")]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRepoView {
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub url: Option<String>,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubBoardItem {
    pub github_item_id: String,
    pub number: i64,
    pub kind: GitHubItemKind,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub url: String,
    pub labels: Vec<GitHubLabel>,
    pub assignees: Vec<GitHubUser>,
    pub project_field_values: serde_json::Value,
    pub linked_pr_number: Option<i64>,
    pub linked_pr_item_id: Option<String>,
}

/// A field definition from a GitHub Projects v2 board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectV2FieldDef {
    pub id: String,
    pub name: String,
    pub data_type: String,
    pub options: Vec<ProjectV2FieldOption>,
}

/// An option value for a single-select or iteration field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectV2FieldOption {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct GitHubSyncSummary {
    pub repo_full_name: String,
    pub issues_synced: usize,
    pub prs_synced: usize,
    pub projects_synced: usize,
    pub project_items_synced: usize,
    pub comments_synced: usize,
    pub reviews_synced: usize,
    pub review_comments_synced: usize,
}

#[derive(Debug, Clone)]
pub struct GitHubProjectBoardSnapshot {
    pub projects: Vec<GitHubProjectV2Cache>,
    pub selected_project: Option<GitHubProjectV2Cache>,
    pub items: Vec<GitHubBoardItem>,
}

pub fn sync_project_board(
    store: &Store,
    project: &crate::store::Project,
    selected_project_hint: Option<&str>,
) -> Result<GitHubProjectBoardSnapshot> {
    let repo = repo_view_with_preferred_auth(&project.repo_path)?;
    let cached_repo = store.upsert_github_repo(
        Some(&project.id),
        &repo.owner,
        &repo.name,
        repo.url.as_deref(),
        repo.default_branch.as_deref(),
    )?;

    let discovered_projects = sync_projects_v2_for_repo(store, &repo, &cached_repo.id)?;
    let selected_project =
        resolve_selected_project(&discovered_projects, selected_project_hint).cloned();

    let items = if let Some(selected_project) = &selected_project {
        sync_project_v2_items(store, &repo, selected_project, &cached_repo.id)?;
        link_prs_to_issues(store, &cached_repo.id)?;
        load_board_items(store, selected_project)?
    } else {
        Vec::new()
    };

    Ok(GitHubProjectBoardSnapshot {
        projects: discovered_projects,
        selected_project,
        items,
    })
}

pub fn load_project_board_from_cache(
    store: &Store,
    project: &crate::store::Project,
    selected_project_hint: Option<&str>,
) -> Result<GitHubProjectBoardSnapshot> {
    let cached_repo = store
        .get_github_repo_for_project(&project.id)?
        .ok_or_else(|| anyhow::anyhow!("Sync GitHub for this repository first."))?;

    let discovered_projects = store.list_github_projects_v2_for_repo(&cached_repo.id)?;
    let selected_project =
        resolve_selected_project(&discovered_projects, selected_project_hint).cloned();

    let items = if let Some(selected_project) = &selected_project {
        load_board_items(store, selected_project)?
    } else {
        Vec::new()
    };

    Ok(GitHubProjectBoardSnapshot {
        projects: discovered_projects,
        selected_project,
        items,
    })
}

/// Fetch field definitions for a GitHub Projects v2 board.
pub fn fetch_project_v2_fields(project_node_id: &str) -> Result<Vec<ProjectV2FieldDef>> {
    let token =
        preferred_github_token()?.ok_or_else(|| anyhow::anyhow!("no GitHub token available"))?;

    let data = github_graphql_with_token(
        &token,
        PROJECT_V2_FIELDS_QUERY,
        &serde_json::json!({ "projectId": project_node_id }),
    )
    .context("failed to fetch project v2 field definitions")?;

    let nodes = data
        .get("node")
        .and_then(|node| node.get("fields"))
        .and_then(|fields| fields.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut fields = Vec::new();
    for node in nodes {
        let Some(id) = node.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(name) = node.get("name").and_then(Value::as_str) else {
            continue;
        };
        let data_type = node
            .get("dataType")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let mut options = Vec::new();

        // Single-select options
        if let Some(opts) = node.get("options").and_then(Value::as_array) {
            for opt in opts {
                if let (Some(opt_id), Some(opt_name)) = (
                    opt.get("id").and_then(Value::as_str),
                    opt.get("name").and_then(Value::as_str),
                ) {
                    options.push(ProjectV2FieldOption {
                        id: opt_id.to_string(),
                        name: opt_name.to_string(),
                    });
                }
            }
        }

        // Iteration options (from configuration.iterations)
        if let Some(iterations) = node
            .get("configuration")
            .and_then(|cfg| cfg.get("iterations"))
            .and_then(Value::as_array)
        {
            for iter in iterations {
                if let (Some(iter_id), Some(iter_title)) = (
                    iter.get("id").and_then(Value::as_str),
                    iter.get("title").and_then(Value::as_str),
                ) {
                    options.push(ProjectV2FieldOption {
                        id: iter_id.to_string(),
                        name: iter_title.to_string(),
                    });
                }
            }
        }

        fields.push(ProjectV2FieldDef {
            id: id.to_string(),
            name: name.to_string(),
            data_type,
            options,
        });
    }

    Ok(fields)
}

/// Update a project field value on a project item.
pub fn update_project_v2_item_field(
    project_node_id: &str,
    item_node_id: &str,
    field_id: &str,
    value: &serde_json::Value,
) -> Result<()> {
    let token =
        preferred_github_token()?.ok_or_else(|| anyhow::anyhow!("no GitHub token available"))?;

    github_graphql_with_token(
        &token,
        UPDATE_PROJECT_V2_ITEM_FIELD_MUTATION,
        &serde_json::json!({
            "projectId": project_node_id,
            "itemId": item_node_id,
            "fieldId": field_id,
            "value": value,
        }),
    )
    .context("failed to update project v2 item field value")?;

    Ok(())
}

/// Fetch open issues (and optionally recently closed) from a git repository.
/// Uses `gh issue list --json ...` from the repo directory.
/// If `milestone` is `Some`, filters to that milestone title.
pub fn fetch_issues(repo_path: &str, milestone: Option<&str>) -> Result<Vec<GitHubIssue>> {
    if let Some(token) = preferred_github_app_token()? {
        return fetch_issues_with_token(repo_path, milestone, &token);
    }

    let fields = "number,title,body,state,url,labels,assignees,milestone,createdAt";
    let mut args = vec![
        "issue",
        "list",
        "--json",
        fields,
        "--limit",
        "500",
        "--state",
        "all",
        "--assignee",
        "@me",
    ];

    if let Some(ms) = milestone {
        args.extend(["--milestone", ms]);
    }

    let output = Command::new("gh")
        .args(&args)
        .current_dir(repo_path)
        .output()
        .context("failed to run `gh issue list` — is `gh` installed and authenticated?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gh issue list failed: {stderr}");
    }

    let issues: Vec<GitHubIssue> =
        serde_json::from_slice(&output.stdout).context("failed to parse gh issue list output")?;

    Ok(issues)
}

/// Fetch milestones from a git repository.
/// Returns open milestones sorted by due date (closest first).
pub fn fetch_milestones(repo_path: &str) -> Result<Vec<GitHubMilestone>> {
    if let Some(token) = preferred_github_app_token()? {
        return fetch_milestones_with_token(repo_path, &token);
    }

    let output = Command::new("gh")
        .args([
            "api",
            "repos/{owner}/{repo}/milestones",
            "--jq",
            ".",
            "--paginate",
        ])
        .current_dir(repo_path)
        .output()
        .context("failed to run `gh api` for milestones")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gh api milestones failed: {stderr}");
    }

    let mut milestones: Vec<GitHubMilestone> =
        serde_json::from_slice(&output.stdout).context("failed to parse milestones response")?;

    // Sort: open milestones first, then by due date (nearest first).
    milestones.sort_by(|a, b| {
        let a_open = a.state == "open";
        let b_open = b.state == "open";
        b_open.cmp(&a_open).then_with(|| {
            a.due_on
                .as_deref()
                .unwrap_or("9999")
                .cmp(b.due_on.as_deref().unwrap_or("9999"))
        })
    });

    Ok(milestones)
}

/// Get the "current" milestone -- the first open milestone with the nearest due date.
pub fn current_milestone(milestones: &[GitHubMilestone]) -> Option<&GitHubMilestone> {
    milestones.iter().find(|m| m.state == "open")
}

/// Assign an issue to a board column based on its labels.
///
/// Returns the column index (0-based) matching the first label found,
/// or the default column (0 = first column) if no labels match.
/// Closed issues always go to the last column.
pub fn assign_column(issue: &GitHubIssue, column_labels: &[(String, Vec<String>)]) -> usize {
    // Closed issues go to last column (typically "Done").
    if issue.state == "CLOSED" || issue.state == "closed" {
        return column_labels.len().saturating_sub(1);
    }

    let issue_label_names: Vec<String> =
        issue.labels.iter().map(|l| l.name.to_lowercase()).collect();

    for (col_idx, (_name, labels)) in column_labels.iter().enumerate() {
        for label in labels {
            if issue_label_names.contains(&label.to_lowercase()) {
                return col_idx;
            }
        }
    }

    // Default: first column (backlog).
    0
}

pub fn auth_status() -> Result<String> {
    let output = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .context("failed to run `gh auth status`")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        Ok(if stdout.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        })
    } else {
        anyhow::bail!(
            "{}",
            if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            }
        );
    }
}

pub fn repo_view(repo_path: &str) -> Result<GitHubRepoView> {
    if let Some(token) = preferred_github_app_token()? {
        return repo_view_with_token(repo_path, &token);
    }

    let output = Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "name,nameWithOwner,url,defaultBranchRef",
        ])
        .current_dir(repo_path)
        .output()
        .context("failed to run `gh repo view`")?;

    if !output.status.success() {
        anyhow::bail!(
            "gh repo view failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let value: Value =
        serde_json::from_slice(&output.stdout).context("failed to parse gh repo view output")?;
    let full_name = string_field(&value, "nameWithOwner")
        .context("gh repo view did not include nameWithOwner")?;
    let (owner, name) = full_name
        .split_once('/')
        .with_context(|| format!("invalid nameWithOwner from GitHub: {full_name}"))?;

    Ok(GitHubRepoView {
        owner: owner.to_string(),
        name: name.to_string(),
        full_name: full_name.to_string(),
        url: optional_string_field(&value, "url"),
        default_branch: value
            .get("defaultBranchRef")
            .and_then(|branch| branch.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

pub fn sync_project_repo(
    store: &Store,
    project: &crate::store::Project,
) -> Result<GitHubSyncSummary> {
    let config = config::load().unwrap_or_default();
    let selected_project_hint = config.github_app.default_project_id;
    if let Some(token) = preferred_github_app_token()? {
        return sync_project_repo_with_token(
            store,
            project,
            &token,
            selected_project_hint.as_deref(),
        );
    }

    let repo = repo_view(&project.repo_path)?;
    let cached_repo = store.upsert_github_repo(
        Some(&project.id),
        &repo.owner,
        &repo.name,
        repo.url.as_deref(),
        repo.default_branch.as_deref(),
    )?;

    let mut summary = GitHubSyncSummary {
        repo_full_name: repo.full_name.clone(),
        ..GitHubSyncSummary::default()
    };

    let discovered_projects = sync_projects_v2_for_repo(store, &repo, &cached_repo.id)?;
    summary.projects_synced = discovered_projects.len();
    if let Some(selected_project) =
        resolve_selected_project(&discovered_projects, selected_project_hint.as_deref())
    {
        summary.project_items_synced =
            sync_project_v2_items(store, &repo, selected_project, &cached_repo.id)?;
    }

    for issue in gh_api_list(
        &project.repo_path,
        &format!("repos/{}/issues?state=all&per_page=100", repo.full_name),
    )? {
        if issue.get("pull_request").is_some() {
            continue;
        }
        let item = store.upsert_github_item(
            &cached_repo.id,
            None,
            optional_string_field(&issue, "node_id").as_deref(),
            number_field(&issue, "number").context("issue missing number")?,
            GitHubItemKind::Issue,
            string_field(&issue, "title").context("issue missing title")?,
            optional_string_field(&issue, "state")
                .as_deref()
                .unwrap_or("unknown"),
            string_field(&issue, "html_url").context("issue missing html_url")?,
            optional_string_field(&issue, "body_text").as_deref(),
            &logins_array(&issue, "assignees"),
            &label_names(&issue),
            &serde_json::json!({}),
            &serde_json::json!({}),
            None,
            None,
            optional_string_field(&issue, "updated_at").as_deref(),
        )?;
        store.upsert_github_issue_cache(
            &item.id,
            optional_string_field(&issue, "body").as_deref(),
            optional_string_field(&issue, "body_text").as_deref(),
            optional_string_field(&issue, "body_html").as_deref(),
            issue
                .get("user")
                .and_then(|user| user.get("login"))
                .and_then(Value::as_str),
            issue
                .get("milestone")
                .and_then(|milestone| milestone.get("title"))
                .and_then(Value::as_str),
            Some(&serde_json::to_string(&issue)?),
        )?;
        summary.issues_synced += 1;

        for comment in gh_api_list(
            &project.repo_path,
            &format!(
                "repos/{}/issues/{}/comments?per_page=100",
                repo.full_name, item.number
            ),
        )? {
            store.upsert_github_comment_cache(
                &item.id,
                &id_string(&comment),
                comment
                    .get("user")
                    .and_then(|user| user.get("login"))
                    .and_then(Value::as_str),
                optional_string_field(&comment, "body").as_deref(),
                optional_string_field(&comment, "body_text").as_deref(),
                optional_string_field(&comment, "body_html").as_deref(),
                string_field(&comment, "created_at").context("comment missing created_at")?,
                optional_string_field(&comment, "updated_at").as_deref(),
                Some(&serde_json::to_string(&comment)?),
            )?;
            summary.comments_synced += 1;
        }
    }

    for pull in gh_api_list(
        &project.repo_path,
        &format!("repos/{}/pulls?state=open&per_page=100", repo.full_name),
    )? {
        let number = number_field(&pull, "number").context("pull request missing number")?;
        let item = store.upsert_github_item(
            &cached_repo.id,
            None,
            optional_string_field(&pull, "node_id").as_deref(),
            number,
            GitHubItemKind::PullRequest,
            string_field(&pull, "title").context("pull request missing title")?,
            optional_string_field(&pull, "state")
                .as_deref()
                .unwrap_or("unknown"),
            string_field(&pull, "html_url").context("pull request missing html_url")?,
            optional_string_field(&pull, "body_text").as_deref(),
            &logins_array(&pull, "assignees"),
            &label_names(&pull),
            &serde_json::json!({}),
            &serde_json::json!({}),
            pull.get("base")
                .and_then(|base| base.get("ref"))
                .and_then(Value::as_str),
            pull.get("head")
                .and_then(|head| head.get("ref"))
                .and_then(Value::as_str),
            optional_string_field(&pull, "updated_at").as_deref(),
        )?;
        store.upsert_github_pr_cache(
            &item.id,
            optional_string_field(&pull, "body").as_deref(),
            optional_string_field(&pull, "body_text").as_deref(),
            optional_string_field(&pull, "body_html").as_deref(),
            pull.get("base")
                .and_then(|base| base.get("ref"))
                .and_then(Value::as_str),
            pull.get("head")
                .and_then(|head| head.get("ref"))
                .and_then(Value::as_str),
            optional_string_field(&pull, "merge_state_status").as_deref(),
            optional_string_field(&pull, "review_decision").as_deref(),
            pull.get("draft").and_then(Value::as_bool).unwrap_or(false),
            Some(&serde_json::to_string(&pull)?),
        )?;
        summary.prs_synced += 1;

        for review in gh_api_list(
            &project.repo_path,
            &format!(
                "repos/{}/pulls/{number}/reviews?per_page=100",
                repo.full_name
            ),
        )? {
            let cached_review = store.upsert_github_review_cache(
                &item.id,
                &id_string(&review),
                optional_string_field(&review, "state")
                    .as_deref()
                    .unwrap_or("COMMENTED"),
                optional_string_field(&review, "commit_id").as_deref(),
                review
                    .get("user")
                    .and_then(|user| user.get("login"))
                    .and_then(Value::as_str),
                optional_string_field(&review, "body").as_deref(),
                optional_string_field(&review, "body_text").as_deref(),
                optional_string_field(&review, "body_html").as_deref(),
                optional_string_field(&review, "submitted_at").as_deref(),
                Some(&serde_json::to_string(&review)?),
            )?;
            summary.reviews_synced += 1;

            for review_comment in gh_api_list(
                &project.repo_path,
                &format!(
                    "repos/{}/pulls/{number}/comments?per_page=100",
                    repo.full_name
                ),
            )? {
                let review_id = review_comment
                    .get("pull_request_review_id")
                    .and_then(Value::as_i64)
                    .map(|value| value.to_string());
                let review_matches =
                    review_id.as_deref() == Some(cached_review.github_review_id.as_str());
                if !review_matches {
                    continue;
                }
                store.upsert_github_review_comment_cache(
                    Some(&cached_review.id),
                    &item.id,
                    &id_string(&review_comment),
                    review_comment
                        .get("user")
                        .and_then(|user| user.get("login"))
                        .and_then(Value::as_str),
                    optional_string_field(&review_comment, "path").as_deref(),
                    review_comment.get("line").and_then(Value::as_i64),
                    optional_string_field(&review_comment, "side").as_deref(),
                    review_comment.get("start_line").and_then(Value::as_i64),
                    optional_string_field(&review_comment, "diff_hunk").as_deref(),
                    review_comment
                        .get("in_reply_to_id")
                        .and_then(Value::as_i64)
                        .map(|value| value.to_string())
                        .as_deref(),
                    optional_string_field(&review_comment, "body").as_deref(),
                    optional_string_field(&review_comment, "body_text").as_deref(),
                    optional_string_field(&review_comment, "body_html").as_deref(),
                    string_field(&review_comment, "created_at")
                        .context("review comment missing created_at")?,
                    optional_string_field(&review_comment, "updated_at").as_deref(),
                    Some(&serde_json::to_string(&review_comment)?),
                )?;
                summary.review_comments_synced += 1;
            }
        }
    }

    link_prs_to_issues(store, &cached_repo.id)?;

    Ok(summary)
}

fn repo_view_with_preferred_auth(repo_path: &str) -> Result<GitHubRepoView> {
    if let Some(token) = preferred_github_token()? {
        repo_view_with_token(repo_path, &token)
    } else {
        repo_view(repo_path)
    }
}

fn preferred_github_app_token() -> Result<Option<String>> {
    let config = config::load().unwrap_or_default();
    github_app::ensure_access_token(&config.github_app)
}

fn preferred_github_token() -> Result<Option<String>> {
    if let Some(token) = preferred_github_app_token()? {
        return Ok(Some(token));
    }
    github_app::github_cli_token()
}

fn resolve_selected_project<'a>(
    projects: &'a [GitHubProjectV2Cache],
    hint: Option<&str>,
) -> Option<&'a GitHubProjectV2Cache> {
    let normalized_hint = hint.map(str::trim).filter(|value| !value.is_empty());

    normalized_hint
        .and_then(|hint| {
            projects.iter().find(|project| {
                project.id == hint
                    || project.project_number.to_string() == hint
                    || project.title.eq_ignore_ascii_case(hint)
            })
        })
        .or_else(|| projects.first())
}

fn sync_projects_v2_for_repo(
    store: &Store,
    repo: &GitHubRepoView,
    repo_id: &str,
) -> Result<Vec<GitHubProjectV2Cache>> {
    let Some(token) = preferred_github_token()? else {
        return Ok(Vec::new());
    };

    let owner_login = &repo.owner;
    let mut after: Option<String> = None;
    let mut synced = Vec::new();

    loop {
        let data = github_graphql_with_token(
            &token,
            PROJECTS_V2_DISCOVERY_QUERY,
            &serde_json::json!({
                "owner": owner_login,
                "after": after,
            }),
        )
        .map_err(map_projects_v2_error)
        .with_context(|| {
            format!(
                "failed to load GitHub Projects v2 for {} (ensure the token has project scope)",
                repo.full_name
            )
        })?;

        let project_connection = owner_projects_connection(&data).ok_or_else(|| {
            anyhow::anyhow!("GitHub Projects v2 are not available for this owner")
        })?;
        let nodes = project_connection
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        for project_value in nodes {
            let project_number =
                number_field(&project_value, "number").context("project missing number")?;
            let title = string_field(&project_value, "title").context("project missing title")?;
            let url = optional_string_field(&project_value, "url");
            let cached = store.upsert_github_project_v2(
                repo_id,
                project_number,
                title,
                url.as_deref(),
                None,
            )?;
            synced.push(cached);
        }

        let has_next_page = project_connection
            .get("pageInfo")
            .and_then(|page| page.get("hasNextPage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !has_next_page {
            break;
        }
        after = project_connection
            .get("pageInfo")
            .and_then(|page| page.get("endCursor"))
            .and_then(Value::as_str)
            .map(str::to_string);
    }

    synced.sort_by(|left, right| {
        left.title
            .cmp(&right.title)
            .then(left.project_number.cmp(&right.project_number))
    });
    synced.dedup_by(|left, right| left.id == right.id);

    Ok(if synced.is_empty() {
        store
            .get_github_repo_by_full_name(&repo.full_name)?
            .map(|cached_repo| {
                store
                    .list_github_projects_v2_for_repo(&cached_repo.id)
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    } else {
        synced
    })
}

fn sync_project_v2_items(
    store: &Store,
    repo: &GitHubRepoView,
    project: &GitHubProjectV2Cache,
    repo_id: &str,
) -> Result<usize> {
    let Some(token) = preferred_github_token()? else {
        return Ok(0);
    };

    store.clear_github_project_assignments(&project.id)?;

    let mut after: Option<String> = None;
    let mut synced_count = 0usize;
    let mut node_id_persisted = false;

    loop {
        let data = github_graphql_with_token(
            &token,
            PROJECT_V2_ITEMS_QUERY,
            &serde_json::json!({
                "owner": repo.owner,
                "number": project.project_number,
                "after": after,
            }),
        )
        .map_err(map_projects_v2_error)
        .with_context(|| {
            format!(
                "failed to load project items for {} #{}",
                repo.owner, project.project_number
            )
        })?;

        let project_value = owner_project_v2_value(&data).ok_or_else(|| {
            anyhow::anyhow!("GitHub project #{} not found", project.project_number)
        })?;

        // Persist the project's GraphQL node_id on the first page.
        if !node_id_persisted {
            if let Some(project_node_id) = project_value.get("id").and_then(Value::as_str) {
                store.upsert_github_project_v2(
                    repo_id,
                    project.project_number,
                    &project.title,
                    project.url.as_deref(),
                    Some(project_node_id),
                )?;
            }
            node_id_persisted = true;
        }

        let items_connection = project_value
            .get("items")
            .ok_or_else(|| anyhow::anyhow!("project response missing items"))?;
        let nodes = items_connection
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        for item_value in nodes {
            let Some(content) = item_value.get("content") else {
                continue;
            };
            let typename = string_field(content, "__typename").unwrap_or_default();
            let Some((kind, body, body_text, body_html, base_ref, head_ref)) = (match typename {
                "Issue" => Some((
                    GitHubItemKind::Issue,
                    optional_string_field(content, "body"),
                    optional_string_field(content, "bodyText"),
                    optional_string_field(content, "bodyHTML"),
                    None,
                    None,
                )),
                "PullRequest" => Some((
                    GitHubItemKind::PullRequest,
                    optional_string_field(content, "body"),
                    optional_string_field(content, "bodyText"),
                    optional_string_field(content, "bodyHTML"),
                    optional_string_field(content, "baseRefName"),
                    optional_string_field(content, "headRefName"),
                )),
                _ => None,
            }) else {
                continue;
            };

            let number = number_field(content, "number").context("project item missing number")?;
            let title = string_field(content, "title").context("project item missing title")?;
            let state =
                optional_string_field(content, "state").unwrap_or_else(|| "OPEN".to_string());
            let url = string_field(content, "url").context("project item missing url")?;
            let field_values = extract_project_field_values(&item_value);
            let label_names = nested_name_list(content, "labels");
            let label_colors = nested_label_colors(content, "labels");
            let assignee_logins = nested_login_list(content, "assignees");

            let cached_item = store.upsert_github_item(
                repo_id,
                Some(&project.id),
                optional_string_field(content, "id").as_deref(),
                number,
                kind,
                title,
                &state,
                url,
                body_text.as_deref(),
                &assignee_logins,
                &label_names,
                &label_colors,
                &field_values,
                base_ref.as_deref(),
                head_ref.as_deref(),
                optional_string_field(content, "updatedAt").as_deref(),
            )?;

            match kind {
                GitHubItemKind::Issue => {
                    store.upsert_github_issue_cache(
                        &cached_item.id,
                        body.as_deref(),
                        body_text.as_deref(),
                        body_html.as_deref(),
                        content
                            .get("author")
                            .and_then(|author| author.get("login"))
                            .and_then(Value::as_str),
                        field_values
                            .get("Sprint")
                            .and_then(Value::as_str)
                            .or_else(|| field_values.get("Iteration").and_then(Value::as_str))
                            .or_else(|| field_values.get("Milestone").and_then(Value::as_str)),
                        Some(&serde_json::to_string(content)?),
                    )?;
                }
                GitHubItemKind::PullRequest => {
                    let merge_state_status = optional_string_field(content, "mergeStateStatus");
                    let review_decision = optional_string_field(content, "reviewDecision");
                    let is_draft = content
                        .get("isDraft")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    store.upsert_github_pr_cache(
                        &cached_item.id,
                        body.as_deref(),
                        body_text.as_deref(),
                        body_html.as_deref(),
                        base_ref.as_deref(),
                        head_ref.as_deref(),
                        merge_state_status.as_deref(),
                        review_decision.as_deref(),
                        is_draft,
                        Some(&serde_json::to_string(content)?),
                    )?;
                }
                GitHubItemKind::ProjectItem => {}
            }

            synced_count += 1;
        }

        let has_next_page = items_connection
            .get("pageInfo")
            .and_then(|page| page.get("hasNextPage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !has_next_page {
            break;
        }
        after = items_connection
            .get("pageInfo")
            .and_then(|page| page.get("endCursor"))
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    Ok(synced_count)
}

fn load_board_items(store: &Store, project: &GitHubProjectV2Cache) -> Result<Vec<GitHubBoardItem>> {
    let mut board_items = Vec::new();
    for item in store.list_github_items_for_project_v2(&project.id)? {
        let body = match item.kind {
            GitHubItemKind::Issue => store
                .get_github_issue_cache(&item.id)
                .ok()
                .and_then(|cache| cache.body.or(cache.body_text)),
            GitHubItemKind::PullRequest => store
                .get_github_pr_cache(&item.id)
                .ok()
                .and_then(|cache| cache.body.or(cache.body_text)),
            GitHubItemKind::ProjectItem => None,
        };

        board_items.push(GitHubBoardItem {
            github_item_id: item.id,
            number: item.number,
            kind: item.kind,
            title: item.title,
            body,
            state: item.state,
            url: item.url,
            labels: item
                .label_names
                .into_iter()
                .map(|name| {
                    let color = item
                        .label_colors
                        .get(&name)
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    GitHubLabel { name, color }
                })
                .collect(),
            assignees: item
                .assignee_logins
                .into_iter()
                .map(|login| GitHubUser { login })
                .collect(),
            project_field_values: item.project_field_values,
            linked_pr_number: item.linked_pr_number,
            linked_pr_item_id: item.linked_pr_item_id,
        });
    }

    board_items.sort_by(|left, right| left.number.cmp(&right.number));
    Ok(board_items)
}

fn nested_login_list(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|value| value.get("nodes"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("login")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn nested_name_list(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|value| value.get("nodes"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn nested_label_colors(value: &Value, key: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    if let Some(nodes) = value
        .get(key)
        .and_then(|v| v.get("nodes"))
        .and_then(Value::as_array)
    {
        for node in nodes {
            if let (Some(name), Some(color)) = (
                node.get("name").and_then(Value::as_str),
                node.get("color").and_then(Value::as_str),
            ) {
                map.insert(name.to_string(), Value::String(color.to_string()));
            }
        }
    }
    Value::Object(map)
}

fn extract_project_field_values(item_value: &Value) -> serde_json::Value {
    let mut field_values = serde_json::Map::new();
    let nodes = item_value
        .get("fieldValues")
        .and_then(|field_values| field_values.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for node in nodes {
        let Some(field_name) = node
            .get("field")
            .and_then(|field| field.get("name"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let value = match string_field(&node, "__typename").unwrap_or_default() {
            "ProjectV2ItemFieldSingleSelectValue" => optional_string_field(&node, "name"),
            "ProjectV2ItemFieldIterationValue" => optional_string_field(&node, "title"),
            "ProjectV2ItemFieldTextValue" => optional_string_field(&node, "text"),
            "ProjectV2ItemFieldDateValue" => optional_string_field(&node, "date"),
            "ProjectV2ItemFieldNumberValue" => node
                .get("number")
                .and_then(Value::as_f64)
                .map(|value| format!("{value}")),
            _ => None,
        };
        if let Some(value) = value {
            field_values.insert(field_name.to_string(), Value::String(value));
        }
    }

    Value::Object(field_values)
}

fn owner_projects_connection(data: &Value) -> Option<&Value> {
    data.get("organization")
        .and_then(|value| value.get("projectsV2"))
        .or_else(|| data.get("user").and_then(|value| value.get("projectsV2")))
}

fn owner_project_v2_value(data: &Value) -> Option<&Value> {
    data.get("organization")
        .and_then(|value| value.get("projectV2"))
        .or_else(|| data.get("user").and_then(|value| value.get("projectV2")))
}

fn github_graphql_with_token(token: &str, query: &str, variables: &Value) -> Result<Value> {
    let response = ureq::post(&format!("{GITHUB_API_BASE}/graphql"))
        .set("Accept", "application/vnd.github+json")
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "claustre")
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .send_json(serde_json::json!({
            "query": query,
            "variables": variables,
        }))
        .map_err(http_error)?;

    let value: Value = response
        .into_json()
        .context("failed to parse GitHub GraphQL response")?;

    parse_graphql_response(&value)
}

fn parse_graphql_response(value: &Value) -> Result<Value> {
    let data = value
        .get("data")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("GitHub GraphQL response missing data"))?;

    if let Some(errors) = value.get("errors").and_then(Value::as_array)
        && !errors.is_empty()
    {
        let all_not_found = errors
            .iter()
            .all(|error| error.get("type").and_then(Value::as_str) == Some("NOT_FOUND"));
        if all_not_found {
            return Ok(data);
        }

        let message = errors
            .iter()
            .filter_map(|error| error.get("message").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("; ");
        anyhow::bail!("{message}");
    }

    Ok(data)
}

fn map_projects_v2_error(error: anyhow::Error) -> anyhow::Error {
    let message = error.to_string();
    if message.contains("INSUFFICIENT_SCOPES") || message.contains("read:project") {
        anyhow::anyhow!(
            "GitHub access is missing Projects v2 scope (`read:project`). Open Settings -> GitHub and reconnect to refresh permissions."
        )
    } else {
        error
    }
}

fn fetch_issues_with_token(
    repo_path: &str,
    milestone: Option<&str>,
    token: &str,
) -> Result<Vec<GitHubIssue>> {
    let repo = repo_view_with_token(repo_path, token)?;
    let assignee = github_app::load_session()?
        .and_then(|session| session.user_login)
        .or_else(|| {
            github_app::fetch_viewer(token)
                .ok()
                .map(|viewer| viewer.login)
        });

    let mut endpoint = format!("repos/{}/issues?state=all&per_page=100", repo.full_name);
    if let Some(assignee) = assignee.as_deref().filter(|assignee| !assignee.is_empty()) {
        endpoint.push_str("&assignee=");
        endpoint.push_str(assignee);
    }

    if let Some(title) = milestone {
        let milestones = fetch_milestones_with_token(repo_path, token)?;
        let Some(selected) = milestones.iter().find(|candidate| candidate.title == title) else {
            return Ok(Vec::new());
        };
        endpoint.push_str("&milestone=");
        endpoint.push_str(&selected.number.to_string());
    }

    let values = github_api_list_with_token(token, &endpoint)?;
    let mut issues = Vec::new();
    for value in values {
        if value.get("pull_request").is_some() {
            continue;
        }
        let issue: GitHubIssue =
            serde_json::from_value(value).context("failed to parse GitHub issue response")?;
        issues.push(issue);
    }
    Ok(issues)
}

fn fetch_milestones_with_token(repo_path: &str, token: &str) -> Result<Vec<GitHubMilestone>> {
    let repo = repo_view_with_token(repo_path, token)?;
    let mut milestones: Vec<GitHubMilestone> = github_api_get_with_token(
        token,
        &format!("repos/{}/milestones?state=all&per_page=100", repo.full_name),
    )?;
    milestones.sort_by(|a, b| {
        let a_open = a.state == "open";
        let b_open = b.state == "open";
        b_open.cmp(&a_open).then_with(|| {
            a.due_on
                .as_deref()
                .unwrap_or("9999")
                .cmp(b.due_on.as_deref().unwrap_or("9999"))
        })
    });
    Ok(milestones)
}

fn repo_view_with_token(repo_path: &str, token: &str) -> Result<GitHubRepoView> {
    let full_name = github_repo_full_name(repo_path)?;
    let value: Value = github_api_get_with_token(token, &format!("repos/{full_name}"))?;
    let (owner, name) = full_name
        .split_once('/')
        .with_context(|| format!("invalid GitHub repository name `{full_name}`"))?;
    Ok(GitHubRepoView {
        owner: owner.to_string(),
        name: name.to_string(),
        full_name,
        url: optional_string_field(&value, "html_url")
            .or_else(|| optional_string_field(&value, "url")),
        default_branch: optional_string_field(&value, "default_branch"),
    })
}

fn sync_project_repo_with_token(
    store: &Store,
    project: &crate::store::Project,
    token: &str,
    selected_project_hint: Option<&str>,
) -> Result<GitHubSyncSummary> {
    let repo = repo_view_with_token(&project.repo_path, token)?;
    let cached_repo = store.upsert_github_repo(
        Some(&project.id),
        &repo.owner,
        &repo.name,
        repo.url.as_deref(),
        repo.default_branch.as_deref(),
    )?;

    let mut summary = GitHubSyncSummary {
        repo_full_name: repo.full_name.clone(),
        ..GitHubSyncSummary::default()
    };

    let discovered_projects = sync_projects_v2_for_repo(store, &repo, &cached_repo.id)?;
    summary.projects_synced = discovered_projects.len();
    if let Some(selected_project) =
        resolve_selected_project(&discovered_projects, selected_project_hint)
    {
        summary.project_items_synced =
            sync_project_v2_items(store, &repo, selected_project, &cached_repo.id)?;
    }

    for issue in github_api_list_with_token(
        token,
        &format!("repos/{}/issues?state=all&per_page=100", repo.full_name),
    )? {
        if issue.get("pull_request").is_some() {
            continue;
        }
        let item = store.upsert_github_item(
            &cached_repo.id,
            None,
            optional_string_field(&issue, "node_id").as_deref(),
            number_field(&issue, "number").context("issue missing number")?,
            GitHubItemKind::Issue,
            string_field(&issue, "title").context("issue missing title")?,
            optional_string_field(&issue, "state")
                .as_deref()
                .unwrap_or("unknown"),
            string_field(&issue, "html_url").context("issue missing html_url")?,
            optional_string_field(&issue, "body_text").as_deref(),
            &logins_array(&issue, "assignees"),
            &label_names(&issue),
            &serde_json::json!({}),
            &serde_json::json!({}),
            None,
            None,
            optional_string_field(&issue, "updated_at").as_deref(),
        )?;
        store.upsert_github_issue_cache(
            &item.id,
            optional_string_field(&issue, "body").as_deref(),
            optional_string_field(&issue, "body_text").as_deref(),
            optional_string_field(&issue, "body_html").as_deref(),
            issue
                .get("user")
                .and_then(|user| user.get("login"))
                .and_then(Value::as_str),
            issue
                .get("milestone")
                .and_then(|milestone| milestone.get("title"))
                .and_then(Value::as_str),
            Some(&serde_json::to_string(&issue)?),
        )?;
        summary.issues_synced += 1;

        for comment in github_api_list_with_token(
            token,
            &format!(
                "repos/{}/issues/{}/comments?per_page=100",
                repo.full_name, item.number
            ),
        )? {
            store.upsert_github_comment_cache(
                &item.id,
                &id_string(&comment),
                comment
                    .get("user")
                    .and_then(|user| user.get("login"))
                    .and_then(Value::as_str),
                optional_string_field(&comment, "body").as_deref(),
                optional_string_field(&comment, "body_text").as_deref(),
                optional_string_field(&comment, "body_html").as_deref(),
                string_field(&comment, "created_at").context("comment missing created_at")?,
                optional_string_field(&comment, "updated_at").as_deref(),
                Some(&serde_json::to_string(&comment)?),
            )?;
            summary.comments_synced += 1;
        }
    }

    let review_comments = github_api_list_with_token(
        token,
        &format!("repos/{}/pulls/comments?per_page=100", repo.full_name),
    )
    .unwrap_or_default();

    for pull in github_api_list_paginated_with_token(
        token,
        &format!("repos/{}/pulls?state=open&per_page=100", repo.full_name),
    )? {
        let number = number_field(&pull, "number").context("pull request missing number")?;
        let item = store.upsert_github_item(
            &cached_repo.id,
            None,
            optional_string_field(&pull, "node_id").as_deref(),
            number,
            GitHubItemKind::PullRequest,
            string_field(&pull, "title").context("pull request missing title")?,
            optional_string_field(&pull, "state")
                .as_deref()
                .unwrap_or("unknown"),
            string_field(&pull, "html_url").context("pull request missing html_url")?,
            optional_string_field(&pull, "body_text").as_deref(),
            &logins_array(&pull, "assignees"),
            &label_names(&pull),
            &serde_json::json!({}),
            &serde_json::json!({}),
            pull.get("base")
                .and_then(|base| base.get("ref"))
                .and_then(Value::as_str),
            pull.get("head")
                .and_then(|head| head.get("ref"))
                .and_then(Value::as_str),
            optional_string_field(&pull, "updated_at").as_deref(),
        )?;
        store.upsert_github_pr_cache(
            &item.id,
            optional_string_field(&pull, "body").as_deref(),
            optional_string_field(&pull, "body_text").as_deref(),
            optional_string_field(&pull, "body_html").as_deref(),
            pull.get("base")
                .and_then(|base| base.get("ref"))
                .and_then(Value::as_str),
            pull.get("head")
                .and_then(|head| head.get("ref"))
                .and_then(Value::as_str),
            optional_string_field(&pull, "merge_state_status").as_deref(),
            optional_string_field(&pull, "review_decision").as_deref(),
            pull.get("draft").and_then(Value::as_bool).unwrap_or(false),
            Some(&serde_json::to_string(&pull)?),
        )?;
        summary.prs_synced += 1;

        for review in github_api_list_with_token(
            token,
            &format!(
                "repos/{}/pulls/{number}/reviews?per_page=100",
                repo.full_name
            ),
        )? {
            let cached_review = store.upsert_github_review_cache(
                &item.id,
                &id_string(&review),
                optional_string_field(&review, "state")
                    .as_deref()
                    .unwrap_or("COMMENTED"),
                optional_string_field(&review, "commit_id").as_deref(),
                review
                    .get("user")
                    .and_then(|user| user.get("login"))
                    .and_then(Value::as_str),
                optional_string_field(&review, "body").as_deref(),
                optional_string_field(&review, "body_text").as_deref(),
                optional_string_field(&review, "body_html").as_deref(),
                optional_string_field(&review, "submitted_at").as_deref(),
                Some(&serde_json::to_string(&review)?),
            )?;
            summary.reviews_synced += 1;

            for review_comment in review_comments.iter().filter(|review_comment| {
                review_comment
                    .get("pull_request_url")
                    .and_then(Value::as_str)
                    .is_some_and(|url| url.ends_with(&format!("/{number}")))
                    && review_comment
                        .get("pull_request_review_id")
                        .and_then(Value::as_i64)
                        .map(|value| value.to_string())
                        .as_deref()
                        == Some(cached_review.github_review_id.as_str())
            }) {
                store.upsert_github_review_comment_cache(
                    Some(&cached_review.id),
                    &item.id,
                    &id_string(review_comment),
                    review_comment
                        .get("user")
                        .and_then(|user| user.get("login"))
                        .and_then(Value::as_str),
                    optional_string_field(review_comment, "path").as_deref(),
                    review_comment.get("line").and_then(Value::as_i64),
                    optional_string_field(review_comment, "side").as_deref(),
                    review_comment.get("start_line").and_then(Value::as_i64),
                    optional_string_field(review_comment, "diff_hunk").as_deref(),
                    review_comment
                        .get("in_reply_to_id")
                        .and_then(Value::as_i64)
                        .map(|value| value.to_string())
                        .as_deref(),
                    optional_string_field(review_comment, "body").as_deref(),
                    optional_string_field(review_comment, "body_text").as_deref(),
                    optional_string_field(review_comment, "body_html").as_deref(),
                    string_field(review_comment, "created_at")
                        .context("review comment missing created_at")?,
                    optional_string_field(review_comment, "updated_at").as_deref(),
                    Some(&serde_json::to_string(review_comment)?),
                )?;
                summary.review_comments_synced += 1;
            }
        }
    }

    link_prs_to_issues(store, &cached_repo.id)?;

    Ok(summary)
}

// ── GitHub mutation API ─────────────────────────────────────────────

/// Create a new issue on the repository.
pub fn create_issue(
    repo_path: &str,
    title: &str,
    body: Option<&str>,
    labels: &[String],
    assignees: &[String],
) -> Result<GitHubIssue> {
    let token =
        preferred_github_token()?.ok_or_else(|| anyhow::anyhow!("no GitHub token available"))?;
    let repo = github_repo_full_name(repo_path)?;
    let mut payload = serde_json::json!({ "title": title });
    if let Some(body) = body {
        payload["body"] = serde_json::json!(body);
    }
    if !labels.is_empty() {
        payload["labels"] = serde_json::json!(labels);
    }
    if !assignees.is_empty() {
        payload["assignees"] = serde_json::json!(assignees);
    }
    github_api_post_with_token(&token, &format!("repos/{repo}/issues"), &payload)
}

/// Edit an existing issue (title, body, labels, assignees).
pub fn edit_issue(
    repo_path: &str,
    number: i64,
    title: Option<&str>,
    body: Option<&str>,
    labels: Option<&[String]>,
    assignees: Option<&[String]>,
) -> Result<GitHubIssue> {
    let token =
        preferred_github_token()?.ok_or_else(|| anyhow::anyhow!("no GitHub token available"))?;
    let repo = github_repo_full_name(repo_path)?;
    let mut payload = serde_json::json!({});
    if let Some(title) = title {
        payload["title"] = serde_json::json!(title);
    }
    if let Some(body) = body {
        payload["body"] = serde_json::json!(body);
    }
    if let Some(labels) = labels {
        payload["labels"] = serde_json::json!(labels);
    }
    if let Some(assignees) = assignees {
        payload["assignees"] = serde_json::json!(assignees);
    }
    github_api_patch_with_token(&token, &format!("repos/{repo}/issues/{number}"), &payload)
}

/// Post a comment on an issue or pull request.
pub fn create_comment(repo_path: &str, number: i64, body: &str) -> Result<Value> {
    let token =
        preferred_github_token()?.ok_or_else(|| anyhow::anyhow!("no GitHub token available"))?;
    let repo = github_repo_full_name(repo_path)?;
    let payload = serde_json::json!({ "body": body });
    github_api_post_with_token(
        &token,
        &format!("repos/{repo}/issues/{number}/comments"),
        &payload,
    )
}

/// Set the state of an issue or pull request (`open` or `closed`).
pub fn set_issue_state(repo_path: &str, number: i64, state: &str) -> Result<()> {
    let token =
        preferred_github_token()?.ok_or_else(|| anyhow::anyhow!("no GitHub token available"))?;
    let repo = github_repo_full_name(repo_path)?;
    let payload = serde_json::json!({ "state": state });
    let _: Value =
        github_api_patch_with_token(&token, &format!("repos/{repo}/issues/{number}"), &payload)?;
    Ok(())
}

/// Add labels to an issue or pull request.
pub fn add_labels(repo_path: &str, number: i64, labels: &[String]) -> Result<()> {
    let token =
        preferred_github_token()?.ok_or_else(|| anyhow::anyhow!("no GitHub token available"))?;
    let repo = github_repo_full_name(repo_path)?;
    let payload = serde_json::json!({ "labels": labels });
    let _: Value = github_api_post_with_token(
        &token,
        &format!("repos/{repo}/issues/{number}/labels"),
        &payload,
    )?;
    Ok(())
}

/// Remove a single label from an issue or pull request.
pub fn remove_label(repo_path: &str, number: i64, label: &str) -> Result<()> {
    let token =
        preferred_github_token()?.ok_or_else(|| anyhow::anyhow!("no GitHub token available"))?;
    let repo = github_repo_full_name(repo_path)?;
    let encoded = percent_encode_label(label);
    github_api_delete_with_token(
        &token,
        &format!("repos/{repo}/issues/{number}/labels/{encoded}"),
    )
}

fn github_repo_full_name(repo_path: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo_path)
        .output()
        .context("failed to run `git remote get-url origin`")?;

    if !output.status.success() {
        anyhow::bail!(
            "git remote get-url origin failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_github_remote(&remote)
        .with_context(|| format!("failed to parse GitHub owner/repo from remote `{remote}`"))
}

fn parse_github_remote(remote: &str) -> Result<String> {
    let trimmed = remote.trim().trim_end_matches(".git");

    if let Some(path) = trimmed.strip_prefix("git@github.com:") {
        return Ok(path.to_string());
    }
    if let Some(path) = trimmed.strip_prefix("ssh://git@github.com/") {
        return Ok(path.to_string());
    }
    if let Some(path) = trimmed.strip_prefix("https://github.com/") {
        return Ok(path.to_string());
    }
    if let Some(path) = trimmed.strip_prefix("http://github.com/") {
        return Ok(path.to_string());
    }

    anyhow::bail!("unsupported GitHub remote URL")
}

fn github_api_get_with_token<T>(token: &str, endpoint: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let url = format!("{GITHUB_API_BASE}/{endpoint}");
    let response = ureq::get(&url)
        .set("Accept", "application/vnd.github.full+json")
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "claustre")
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .call()
        .map_err(http_error)?;
    response
        .into_json()
        .with_context(|| format!("failed to parse GitHub API response for {endpoint}"))
}

fn github_api_list_with_token(token: &str, endpoint: &str) -> Result<Vec<Value>> {
    github_api_get_with_token(token, endpoint)
}

/// Paginated variant — follows `Link: rel="next"` headers.
/// Use only for endpoints with bounded result sets (e.g. open PRs).
fn github_api_list_paginated_with_token(token: &str, endpoint: &str) -> Result<Vec<Value>> {
    let mut all_items: Vec<Value> = Vec::new();
    let mut url = format!("{GITHUB_API_BASE}/{endpoint}");
    const MAX_PAGES: usize = 10;

    for _ in 0..MAX_PAGES {
        let response = ureq::get(&url)
            .set("Accept", "application/vnd.github.full+json")
            .set("Authorization", &format!("Bearer {token}"))
            .set("User-Agent", "claustre")
            .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
            .call()
            .map_err(http_error)?;

        let next_url = response.header("Link").and_then(parse_next_link_url);

        let page: Vec<Value> = response
            .into_json()
            .with_context(|| format!("failed to parse GitHub API list response for {endpoint}"))?;

        if page.is_empty() {
            break;
        }
        all_items.extend(page);

        match next_url {
            Some(next) => url = next,
            None => break,
        }
    }

    Ok(all_items)
}

/// Parse the `next` URL from a GitHub `Link` header.
/// Format: `<https://api.github.com/...?page=2>; rel="next", <...>; rel="last"`
fn parse_next_link_url(link_header: &str) -> Option<String> {
    for part in link_header.split(',') {
        let part = part.trim();
        if part.contains("rel=\"next\"") {
            if let Some(start) = part.find('<') {
                if let Some(end) = part.find('>') {
                    return Some(part[start + 1..end].to_string());
                }
            }
        }
    }
    None
}

fn github_api_post_with_token<T>(token: &str, endpoint: &str, body: &Value) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let url = format!("{GITHUB_API_BASE}/{endpoint}");
    let response = ureq::post(&url)
        .set("Accept", "application/vnd.github+json")
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "claustre")
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .send_json(body)
        .map_err(http_error)?;
    response
        .into_json()
        .with_context(|| format!("failed to parse GitHub API response for POST {endpoint}"))
}

fn github_api_patch_with_token<T>(token: &str, endpoint: &str, body: &Value) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let url = format!("{GITHUB_API_BASE}/{endpoint}");
    let response = ureq::request("PATCH", &url)
        .set("Accept", "application/vnd.github+json")
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "claustre")
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .send_json(body)
        .map_err(http_error)?;
    response
        .into_json()
        .with_context(|| format!("failed to parse GitHub API response for PATCH {endpoint}"))
}

fn github_api_delete_with_token(token: &str, endpoint: &str) -> Result<()> {
    let url = format!("{GITHUB_API_BASE}/{endpoint}");
    ureq::delete(&url)
        .set("Accept", "application/vnd.github+json")
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "claustre")
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .call()
        .map_err(http_error)?;
    Ok(())
}

fn http_error(error: ureq::Error) -> anyhow::Error {
    match error {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            anyhow::anyhow!("GitHub request failed ({code}): {}", body.trim())
        }
        ureq::Error::Transport(error) => anyhow::anyhow!("GitHub request failed: {error}"),
    }
}

fn gh_api_list(repo_path: &str, endpoint: &str) -> Result<Vec<Value>> {
    let output = Command::new("gh")
        .args([
            "api",
            "-H",
            "Accept: application/vnd.github.full+json",
            endpoint,
        ])
        .current_dir(repo_path)
        .output()
        .with_context(|| format!("failed to run `gh api {endpoint}`"))?;

    if !output.status.success() {
        anyhow::bail!(
            "gh api {} failed: {}",
            endpoint,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("failed to parse gh api response for {endpoint}"))
}

fn optional_string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn number_field(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn logins_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("login")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn label_names(value: &Value) -> Vec<String> {
    value
        .get("labels")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Minimal percent-encoding for GitHub label names in URL path segments.
fn percent_encode_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for byte in label.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Link PRs to issues using GitHub's `closingIssuesReferences` API data first,
/// falling back to body-text regex scanning for `Closes #N`, `Fixes #N`, etc.
fn link_prs_to_issues(store: &Store, repo_id: &str) -> Result<usize> {
    let items = store.list_github_items_for_repo(repo_id)?;

    // Build a lookup from issue number → item id (issues only).
    let issue_by_number: std::collections::HashMap<i64, String> = items
        .iter()
        .filter(|item| item.kind == GitHubItemKind::Issue)
        .map(|item| (item.number, item.id.clone()))
        .collect();

    let mut linked = 0usize;
    let mut linked_issue_ids = std::collections::HashSet::new();

    for pr_item in items
        .iter()
        .filter(|item| item.kind == GitHubItemKind::PullRequest)
    {
        // Only link open PRs to issues.
        let pr_state = pr_item.state.to_uppercase();
        if pr_state == "CLOSED" || pr_state == "MERGED" {
            continue;
        }

        let mut referenced_issue_numbers: Vec<i64> = Vec::new();

        // 1. Use GitHub API's closingIssuesReferences from the PR cache (authoritative).
        if let Ok(pr_cache) = store.get_github_pr_cache(&pr_item.id)
            && let Some(payload) = pr_cache.json_payload.as_deref()
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(payload)
            && let Some(nodes) = json
                .get("closingIssuesReferences")
                .and_then(|refs| refs.get("nodes"))
                .and_then(serde_json::Value::as_array)
        {
            for node in nodes {
                if let Some(number) = node.get("number").and_then(serde_json::Value::as_i64) {
                    referenced_issue_numbers.push(number);
                }
            }
        }

        // 2. Fallback: scan PR title + body text for closing keywords.
        if referenced_issue_numbers.is_empty() {
            // Check title first (e.g. "Fix #26570: Add continuous migrations")
            for cap in CLOSING_REF_RE.captures_iter(&pr_item.title) {
                if let Ok(issue_number) = cap[1].parse::<i64>() {
                    referenced_issue_numbers.push(issue_number);
                }
            }
            // Then body
            if referenced_issue_numbers.is_empty() {
                let body = store
                    .get_github_pr_cache(&pr_item.id)
                    .ok()
                    .and_then(|cache| cache.body.or(cache.body_text))
                    .or_else(|| pr_item.body_text.clone());

                if let Some(body) = body {
                    for cap in CLOSING_REF_RE.captures_iter(&body) {
                        if let Ok(issue_number) = cap[1].parse::<i64>() {
                            referenced_issue_numbers.push(issue_number);
                        }
                    }
                }
            }
        }

        for issue_number in referenced_issue_numbers {
            if let Some(issue_item_id) = issue_by_number.get(&issue_number) {
                store.update_linked_pr(issue_item_id, pr_item.number, &pr_item.id)?;
                linked_issue_ids.insert(issue_item_id.clone());
                linked += 1;
                debug!(
                    issue_number,
                    pr_number = pr_item.number,
                    "linked PR to issue"
                );
            }
        }
    }

    // 3. For open issues still without a linked PR, try the GitHub API timeline.
    //    This catches PRs linked via the "Development" sidebar that don't use
    //    closing keywords in their title or body.
    let unlinked_issues: Vec<_> = items
        .iter()
        .filter(|item| {
            item.kind == GitHubItemKind::Issue
                && !linked_issue_ids.contains(&item.id)
                && item.state.eq_ignore_ascii_case("open")
        })
        .take(20)
        .collect();

    if !unlinked_issues.is_empty()
        && let Ok(Some(token)) = preferred_github_token()
    {
        let repo_cache = store
            .list_github_repos()?
            .into_iter()
            .find(|r| r.id == repo_id);
        let repo_full_name = repo_cache.map(|r| r.full_name);

        if let Some(repo_name) = repo_full_name {
            for item in &unlinked_issues {
                if let Ok(events) = github_api_list_with_token(
                    &token,
                    &format!(
                        "repos/{repo_name}/issues/{}/timeline?per_page=50",
                        item.number
                    ),
                ) {
                    for event in &events {
                        let event_type = event
                            .get("event")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if event_type != "cross-referenced" && event_type != "connected" {
                            continue;
                        }
                        // cross-referenced events have source.issue which is the PR
                        let pr_number = event
                            .get("source")
                            .and_then(|s| s.get("issue"))
                            .and_then(|issue| issue.get("number"))
                            .and_then(Value::as_i64)
                            .or_else(|| {
                                // connected events may have the PR number directly
                                event.get("number").and_then(Value::as_i64)
                            });
                        let is_pr = event
                            .get("source")
                            .and_then(|s| s.get("issue"))
                            .and_then(|issue| issue.get("pull_request"))
                            .is_some();

                        if let Some(pr_num) = pr_number
                            && is_pr
                        {
                            // Find the PR in our cache, or create a reference
                            if let Some(pr_item_id) = items
                                .iter()
                                .find(|i| {
                                    i.kind == GitHubItemKind::PullRequest && i.number == pr_num
                                })
                                .map(|i| i.id.clone())
                            {
                                store.update_linked_pr(&item.id, pr_num, &pr_item_id)?;
                            } else {
                                // PR not in cache — store number only
                                store.update_linked_pr(&item.id, pr_num, "")?;
                            }
                            linked_issue_ids.insert(item.id.clone());
                            linked += 1;
                            debug!(
                                issue_number = item.number,
                                pr_number = pr_num,
                                "linked PR to issue via timeline"
                            );
                            break;
                        }
                    }
                }
            }
        }
    }

    // Clear stale links: issues that previously had a linked PR but no longer do.
    for item in items
        .iter()
        .filter(|item| item.kind == GitHubItemKind::Issue)
    {
        if item.linked_pr_item_id.is_some() && !linked_issue_ids.contains(&item.id) {
            store.clear_linked_pr(&item.id)?;
        }
    }

    Ok(linked)
}

fn id_string(value: &Value) -> String {
    value
        .get("id")
        .and_then(Value::as_i64)
        .map(|id| id.to_string())
        .or_else(|| optional_string_field(value, "node_id"))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_issue(state: &str, labels: Vec<&str>) -> GitHubIssue {
        GitHubIssue {
            number: 1,
            title: "test".to_string(),
            body: None,
            state: state.to_string(),
            url: "https://github.com/test/1".to_string(),
            labels: labels
                .into_iter()
                .map(|name| GitHubLabel {
                    name: name.to_string(),
                    color: None,
                })
                .collect(),
            assignees: vec![],
            milestone: None,
            created_at: None,
        }
    }

    #[test]
    fn assign_column_closed_goes_to_last() {
        let columns = vec![
            ("Backlog".to_string(), vec![]),
            ("In Progress".to_string(), vec!["in progress".to_string()]),
            ("Done".to_string(), vec![]),
        ];
        let issue = make_issue("CLOSED", vec![]);
        assert_eq!(assign_column(&issue, &columns), 2);
    }

    #[test]
    fn assign_column_matches_label() {
        let columns = vec![
            ("Backlog".to_string(), vec![]),
            (
                "In Progress".to_string(),
                vec!["in progress".to_string(), "wip".to_string()],
            ),
            ("In Review".to_string(), vec!["in review".to_string()]),
            ("Done".to_string(), vec![]),
        ];
        let issue = make_issue("OPEN", vec!["In Progress"]);
        assert_eq!(assign_column(&issue, &columns), 1);
    }

    #[test]
    fn assign_column_no_match_goes_to_first() {
        let columns = vec![
            ("Backlog".to_string(), vec![]),
            ("In Progress".to_string(), vec!["in progress".to_string()]),
            ("Done".to_string(), vec![]),
        ];
        let issue = make_issue("OPEN", vec!["bug"]);
        assert_eq!(assign_column(&issue, &columns), 0);
    }

    #[test]
    fn assign_column_case_insensitive() {
        let columns = vec![
            ("Backlog".to_string(), vec![]),
            ("In Progress".to_string(), vec!["IN PROGRESS".to_string()]),
            ("Done".to_string(), vec![]),
        ];
        let issue = make_issue("OPEN", vec!["in progress"]);
        assert_eq!(assign_column(&issue, &columns), 1);
    }

    #[test]
    fn current_milestone_returns_first_open() {
        let milestones = vec![
            GitHubMilestone {
                number: 1,
                title: "Sprint 1".to_string(),
                state: "closed".to_string(),
                due_on: Some("2024-01-01".to_string()),
            },
            GitHubMilestone {
                number: 2,
                title: "Sprint 2".to_string(),
                state: "open".to_string(),
                due_on: Some("2024-02-01".to_string()),
            },
            GitHubMilestone {
                number: 3,
                title: "Sprint 3".to_string(),
                state: "open".to_string(),
                due_on: Some("2024-03-01".to_string()),
            },
        ];
        let current = current_milestone(&milestones).expect("should find an open milestone");
        assert_eq!(current.title, "Sprint 2");
    }

    #[test]
    fn current_milestone_returns_none_when_all_closed() {
        let milestones = vec![GitHubMilestone {
            number: 1,
            title: "Sprint 1".to_string(),
            state: "closed".to_string(),
            due_on: None,
        }];
        assert!(current_milestone(&milestones).is_none());
    }

    #[test]
    fn parse_graphql_response_tolerates_partial_not_found_errors() {
        let value = serde_json::json!({
            "data": {
                "organization": {
                    "projectsV2": {
                        "nodes": [{ "number": 105, "title": "Documentation" }]
                    }
                },
                "user": null
            },
            "errors": [{
                "type": "NOT_FOUND",
                "path": ["user"],
                "message": "Could not resolve to a User with the login of 'open-metadata'."
            }]
        });

        let data = parse_graphql_response(&value).expect("partial owner miss should succeed");
        assert_eq!(
            data["organization"]["projectsV2"]["nodes"][0]["title"],
            "Documentation"
        );
    }

    #[test]
    fn parse_graphql_response_rejects_non_not_found_errors() {
        let value = serde_json::json!({
            "data": {
                "organization": null,
                "user": null
            },
            "errors": [{
                "type": "FORBIDDEN",
                "message": "Resource not accessible by integration"
            }]
        });

        let error = parse_graphql_response(&value).expect_err("forbidden responses must fail");
        assert!(
            error
                .to_string()
                .contains("Resource not accessible by integration")
        );
    }

    #[test]
    fn parse_github_remote_handles_https() {
        assert_eq!(
            parse_github_remote("https://github.com/open-metadata/OpenMetadata.git").unwrap(),
            "open-metadata/OpenMetadata"
        );
    }

    #[test]
    fn parse_github_remote_handles_ssh() {
        assert_eq!(
            parse_github_remote("git@github.com:open-metadata/OpenMetadata.git").unwrap(),
            "open-metadata/OpenMetadata"
        );
    }

    #[test]
    fn load_project_board_from_cache_uses_cached_repo_projects_and_items() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("OpenMetadata", "/tmp/openmetadata", "main", true)
            .unwrap();
        let repo = store
            .upsert_github_repo(
                Some(&project.id),
                "open-metadata",
                "OpenMetadata",
                Some("https://github.com/open-metadata/OpenMetadata"),
                Some("main"),
            )
            .unwrap();
        let board = store
            .upsert_github_project_v2(&repo.id, 81, "Shipping", None, None)
            .unwrap();
        store
            .upsert_github_item(
                &repo.id,
                Some(&board.id),
                Some("node-1"),
                24231,
                GitHubItemKind::Issue,
                "Tableau as a pipeline service",
                "OPEN",
                "https://github.com/open-metadata/OpenMetadata/issues/24231",
                Some("body"),
                &[String::from("harshach")],
                &[String::from("enhancement")],
                &serde_json::json!({}),
                &serde_json::json!({
                    "Status": "Backlog",
                    "Sprint": "Sprint 2",
                }),
                None,
                None,
                Some("2026-03-18T00:00:00Z"),
            )
            .unwrap();

        let snapshot = load_project_board_from_cache(&store, &project, Some("81")).unwrap();
        assert_eq!(snapshot.projects.len(), 1);
        assert_eq!(
            snapshot
                .selected_project
                .as_ref()
                .map(|selected| selected.project_number),
            Some(81)
        );
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].number, 24231);
        assert_eq!(
            snapshot.items[0]
                .project_field_values
                .get("Sprint")
                .and_then(Value::as_str),
            Some("Sprint 2")
        );
    }

    #[test]
    fn closing_ref_regex_matches_variants() {
        let bodies = [
            "This PR closes #42",
            "Fixes #100 and also fixes #200",
            "resolves #5",
            "Close #7",
            "fix #8",
            "CLOSES #9",
        ];
        let expected: Vec<Vec<i64>> =
            vec![vec![42], vec![100, 200], vec![5], vec![7], vec![8], vec![9]];
        for (body, expected) in bodies.iter().zip(expected.iter()) {
            let found: Vec<i64> = CLOSING_REF_RE
                .captures_iter(body)
                .filter_map(|cap| cap[1].parse().ok())
                .collect();
            assert_eq!(&found, expected, "body: {body}");
        }
    }

    #[test]
    fn closing_ref_regex_no_false_positives() {
        let body = "See issue #42 for details. Not closing anything.";
        let found: Vec<i64> = CLOSING_REF_RE
            .captures_iter(body)
            .filter_map(|cap| cap[1].parse().ok())
            .collect();
        assert!(found.is_empty(), "should not match plain #42 reference");
    }

    #[test]
    fn link_prs_to_issues_sets_linked_pr() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("proj", "/tmp/proj", "main", true)
            .unwrap();
        let repo = store
            .upsert_github_repo(
                Some(&project.id),
                "owner",
                "repo",
                Some("https://github.com/owner/repo"),
                Some("main"),
            )
            .unwrap();
        // Create an issue
        let issue = store
            .upsert_github_item(
                &repo.id,
                None,
                Some("node-issue"),
                42,
                GitHubItemKind::Issue,
                "Fix the bug",
                "OPEN",
                "https://github.com/owner/repo/issues/42",
                None,
                &[],
                &[],
                &serde_json::json!({}),
                &serde_json::json!({}),
                None,
                None,
                None,
            )
            .unwrap();
        // Create a PR that closes #42
        let pr = store
            .upsert_github_item(
                &repo.id,
                None,
                Some("node-pr"),
                10,
                GitHubItemKind::PullRequest,
                "Fix: resolve bug",
                "OPEN",
                "https://github.com/owner/repo/pull/10",
                None,
                &[],
                &[],
                &serde_json::json!({}),
                &serde_json::json!({}),
                Some("main"),
                Some("fix/bug"),
                None,
            )
            .unwrap();
        store
            .upsert_github_pr_cache(
                &pr.id,
                Some("Closes #42"),
                Some("Closes #42"),
                None,
                Some("main"),
                Some("fix/bug"),
                None,
                None,
                false,
                None,
            )
            .unwrap();

        let linked = link_prs_to_issues(&store, &repo.id).unwrap();
        assert_eq!(linked, 1);

        let updated_issue = store.get_github_item(&issue.id).unwrap();
        assert_eq!(updated_issue.linked_pr_number, Some(10));
        assert_eq!(
            updated_issue.linked_pr_item_id.as_deref(),
            Some(pr.id.as_str())
        );

        // find_linked_pr_for_issue should return the PR
        let linked_pr = store.find_linked_pr_for_issue(&issue.id).unwrap();
        assert!(linked_pr.is_some());
        assert_eq!(linked_pr.unwrap().number, 10);
    }
}
