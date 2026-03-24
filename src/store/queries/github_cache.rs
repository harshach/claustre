//! GitHub cache persistence for issues, PRs, reviews, and comments.

use anyhow::{Context, Result};
use rusqlite::params;
use tracing::warn;
use uuid::Uuid;

use crate::store::Store;
use crate::store::models::{
    GitHubCommentCache, GitHubIssueCache, GitHubItem, GitHubItemKind, GitHubPrCache,
    GitHubProjectV2Cache, GitHubRepoCache, GitHubReviewCache, GitHubReviewCommentCache,
};

use super::optional;

const GITHUB_REPO_COLUMNS: &str =
    "id, project_id, owner, name, full_name, repo_url, default_branch, synced_at, created_at";

const GITHUB_PROJECT_V2_COLUMNS: &str =
    "id, repo_id, project_number, title, url, node_id, synced_at, created_at";

const GITHUB_ITEM_COLUMNS: &str = "\
    id, repo_id, project_v2_id, node_id, number, kind, title, state, url, body_text, \
    assignee_logins, label_names, label_colors, project_field_values, base_ref, head_ref, \
    github_updated_at, synced_at, created_at, updated_at, linked_pr_number, linked_pr_item_id";

const GITHUB_ISSUE_CACHE_COLUMNS: &str =
    "item_id, body, body_text, body_html, author_login, milestone_title, json_payload, cached_at";

const GITHUB_PR_CACHE_COLUMNS: &str = "\
    item_id, body, body_text, body_html, base_ref, head_ref, merge_state_status, review_decision, \
    is_draft, json_payload, cached_at";

const GITHUB_COMMENT_CACHE_COLUMNS: &str = "\
    id, item_id, github_comment_id, author_login, body, body_text, body_html, created_at, \
    updated_at, json_payload";

const GITHUB_REVIEW_CACHE_COLUMNS: &str = "\
    id, item_id, github_review_id, state, commit_id, author_login, body, body_text, body_html, \
    submitted_at, json_payload";

const GITHUB_REVIEW_COMMENT_CACHE_COLUMNS: &str = "\
    id, review_id, item_id, github_comment_id, author_login, path, line, side, start_line, \
    diff_hunk, in_reply_to_id, body, body_text, body_html, created_at, updated_at, json_payload";

impl Store {
    pub fn upsert_github_repo(
        &self,
        project_id: Option<&str>,
        owner: &str,
        name: &str,
        repo_url: Option<&str>,
        default_branch: Option<&str>,
    ) -> Result<GitHubRepoCache> {
        let full_name = format!("{owner}/{name}");
        if let Some(existing) = self.get_github_repo_by_full_name(&full_name)? {
            self.conn.execute(
                "UPDATE github_repos
                 SET project_id = ?1, repo_url = ?2, default_branch = ?3, synced_at = ?4
                 WHERE id = ?5",
                params![
                    project_id,
                    repo_url,
                    default_branch,
                    chrono::Utc::now().to_rfc3339(),
                    existing.id
                ],
            )?;
            return self.get_github_repo(&existing.id);
        }

        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO github_repos (id, project_id, owner, name, full_name, repo_url, default_branch, synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                project_id,
                owner,
                name,
                full_name,
                repo_url,
                default_branch,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        self.get_github_repo(&id)
    }

    pub fn get_github_repo(&self, id: &str) -> Result<GitHubRepoCache> {
        let sql = format!("SELECT {GITHUB_REPO_COLUMNS} FROM github_repos WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_github_repo)
            .with_context(|| format!("failed to fetch GitHub repo '{id}'"))
    }

    pub fn get_github_repo_by_full_name(&self, full_name: &str) -> Result<Option<GitHubRepoCache>> {
        let sql = format!("SELECT {GITHUB_REPO_COLUMNS} FROM github_repos WHERE full_name = ?1");
        optional(
            self.conn
                .query_row(&sql, params![full_name], Self::row_to_github_repo),
        )
        .with_context(|| format!("failed to fetch GitHub repo '{full_name}'"))
    }

    pub fn list_github_repos(&self) -> Result<Vec<GitHubRepoCache>> {
        let sql = format!("SELECT {GITHUB_REPO_COLUMNS} FROM github_repos ORDER BY full_name");
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map([], Self::row_to_github_repo)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn get_github_repo_for_project(&self, project_id: &str) -> Result<Option<GitHubRepoCache>> {
        let sql = format!("SELECT {GITHUB_REPO_COLUMNS} FROM github_repos WHERE project_id = ?1");
        optional(
            self.conn
                .query_row(&sql, params![project_id], Self::row_to_github_repo),
        )
        .with_context(|| format!("failed to fetch GitHub repo for project '{project_id}'"))
    }

    pub fn upsert_github_project_v2(
        &self,
        repo_id: &str,
        project_number: i64,
        title: &str,
        url: Option<&str>,
        node_id: Option<&str>,
    ) -> Result<GitHubProjectV2Cache> {
        let existing_id: Option<String> = optional(self.conn.query_row(
            "SELECT id FROM github_projects_v2 WHERE repo_id = ?1 AND project_number = ?2",
            params![repo_id, project_number],
            |row| row.get(0),
        ))?;

        if let Some(existing_id) = existing_id {
            self.conn.execute(
                "UPDATE github_projects_v2 SET title = ?1, url = ?2, node_id = ?3, synced_at = ?4 WHERE id = ?5",
                params![title, url, node_id, chrono::Utc::now().to_rfc3339(), existing_id],
            )?;
            return self.get_github_project_v2(&existing_id);
        }

        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO github_projects_v2 (id, repo_id, project_number, title, url, node_id, synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                repo_id,
                project_number,
                title,
                url,
                node_id,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        self.get_github_project_v2(&id)
    }

    pub fn get_github_project_v2(&self, id: &str) -> Result<GitHubProjectV2Cache> {
        let sql =
            format!("SELECT {GITHUB_PROJECT_V2_COLUMNS} FROM github_projects_v2 WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_github_project_v2)
            .with_context(|| format!("failed to fetch GitHub project '{id}'"))
    }

    pub fn list_github_projects_v2_for_repo(
        &self,
        repo_id: &str,
    ) -> Result<Vec<GitHubProjectV2Cache>> {
        let sql = format!(
            "SELECT {GITHUB_PROJECT_V2_COLUMNS} FROM github_projects_v2 WHERE repo_id = ?1 ORDER BY title, project_number"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![repo_id], Self::row_to_github_project_v2)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "GitHub items mirror remote payload fields"
    )]
    pub fn upsert_github_item(
        &self,
        repo_id: &str,
        project_v2_id: Option<&str>,
        node_id: Option<&str>,
        number: i64,
        kind: GitHubItemKind,
        title: &str,
        state: &str,
        url: &str,
        body_text: Option<&str>,
        assignee_logins: &[String],
        label_names: &[String],
        label_colors: &serde_json::Value,
        project_field_values: &serde_json::Value,
        base_ref: Option<&str>,
        head_ref: Option<&str>,
        github_updated_at: Option<&str>,
    ) -> Result<GitHubItem> {
        let existing_id: Option<String> = optional(self.conn.query_row(
            "SELECT id FROM github_items WHERE repo_id = ?1 AND number = ?2 AND kind = ?3",
            params![repo_id, number, kind.as_str()],
            |row| row.get(0),
        ))?;
        let assignees_json = serde_json::to_string(assignee_logins)?;
        let labels_json = serde_json::to_string(label_names)?;
        let label_colors_json = serde_json::to_string(label_colors)?;
        let field_values_json = serde_json::to_string(project_field_values)?;

        if let Some(existing_id) = existing_id {
            self.conn.execute(
                "UPDATE github_items
                 SET project_v2_id = ?1, node_id = ?2, title = ?3, state = ?4, url = ?5,
                     body_text = ?6, assignee_logins = ?7, label_names = ?8, label_colors = ?9,
                     project_field_values = ?10, base_ref = ?11, head_ref = ?12,
                     github_updated_at = ?13, synced_at = ?14, updated_at = ?14
                 WHERE id = ?15",
                params![
                    project_v2_id,
                    node_id,
                    title,
                    state,
                    url,
                    body_text,
                    assignees_json,
                    labels_json,
                    label_colors_json,
                    field_values_json,
                    base_ref,
                    head_ref,
                    github_updated_at,
                    chrono::Utc::now().to_rfc3339(),
                    existing_id
                ],
            )?;
            return self.get_github_item(&existing_id);
        }

        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO github_items (
                id, repo_id, project_v2_id, node_id, number, kind, title, state, url, body_text,
                assignee_logins, label_names, label_colors, project_field_values, base_ref,
                head_ref, github_updated_at, synced_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?18)",
            params![
                id,
                repo_id,
                project_v2_id,
                node_id,
                number,
                kind.as_str(),
                title,
                state,
                url,
                body_text,
                assignees_json,
                labels_json,
                label_colors_json,
                field_values_json,
                base_ref,
                head_ref,
                github_updated_at,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        self.get_github_item(&id)
    }

    pub fn get_github_item(&self, id: &str) -> Result<GitHubItem> {
        let sql = format!("SELECT {GITHUB_ITEM_COLUMNS} FROM github_items WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_github_item)
            .with_context(|| format!("failed to fetch GitHub item '{id}'"))
    }

    pub fn list_github_items_for_repo(&self, repo_id: &str) -> Result<Vec<GitHubItem>> {
        let sql = format!(
            "SELECT {GITHUB_ITEM_COLUMNS} FROM github_items WHERE repo_id = ?1 ORDER BY number DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![repo_id], Self::row_to_github_item)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn list_github_items_for_project_v2(&self, project_v2_id: &str) -> Result<Vec<GitHubItem>> {
        let sql = format!(
            "SELECT {GITHUB_ITEM_COLUMNS} FROM github_items WHERE project_v2_id = ?1 ORDER BY number"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![project_v2_id], Self::row_to_github_item)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn clear_github_project_assignments(&self, project_v2_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE github_items
             SET project_v2_id = NULL,
                 project_field_values = '{}',
                 synced_at = ?2,
                 updated_at = ?2
             WHERE project_v2_id = ?1",
            params![project_v2_id, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// Set the linked PR on an issue's `github_items` row.
    pub fn update_linked_pr(&self, item_id: &str, pr_number: i64, pr_item_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE github_items
             SET linked_pr_number = ?1, linked_pr_item_id = ?2
             WHERE id = ?3",
            params![pr_number, pr_item_id, item_id],
        )?;
        Ok(())
    }

    /// Clear the linked PR from an issue when the link is no longer valid.
    pub fn clear_linked_pr(&self, item_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE github_items
             SET linked_pr_number = NULL, linked_pr_item_id = NULL
             WHERE id = ?1",
            params![item_id],
        )?;
        Ok(())
    }

    /// Look up the linked PR `GitHubItem` for an issue, if one exists.
    pub fn find_linked_pr_for_issue(&self, issue_item_id: &str) -> Result<Option<GitHubItem>> {
        let pr_item_id: Option<String> = optional(self.conn.query_row(
            "SELECT linked_pr_item_id FROM github_items WHERE id = ?1",
            params![issue_item_id],
            |row| row.get(0),
        ))?
        .flatten();
        match pr_item_id {
            Some(id) => optional(self.conn.query_row(
                &format!("SELECT {GITHUB_ITEM_COLUMNS} FROM github_items WHERE id = ?1"),
                params![id],
                Self::row_to_github_item,
            )),
            None => Ok(None),
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "issue cache mirrors GitHub payload"
    )]
    pub fn upsert_github_issue_cache(
        &self,
        item_id: &str,
        body: Option<&str>,
        body_text: Option<&str>,
        body_html: Option<&str>,
        author_login: Option<&str>,
        milestone_title: Option<&str>,
        json_payload: Option<&str>,
    ) -> Result<GitHubIssueCache> {
        self.conn.execute(
            "INSERT INTO github_issue_cache (
                item_id, body, body_text, body_html, author_login, milestone_title, json_payload, cached_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(item_id) DO UPDATE SET
                body = excluded.body,
                body_text = excluded.body_text,
                body_html = excluded.body_html,
                author_login = excluded.author_login,
                milestone_title = excluded.milestone_title,
                json_payload = excluded.json_payload,
                cached_at = excluded.cached_at",
            params![
                item_id,
                body,
                body_text,
                body_html,
                author_login,
                milestone_title,
                json_payload,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        self.get_github_issue_cache(item_id)
    }

    pub fn get_github_issue_cache(&self, item_id: &str) -> Result<GitHubIssueCache> {
        let sql = format!(
            "SELECT {GITHUB_ISSUE_CACHE_COLUMNS} FROM github_issue_cache WHERE item_id = ?1"
        );
        self.conn
            .query_row(&sql, params![item_id], Self::row_to_github_issue_cache)
            .with_context(|| format!("failed to fetch GitHub issue cache for item '{item_id}'"))
    }

    #[expect(clippy::too_many_arguments, reason = "PR cache mirrors GitHub payload")]
    pub fn upsert_github_pr_cache(
        &self,
        item_id: &str,
        body: Option<&str>,
        body_text: Option<&str>,
        body_html: Option<&str>,
        base_ref: Option<&str>,
        head_ref: Option<&str>,
        merge_state_status: Option<&str>,
        review_decision: Option<&str>,
        is_draft: bool,
        json_payload: Option<&str>,
    ) -> Result<GitHubPrCache> {
        self.conn.execute(
            "INSERT INTO github_pr_cache (
                item_id, body, body_text, body_html, base_ref, head_ref, merge_state_status,
                review_decision, is_draft, json_payload, cached_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(item_id) DO UPDATE SET
                body = excluded.body,
                body_text = excluded.body_text,
                body_html = excluded.body_html,
                base_ref = excluded.base_ref,
                head_ref = excluded.head_ref,
                merge_state_status = excluded.merge_state_status,
                review_decision = excluded.review_decision,
                is_draft = excluded.is_draft,
                json_payload = excluded.json_payload,
                cached_at = excluded.cached_at",
            params![
                item_id,
                body,
                body_text,
                body_html,
                base_ref,
                head_ref,
                merge_state_status,
                review_decision,
                is_draft,
                json_payload,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        self.get_github_pr_cache(item_id)
    }

    pub fn get_github_pr_cache(&self, item_id: &str) -> Result<GitHubPrCache> {
        let sql =
            format!("SELECT {GITHUB_PR_CACHE_COLUMNS} FROM github_pr_cache WHERE item_id = ?1");
        self.conn
            .query_row(&sql, params![item_id], Self::row_to_github_pr_cache)
            .with_context(|| format!("failed to fetch GitHub PR cache for item '{item_id}'"))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "comment cache mirrors GitHub payload"
    )]
    pub fn upsert_github_comment_cache(
        &self,
        item_id: &str,
        github_comment_id: &str,
        author_login: Option<&str>,
        body: Option<&str>,
        body_text: Option<&str>,
        body_html: Option<&str>,
        created_at: &str,
        updated_at: Option<&str>,
        json_payload: Option<&str>,
    ) -> Result<GitHubCommentCache> {
        let existing_id: Option<String> = optional(self.conn.query_row(
            "SELECT id FROM github_comment_cache WHERE github_comment_id = ?1",
            params![github_comment_id],
            |row| row.get(0),
        ))?;
        let id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        self.conn.execute(
            "INSERT INTO github_comment_cache (
                id, item_id, github_comment_id, author_login, body, body_text, body_html,
                created_at, updated_at, json_payload
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(github_comment_id) DO UPDATE SET
                item_id = excluded.item_id,
                author_login = excluded.author_login,
                body = excluded.body,
                body_text = excluded.body_text,
                body_html = excluded.body_html,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at,
                json_payload = excluded.json_payload",
            params![
                id,
                item_id,
                github_comment_id,
                author_login,
                body,
                body_text,
                body_html,
                created_at,
                updated_at,
                json_payload
            ],
        )?;
        self.get_github_comment_cache(&id)
    }

    pub fn get_github_comment_cache(&self, id: &str) -> Result<GitHubCommentCache> {
        let sql = format!(
            "SELECT {GITHUB_COMMENT_CACHE_COLUMNS} FROM github_comment_cache WHERE id = ?1"
        );
        self.conn
            .query_row(&sql, params![id], Self::row_to_github_comment_cache)
            .with_context(|| format!("failed to fetch GitHub comment cache '{id}'"))
    }

    pub fn list_github_comments_for_item(&self, item_id: &str) -> Result<Vec<GitHubCommentCache>> {
        let sql = format!(
            "SELECT {GITHUB_COMMENT_CACHE_COLUMNS} FROM github_comment_cache WHERE item_id = ?1 ORDER BY created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![item_id], Self::row_to_github_comment_cache)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "review cache mirrors GitHub payload"
    )]
    pub fn upsert_github_review_cache(
        &self,
        item_id: &str,
        github_review_id: &str,
        state: &str,
        commit_id: Option<&str>,
        author_login: Option<&str>,
        body: Option<&str>,
        body_text: Option<&str>,
        body_html: Option<&str>,
        submitted_at: Option<&str>,
        json_payload: Option<&str>,
    ) -> Result<GitHubReviewCache> {
        let existing_id: Option<String> = optional(self.conn.query_row(
            "SELECT id FROM github_review_cache WHERE github_review_id = ?1",
            params![github_review_id],
            |row| row.get(0),
        ))?;
        let id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        self.conn.execute(
            "INSERT INTO github_review_cache (
                id, item_id, github_review_id, state, commit_id, author_login, body, body_text,
                body_html, submitted_at, json_payload
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(github_review_id) DO UPDATE SET
                item_id = excluded.item_id,
                state = excluded.state,
                commit_id = excluded.commit_id,
                author_login = excluded.author_login,
                body = excluded.body,
                body_text = excluded.body_text,
                body_html = excluded.body_html,
                submitted_at = excluded.submitted_at,
                json_payload = excluded.json_payload",
            params![
                id,
                item_id,
                github_review_id,
                state,
                commit_id,
                author_login,
                body,
                body_text,
                body_html,
                submitted_at,
                json_payload
            ],
        )?;
        self.get_github_review_cache(&id)
    }

    pub fn get_github_review_cache(&self, id: &str) -> Result<GitHubReviewCache> {
        let sql =
            format!("SELECT {GITHUB_REVIEW_CACHE_COLUMNS} FROM github_review_cache WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_github_review_cache)
            .with_context(|| format!("failed to fetch GitHub review cache '{id}'"))
    }

    pub fn list_github_reviews_for_item(&self, item_id: &str) -> Result<Vec<GitHubReviewCache>> {
        let sql = format!(
            "SELECT {GITHUB_REVIEW_CACHE_COLUMNS} FROM github_review_cache WHERE item_id = ?1 ORDER BY submitted_at, github_review_id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![item_id], Self::row_to_github_review_cache)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "review comment cache mirrors GitHub payload"
    )]
    pub fn upsert_github_review_comment_cache(
        &self,
        review_id: Option<&str>,
        item_id: &str,
        github_comment_id: &str,
        author_login: Option<&str>,
        path: Option<&str>,
        line: Option<i64>,
        side: Option<&str>,
        start_line: Option<i64>,
        diff_hunk: Option<&str>,
        in_reply_to_id: Option<&str>,
        body: Option<&str>,
        body_text: Option<&str>,
        body_html: Option<&str>,
        created_at: &str,
        updated_at: Option<&str>,
        json_payload: Option<&str>,
    ) -> Result<GitHubReviewCommentCache> {
        let existing_id: Option<String> = optional(self.conn.query_row(
            "SELECT id FROM github_review_comment_cache WHERE github_comment_id = ?1",
            params![github_comment_id],
            |row| row.get(0),
        ))?;
        let id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        self.conn.execute(
            "INSERT INTO github_review_comment_cache (
                id, review_id, item_id, github_comment_id, author_login, path, line, side,
                start_line, diff_hunk, in_reply_to_id, body, body_text, body_html, created_at,
                updated_at, json_payload
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
            ON CONFLICT(github_comment_id) DO UPDATE SET
                review_id = excluded.review_id,
                item_id = excluded.item_id,
                author_login = excluded.author_login,
                path = excluded.path,
                line = excluded.line,
                side = excluded.side,
                start_line = excluded.start_line,
                diff_hunk = excluded.diff_hunk,
                in_reply_to_id = excluded.in_reply_to_id,
                body = excluded.body,
                body_text = excluded.body_text,
                body_html = excluded.body_html,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at,
                json_payload = excluded.json_payload",
            params![
                id,
                review_id,
                item_id,
                github_comment_id,
                author_login,
                path,
                line,
                side,
                start_line,
                diff_hunk,
                in_reply_to_id,
                body,
                body_text,
                body_html,
                created_at,
                updated_at,
                json_payload
            ],
        )?;
        self.get_github_review_comment_cache(&id)
    }

    pub fn get_github_review_comment_cache(&self, id: &str) -> Result<GitHubReviewCommentCache> {
        let sql = format!(
            "SELECT {GITHUB_REVIEW_COMMENT_CACHE_COLUMNS} FROM github_review_comment_cache WHERE id = ?1"
        );
        self.conn
            .query_row(&sql, params![id], Self::row_to_github_review_comment_cache)
            .with_context(|| format!("failed to fetch GitHub review comment cache '{id}'"))
    }

    pub fn list_github_review_comments_for_item(
        &self,
        item_id: &str,
    ) -> Result<Vec<GitHubReviewCommentCache>> {
        let sql = format!(
            "SELECT {GITHUB_REVIEW_COMMENT_CACHE_COLUMNS} FROM github_review_comment_cache WHERE item_id = ?1 ORDER BY created_at, github_comment_id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![item_id], Self::row_to_github_review_comment_cache)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    fn row_to_github_repo(row: &rusqlite::Row<'_>) -> rusqlite::Result<GitHubRepoCache> {
        Ok(GitHubRepoCache {
            id: row.get(0)?,
            project_id: row.get(1)?,
            owner: row.get(2)?,
            name: row.get(3)?,
            full_name: row.get(4)?,
            repo_url: row.get(5)?,
            default_branch: row.get(6)?,
            synced_at: row.get(7)?,
            created_at: row.get(8)?,
        })
    }

    fn row_to_github_project_v2(row: &rusqlite::Row<'_>) -> rusqlite::Result<GitHubProjectV2Cache> {
        Ok(GitHubProjectV2Cache {
            id: row.get(0)?,
            repo_id: row.get(1)?,
            project_number: row.get(2)?,
            title: row.get(3)?,
            url: row.get(4)?,
            node_id: row.get(5)?,
            synced_at: row.get(6)?,
            created_at: row.get(7)?,
        })
    }

    fn row_to_github_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<GitHubItem> {
        let kind_str: String = row.get(5)?;
        let id: String = row.get(0)?;
        let assignees_json: String = row.get(10)?;
        let labels_json: String = row.get(11)?;
        let label_colors_json: String = row.get(12)?;
        let project_values_json: String = row.get(13)?;
        let kind = kind_str.parse().unwrap_or_else(|_| {
            warn!(github_item_id = %id, raw = %kind_str, "unknown GitHub item kind in DB, defaulting to Issue");
            GitHubItemKind::Issue
        });
        Ok(GitHubItem {
            id,
            repo_id: row.get(1)?,
            project_v2_id: row.get(2)?,
            node_id: row.get(3)?,
            number: row.get(4)?,
            kind,
            title: row.get(6)?,
            state: row.get(7)?,
            url: row.get(8)?,
            body_text: row.get(9)?,
            assignee_logins: serde_json::from_str(&assignees_json).unwrap_or_default(),
            label_names: serde_json::from_str(&labels_json).unwrap_or_default(),
            label_colors: serde_json::from_str(&label_colors_json)
                .unwrap_or_else(|_| serde_json::json!({})),
            project_field_values: serde_json::from_str(&project_values_json)
                .unwrap_or_else(|_| serde_json::json!({})),
            base_ref: row.get(14)?,
            head_ref: row.get(15)?,
            github_updated_at: row.get(16)?,
            synced_at: row.get(17)?,
            created_at: row.get(18)?,
            updated_at: row.get(19)?,
            linked_pr_number: row.get(20)?,
            linked_pr_item_id: row.get(21)?,
        })
    }

    fn row_to_github_issue_cache(row: &rusqlite::Row<'_>) -> rusqlite::Result<GitHubIssueCache> {
        Ok(GitHubIssueCache {
            item_id: row.get(0)?,
            body: row.get(1)?,
            body_text: row.get(2)?,
            body_html: row.get(3)?,
            author_login: row.get(4)?,
            milestone_title: row.get(5)?,
            json_payload: row.get(6)?,
            cached_at: row.get(7)?,
        })
    }

    fn row_to_github_pr_cache(row: &rusqlite::Row<'_>) -> rusqlite::Result<GitHubPrCache> {
        Ok(GitHubPrCache {
            item_id: row.get(0)?,
            body: row.get(1)?,
            body_text: row.get(2)?,
            body_html: row.get(3)?,
            base_ref: row.get(4)?,
            head_ref: row.get(5)?,
            merge_state_status: row.get(6)?,
            review_decision: row.get(7)?,
            is_draft: row.get(8)?,
            json_payload: row.get(9)?,
            cached_at: row.get(10)?,
        })
    }

    fn row_to_github_comment_cache(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<GitHubCommentCache> {
        Ok(GitHubCommentCache {
            id: row.get(0)?,
            item_id: row.get(1)?,
            github_comment_id: row.get(2)?,
            author_login: row.get(3)?,
            body: row.get(4)?,
            body_text: row.get(5)?,
            body_html: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
            json_payload: row.get(9)?,
        })
    }

    fn row_to_github_review_cache(row: &rusqlite::Row<'_>) -> rusqlite::Result<GitHubReviewCache> {
        Ok(GitHubReviewCache {
            id: row.get(0)?,
            item_id: row.get(1)?,
            github_review_id: row.get(2)?,
            state: row.get(3)?,
            commit_id: row.get(4)?,
            author_login: row.get(5)?,
            body: row.get(6)?,
            body_text: row.get(7)?,
            body_html: row.get(8)?,
            submitted_at: row.get(9)?,
            json_payload: row.get(10)?,
        })
    }

    fn row_to_github_review_comment_cache(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<GitHubReviewCommentCache> {
        Ok(GitHubReviewCommentCache {
            id: row.get(0)?,
            review_id: row.get(1)?,
            item_id: row.get(2)?,
            github_comment_id: row.get(3)?,
            author_login: row.get(4)?,
            path: row.get(5)?,
            line: row.get(6)?,
            side: row.get(7)?,
            start_line: row.get(8)?,
            diff_hunk: row.get(9)?,
            in_reply_to_id: row.get(10)?,
            body: row.get(11)?,
            body_text: row.get(12)?,
            body_html: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
            json_payload: row.get(16)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::store::{GitHubItemKind, Store};

    #[test]
    fn github_cache_round_trip() {
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
        let item = store
            .upsert_github_item(
                &repo.id,
                None,
                Some("node123"),
                42,
                GitHubItemKind::Issue,
                "Issue title",
                "OPEN",
                "https://github.com/owner/repo/issues/42",
                Some("Issue text"),
                &[String::from("harsha")],
                &[String::from("bug")],
                &serde_json::json!({}),
                &serde_json::json!({"Status":"Todo"}),
                None,
                None,
                Some("2026-03-16T00:00:00Z"),
            )
            .unwrap();
        let issue = store
            .upsert_github_issue_cache(
                &item.id,
                Some("**body**"),
                Some("body"),
                Some("<p>body</p>"),
                Some("harsha"),
                Some("Sprint 1"),
                None,
            )
            .unwrap();

        assert_eq!(store.list_github_repos().unwrap().len(), 1);
        assert_eq!(store.list_github_items_for_repo(&repo.id).unwrap().len(), 1);
        assert_eq!(issue.item_id, item.id);
    }
}
