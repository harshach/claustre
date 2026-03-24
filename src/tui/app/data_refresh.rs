use std::sync::atomic::Ordering;

use anyhow::Result;
use serde_json::Value;

use crate::store::{
    ClaudeStatus, GitHubItemKind, Project, Task, TaskStatus, Thread, WorkflowRunStatus,
    WorkflowStageStatus,
};

use super::{
    App, MyTaskItem, ReviewQueueItem, ReviewQueueTab, SelectedThreadContext, SettingsSection,
    SidebarItem, ToastStyle, WorkbenchView, build_project_summaries,
};

impl App {
    pub(crate) fn sidebar_nav_count() -> usize {
        WorkbenchView::ALL.len()
    }

    pub(crate) fn selected_sidebar_item(&self) -> SidebarItem {
        if self.sidebar_cursor < Self::sidebar_nav_count() {
            SidebarItem::Navigation(WorkbenchView::ALL[self.sidebar_cursor])
        } else {
            SidebarItem::Repository(self.sidebar_cursor - Self::sidebar_nav_count())
        }
    }

    pub(crate) fn sync_sidebar_cursor(&mut self) {
        let nav_count = Self::sidebar_nav_count();
        let total = nav_count + self.projects.len();
        if total == 0 {
            self.sidebar_cursor = 0;
            return;
        }

        self.sidebar_cursor = self.sidebar_cursor.min(total.saturating_sub(1));

        if self.projects.is_empty() && self.sidebar_cursor >= nav_count {
            self.sidebar_cursor = nav_count.saturating_sub(1);
        }
    }

    /// Auto-teardown sessions for completed push-mode tasks.
    ///
    /// Push-mode tasks don't create PRs, so the PR merge poller never triggers cleanup.
    /// This method detects sessions whose push-mode task is `Done` and tears them down.
    pub fn maybe_teardown_push_mode_sessions(&mut self) {
        if self.session_op_in_progress {
            return;
        }
        let sessions = self
            .store
            .sessions_needing_push_mode_cleanup()
            .unwrap_or_default();
        if let Some((session_id, task_title)) = sessions.into_iter().next() {
            self.spawn_teardown_session(session_id);
            self.show_toast(
                format!("Push completed — session closed: {task_title}"),
                ToastStyle::Success,
            );
        }
    }

    pub fn refresh_data(&mut self) -> Result<()> {
        self.github_status =
            crate::github_app::local_status(&self.config.github_app).unwrap_or_default();
        self.projects = self.store.list_projects()?;

        if let Some(project) = self.projects.get(self.project_index) {
            self.sessions = self.store.list_sessions_for_project(&project.id)?;
            self.tasks = self.store.list_tasks_for_project(&project.id)?;
            self.threads = self.store.list_threads_for_project(&project.id)?;
            self.github_projects_v2 = self
                .store
                .get_github_repo_for_project(&project.id)?
                .map(|repo| self.store.list_github_projects_v2_for_repo(&repo.id))
                .transpose()?
                .unwrap_or_default();
        } else {
            self.sessions.clear();
            self.tasks.clear();
            self.threads.clear();
            self.github_projects_v2.clear();
        }

        // Detect Working → InReview transitions and show a toast (once per task)
        let new_review_title = self.tasks.iter().find_map(|t| {
            (t.status == TaskStatus::InReview
                && self.prev_task_statuses.get(&t.id) == Some(&TaskStatus::Working)
                && !self.notified_in_review.contains(&t.id))
            .then(|| (t.id.clone(), t.title.clone()))
        });
        // Clear notified set for tasks that left InReview (e.g. marked done)
        self.notified_in_review.retain(|id| {
            self.tasks
                .iter()
                .any(|t| t.id == *id && t.status == TaskStatus::InReview)
        });
        self.prev_task_statuses = self
            .tasks
            .iter()
            .map(|t| (t.id.clone(), t.status))
            .collect();
        if let Some((id, title)) = new_review_title {
            self.notified_in_review.insert(id.clone());
            self.show_toast(format!("Ready for review: {title}"), ToastStyle::Success);

            // Spawn review loop pane if the task has review_loop enabled
            self.maybe_spawn_review_loop(&id);
        }

        // Clean up spawned set for tasks that are done or no longer exist
        self.review_loop_spawned.retain(|id| {
            self.tasks
                .iter()
                .any(|t| t.id == *id && t.status == TaskStatus::InReview)
        });

        // Pre-fetch sidebar summaries for all projects
        self.project_summaries = build_project_summaries(&self.store, &self.projects);

        // Refresh cached project stats for the selected project
        self.project_stats = self
            .selected_project()
            .and_then(|p| self.store.project_stats(&p.id).ok());

        // Pre-fetch subtask counts for visible tasks
        self.subtask_counts.clear();
        for task in &self.tasks {
            if let Ok(counts) = self.store.subtask_count(&task.id)
                && counts.0 > 0
            {
                self.subtask_counts.insert(task.id.clone(), counts);
            }
        }

        // Recompute browse caches after data changes
        self.recompute_visible_tasks();
        self.recompute_my_task_items()?;
        self.recompute_review_items()?;
        self.refresh_board_selected_comments();
        self.refresh_review_selected_context();

        // Clamp indices
        if self.project_index >= self.projects.len() {
            self.project_index = self.projects.len().saturating_sub(1);
        }
        self.sync_sidebar_cursor();
        self.settings_section_index = self
            .settings_section_index
            .min(SettingsSection::ALL.len().saturating_sub(1));
        if self.github_installation_index >= self.github_installations.len()
            && !self.github_installations.is_empty()
        {
            self.github_installation_index = self.github_installations.len().saturating_sub(1);
        } else if self.github_installations.is_empty() {
            self.github_installation_index = 0;
        }
        if self.github_project_index >= self.github_projects_v2.len()
            && !self.github_projects_v2.is_empty()
        {
            self.github_project_index = self.github_projects_v2.len().saturating_sub(1);
        } else if self.github_projects_v2.is_empty() {
            self.github_project_index = 0;
        }
        let visible_count = if self.workbench_view == WorkbenchView::MyTasks {
            if self.uses_github_my_tasks() {
                self.visible_my_task_count()
            } else {
                self.visible_task_count()
            }
        } else {
            self.visible_task_count()
        };
        if self.task_index >= visible_count && visible_count > 0 {
            self.task_index = visible_count.saturating_sub(1);
        } else if visible_count == 0 {
            self.task_index = 0;
        }
        let review_count = self.review_items().len();
        if self.review_index >= review_count && review_count > 0 {
            self.review_index = review_count.saturating_sub(1);
        } else if review_count == 0 {
            self.review_index = 0;
        }
        if self.thread_index >= self.threads.len() && !self.threads.is_empty() {
            self.thread_index = self.threads.len().saturating_sub(1);
        } else if self.threads.is_empty() {
            self.thread_index = 0;
        }

        // Refresh subtasks for selected task
        if self.workbench_view == WorkbenchView::MyTasks {
            self.subtasks.clear();
        } else if let Some(task) = self.visible_task_at(self.task_index) {
            self.subtasks = self
                .store
                .list_subtasks_for_task(&task.id)
                .unwrap_or_default();
        } else {
            self.subtasks.clear();
        }
        if self.subtask_index >= self.subtasks.len() {
            self.subtask_index = self.subtasks.len().saturating_sub(1);
        } else if self.subtasks.is_empty() {
            self.subtask_index = 0;
        }

        // Preserve API-sourced usage values before DB state overwrites them
        // (the DB doesn't store usage percentages — they come from the cache file)
        let prev_usage = (
            self.rate_limit_state.usage_5h_pct,
            self.rate_limit_state.usage_7d_pct,
            self.rate_limit_state.reset_5h.clone(),
            self.rate_limit_state.reset_7d.clone(),
        );

        // Refresh rate limit state and auto-clear if expired
        if let Ok(state) = self.store.get_rate_limit_state() {
            if state.is_rate_limited
                && let Some(ref reset_at) = state.reset_at
                && let Ok(reset_time) = chrono::DateTime::parse_from_rfc3339(reset_at)
                && chrono::Utc::now() > reset_time
            {
                let _ = self.store.clear_rate_limit();
                self.rate_limit_state = self.store.get_rate_limit_state().unwrap_or_default();
            } else {
                self.rate_limit_state = state;
            }
        }

        // Read usage percentages from the Claude API cache
        self.refresh_usage_from_api_cache();

        // Restore previous usage values if cache didn't provide new ones
        let (pct_hourly, pct_daily, reset_hourly, reset_daily) = prev_usage;
        if self.rate_limit_state.usage_5h_pct.is_none() {
            self.rate_limit_state.usage_5h_pct = pct_hourly;
        }
        if self.rate_limit_state.usage_7d_pct.is_none() {
            self.rate_limit_state.usage_7d_pct = pct_daily;
        }
        if self.rate_limit_state.reset_5h.is_none() {
            self.rate_limit_state.reset_5h = reset_hourly;
        }
        if self.rate_limit_state.reset_7d.is_none() {
            self.rate_limit_state.reset_7d = reset_daily;
        }

        // Refresh external sessions list
        self.external_sessions = self.store.list_external_sessions().unwrap_or_default();

        Ok(())
    }

    /// Read usage percentages from ~/.claude/statusline-cache.json (shared with statusline).
    /// Always uses cached data if present. Triggers a background refresh when stale
    /// or when the cache lacks usage percentage data.
    pub(super) fn refresh_usage_from_api_cache(&mut self) {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let cache_path = home.join(".claude/statusline-cache.json");

        let mut cache_fresh = false;
        let mut has_pct_data = false;

        if let Ok(content) = std::fs::read_to_string(&cache_path)
            && let Ok(cache) = serde_json::from_str::<serde_json::Value>(&content)
        {
            // Only overwrite pct values when the cache actually has them.
            // The JS statusline script omits pct fields when the API doesn't
            // return utilization, and overwriting with None would blank the bars.
            if let Some(pct) = cache["data"]["pct5h"].as_f64() {
                self.rate_limit_state.usage_5h_pct = Some(pct);
                has_pct_data = true;
            }
            if let Some(pct) = cache["data"]["pct7d"].as_f64() {
                self.rate_limit_state.usage_7d_pct = Some(pct);
                has_pct_data = true;
            }
            if let Some(reset) = cache["data"]["reset5h"].as_str() {
                self.rate_limit_state.reset_5h = Some(reset.to_string());
            }
            if let Some(reset) = cache["data"]["reset7d"].as_str() {
                self.rate_limit_state.reset_7d = Some(reset.to_string());
            }

            let timestamp = cache["timestamp"].as_f64().unwrap_or(0.0);
            #[expect(
                clippy::cast_precision_loss,
                reason = "millisecond epoch fits in f64 for decades"
            )]
            let age_ms = (chrono::Utc::now().timestamp_millis() as f64) - timestamp;
            cache_fresh = age_ms < 120_000.0;
        }

        // Fetch when cache is stale OR when it exists but lacks percentage data.
        // The JS statusline sometimes writes the cache without pct fields (e.g.
        // when the API omits utilization), so timestamp alone isn't sufficient.
        let needs_fetch = !cache_fresh || !has_pct_data;
        if needs_fetch && !self.usage_fetch_in_progress.load(Ordering::SeqCst) {
            self.spawn_usage_fetch();
        }
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.projects.get(self.project_index)
    }

    /// Pre-fetch comments for the currently selected board item.
    pub(crate) fn refresh_board_selected_comments(&mut self) {
        self.board_selected_comments.clear();
        if let Some(item) = self.selected_board_issue()
            && let Ok(comments) = self
                .store
                .list_github_comments_for_item(&item.github_item_id)
        {
            self.board_selected_comments = comments;
        }
    }

    /// Pre-fetch comments for the currently selected My Task item.
    pub(crate) fn refresh_my_task_selected_comments(&mut self) {
        self.my_task_selected_comments.clear();
        if let Some(item) = self.selected_my_task_item()
            && let Ok(comments) = self
                .store
                .list_github_comments_for_item(&item.github_item.id)
        {
            self.my_task_selected_comments = comments;
        }
    }

    /// Pre-fetch comments, reviews, and review comments for the currently
    /// selected review queue item (used by the inspector and review drawer).
    pub(crate) fn refresh_review_selected_context(&mut self) {
        self.review_selected_comments.clear();
        self.review_selected_reviews.clear();
        self.review_selected_review_comments.clear();
        let Some(item) = self.selected_review_item() else {
            return;
        };
        let item_id = item.github_item.id.clone();
        if let Ok(comments) = self.store.list_github_comments_for_item(&item_id) {
            self.review_selected_comments = comments;
        }
        if let Ok(reviews) = self.store.list_github_reviews_for_item(&item_id) {
            self.review_selected_reviews = reviews;
        }
        if let Ok(review_comments) = self.store.list_github_review_comments_for_item(&item_id) {
            self.review_selected_review_comments = review_comments;
        }
    }

    pub(crate) fn recompute_review_items(&mut self) -> Result<()> {
        self.review_authored_items.clear();
        self.review_requested_items.clear();

        let github_repos = self.store.list_github_repos()?;
        if github_repos.is_empty() {
            return Ok(());
        }
        let Some(current_login) = self.current_github_login() else {
            return Ok(());
        };

        for repo in github_repos {
            for item in self.store.list_github_items_for_repo(&repo.id)? {
                if item.kind != GitHubItemKind::PullRequest
                    || !item.state.eq_ignore_ascii_case("open")
                {
                    continue;
                }
                let Ok(pr_cache) = self.store.get_github_pr_cache(&item.id) else {
                    continue;
                };
                let issue_cache = self.store.get_github_issue_cache(&item.id).ok();
                let author_login = issue_cache
                    .as_ref()
                    .and_then(|issue| issue.author_login.clone())
                    .or_else(|| parse_author_login(pr_cache.json_payload.as_deref()));
                let requested_reviewer_logins =
                    parse_requested_reviewer_logins(pr_cache.json_payload.as_deref());
                let comment_count = self
                    .store
                    .list_github_comments_for_item(&item.id)
                    .map_or(0, |comments| comments.len());
                let review_count = self
                    .store
                    .list_github_reviews_for_item(&item.id)
                    .map_or(0, |reviews| reviews.len());
                let review_comment_count = self
                    .store
                    .list_github_review_comments_for_item(&item.id)
                    .map_or(0, |comments| comments.len());

                let review_item = ReviewQueueItem {
                    github_item: item,
                    pr_cache,
                    issue_cache,
                    author_login,
                    requested_reviewer_logins,
                    comment_count,
                    review_count,
                    review_comment_count,
                };

                if review_item
                    .author_login
                    .as_deref()
                    .is_some_and(|login| login.eq_ignore_ascii_case(current_login.as_str()))
                {
                    self.review_authored_items.push(review_item);
                    continue;
                }

                if review_item_needs_my_review(&review_item, current_login.as_str()) {
                    self.review_requested_items.push(review_item);
                }
            }
        }

        self.review_authored_items.sort_by(|left, right| {
            right
                .github_item
                .github_updated_at
                .cmp(&left.github_item.github_updated_at)
                .then(right.github_item.number.cmp(&left.github_item.number))
        });
        self.review_requested_items.sort_by(|left, right| {
            review_priority(left.pr_cache.review_decision.as_ref())
                .cmp(&review_priority(right.pr_cache.review_decision.as_ref()))
                .then(
                    right
                        .github_item
                        .github_updated_at
                        .cmp(&left.github_item.github_updated_at),
                )
                .then(right.github_item.number.cmp(&left.github_item.number))
        });

        Ok(())
    }

    pub(crate) fn recompute_my_task_items(&mut self) -> Result<()> {
        self.my_task_items.clear();
        self.cached_my_task_indices.clear();

        let github_repos = self.store.list_github_repos()?;
        if github_repos.is_empty() {
            return Ok(());
        }
        let Some(current_login) = self.current_github_login() else {
            return Ok(());
        };

        for repo in github_repos {
            for github_item in self.store.list_github_items_for_repo(&repo.id)? {
                if !github_item.state.eq_ignore_ascii_case("open") {
                    continue;
                }
                if !github_item
                    .assignee_logins
                    .iter()
                    .any(|login| login.eq_ignore_ascii_case(current_login.as_str()))
                {
                    continue;
                }

                let issue_cache = self.store.get_github_issue_cache(&github_item.id).ok();
                let pr_cache = self.store.get_github_pr_cache(&github_item.id).ok();
                let linked_thread = self.store.find_thread_for_github_item(&github_item.id)?;
                let linked_pr = self
                    .store
                    .find_linked_pr_for_issue(&github_item.id)
                    .ok()
                    .flatten();

                self.my_task_items.push(MyTaskItem {
                    repo: repo.clone(),
                    github_item,
                    issue_cache,
                    pr_cache,
                    linked_thread,
                    linked_pr,
                });
            }
        }

        self.my_task_items.sort_by(|left, right| {
            right
                .github_item
                .github_updated_at
                .cmp(&left.github_item.github_updated_at)
                .then(right.github_item.number.cmp(&left.github_item.number))
        });

        let filter_lower = self.task_filter.to_lowercase();
        let mut indices: Vec<usize> = self
            .my_task_items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                if filter_lower.is_empty() {
                    return true;
                }

                item.github_item
                    .title
                    .to_lowercase()
                    .contains(&filter_lower)
                    || item.github_item.number.to_string().contains(&filter_lower)
                    || item
                        .issue_cache
                        .as_ref()
                        .and_then(|cache| cache.body_text.as_deref().or(cache.body.as_deref()))
                        .is_some_and(|body| body.to_lowercase().contains(&filter_lower))
                    || item
                        .pr_cache
                        .as_ref()
                        .and_then(|cache| cache.body_text.as_deref().or(cache.body.as_deref()))
                        .is_some_and(|body| body.to_lowercase().contains(&filter_lower))
                    || item
                        .github_item
                        .label_names
                        .iter()
                        .any(|label| label.to_lowercase().contains(&filter_lower))
                    || item
                        .github_item
                        .assignee_logins
                        .iter()
                        .any(|login| login.to_lowercase().contains(&filter_lower))
            })
            .map(|(index, _)| index)
            .collect();
        indices.sort_unstable();
        self.cached_my_task_indices = indices;

        Ok(())
    }

    pub(crate) fn current_github_login(&self) -> Option<String> {
        self.github_status
            .user_login
            .clone()
            .or_else(|| self.github_status.gh_user_login.clone())
    }

    pub(crate) fn uses_github_my_tasks(&self) -> bool {
        !self.my_task_items.is_empty()
            || (self.current_github_login().is_some()
                && self
                    .store
                    .list_github_repos()
                    .is_ok_and(|repos| !repos.is_empty()))
    }

    pub(crate) fn active_session_id(&self) -> Option<&str> {
        match self.tabs.get(self.active_tab) {
            Some(super::Tab::Session { session_id, .. }) => Some(session_id.as_str()),
            _ => None,
        }
    }

    pub(crate) fn active_session_thread(&self) -> Option<Thread> {
        let session_id = self.active_session_id()?;
        self.threads
            .iter()
            .find(|thread| thread.session_id.as_deref() == Some(session_id))
            .cloned()
            .or_else(|| {
                self.store
                    .find_thread_for_session(session_id)
                    .ok()
                    .flatten()
            })
    }

    pub(crate) fn review_items(&self) -> &[ReviewQueueItem] {
        match self.review_queue_tab {
            ReviewQueueTab::Authored => &self.review_authored_items,
            ReviewQueueTab::NeedsReview => &self.review_requested_items,
        }
    }

    pub(crate) fn selected_review_item(&self) -> Option<&ReviewQueueItem> {
        self.review_items().get(self.review_index)
    }

    pub fn selected_task(&self) -> Option<&Task> {
        match self.workbench_view {
            WorkbenchView::MyTasks => {
                if self.uses_github_my_tasks() {
                    None
                } else {
                    self.visible_task_at(self.task_index)
                }
            }
            WorkbenchView::Reviews => self.selected_review_item().and_then(|review_item| {
                self.tasks.iter().find(|task| {
                    self.threads.iter().any(|thread| {
                        thread.github_item_id.as_deref()
                            == Some(review_item.github_item.id.as_str())
                            && thread.task_id.as_deref() == Some(task.id.as_str())
                    })
                })
            }),
            WorkbenchView::Threads => self
                .selected_thread()
                .and_then(|thread| thread.task_id.as_deref())
                .and_then(|task_id| self.tasks.iter().find(|task| task.id == task_id)),
            _ => self.visible_task_at(self.task_index),
        }
    }

    pub fn selected_thread(&self) -> Option<&Thread> {
        match self.workbench_view {
            WorkbenchView::Threads => self.threads.get(self.thread_index),
            WorkbenchView::MyTasks => {
                if self.uses_github_my_tasks() {
                    self.selected_my_task_item().and_then(|item| {
                        item.linked_thread.as_ref().or_else(|| {
                            self.threads.iter().find(|thread| {
                                thread.github_item_id.as_deref()
                                    == Some(item.github_item.id.as_str())
                            })
                        })
                    })
                } else {
                    self.selected_task().and_then(|task| {
                        self.threads
                            .iter()
                            .find(|thread| thread.task_id.as_deref() == Some(task.id.as_str()))
                    })
                }
            }
            WorkbenchView::Reviews => self.selected_review_item().and_then(|review_item| {
                self.threads.iter().find(|thread| {
                    thread.github_item_id.as_deref() == Some(review_item.github_item.id.as_str())
                })
            }),
            _ => self.selected_task().and_then(|task| {
                self.threads
                    .iter()
                    .find(|thread| thread.task_id.as_deref() == Some(task.id.as_str()))
            }),
        }
    }

    fn build_thread_context(&self, thread: Thread) -> Option<SelectedThreadContext> {
        let messages = self.store.list_thread_messages(&thread.id).ok()?;
        let runs = self.store.list_thread_runs(&thread.id).ok()?;
        let attachments = self.store.list_thread_attachments(&thread.id).ok()?;
        let runtime_state = self
            .store
            .get_thread_runtime_state(&thread.id)
            .ok()
            .flatten();
        let runtime_services = runtime_state
            .as_ref()
            .and_then(|state| serde_json::from_str(&state.services_json).ok())
            .unwrap_or_default();

        let workflow_run = thread
            .workflow_run_id
            .as_deref()
            .and_then(|run_id| self.store.get_workflow_run(run_id).ok());
        let workflow_def = workflow_run
            .as_ref()
            .and_then(|run| self.store.get_workflow_def(&run.workflow_def_id).ok());
        let workflow_stages = workflow_run
            .as_ref()
            .and_then(|run| self.store.list_workflow_stage_runs(&run.id).ok())
            .unwrap_or_default();
        let workflow_stage = workflow_run
            .as_ref()
            .and_then(|run| {
                run.current_stage.as_deref().and_then(|current_stage| {
                    workflow_stages
                        .iter()
                        .find(|stage| stage.stage_name == current_stage)
                        .cloned()
                })
            })
            .or_else(|| workflow_stages.last().cloned());
        let workflow_artifact_count = workflow_run
            .as_ref()
            .and_then(|run| self.store.list_workflow_artifacts(&run.id).ok())
            .map_or(0, |artifacts| artifacts.len());

        let github_item = thread
            .github_item_id
            .as_deref()
            .and_then(|item_id| self.store.get_github_item(item_id).ok());
        let issue_cache = github_item
            .as_ref()
            .and_then(|item| self.store.get_github_issue_cache(&item.id).ok());
        let pr_cache = github_item
            .as_ref()
            .and_then(|item| self.store.get_github_pr_cache(&item.id).ok());
        let comment_count = github_item
            .as_ref()
            .and_then(|item| self.store.list_github_comments_for_item(&item.id).ok())
            .map_or(0, |comments| comments.len());
        let review_count = github_item
            .as_ref()
            .and_then(|item| self.store.list_github_reviews_for_item(&item.id).ok())
            .map_or(0, |reviews| reviews.len());
        let review_comment_count = github_item
            .as_ref()
            .and_then(|item| {
                self.store
                    .list_github_review_comments_for_item(&item.id)
                    .ok()
            })
            .map_or(0, |comments| comments.len());

        let knowledge_card_count = self
            .store
            .list_knowledge_cards_for_project(&thread.project_id)
            .ok()
            .map_or(0, |cards| {
                cards
                    .into_iter()
                    .filter(|card| {
                        card.thread_id.as_deref() == Some(thread.id.as_str())
                            || card.github_item_id.as_deref() == thread.github_item_id.as_deref()
                    })
                    .count()
            });
        let knowledge_draft_count = self
            .store
            .list_knowledge_drafts_for_project(&thread.project_id)
            .ok()
            .map_or(0, |drafts| {
                drafts
                    .into_iter()
                    .filter(|draft| {
                        draft.thread_id.as_deref() == Some(thread.id.as_str())
                            || draft.github_item_id.as_deref() == thread.github_item_id.as_deref()
                    })
                    .count()
            });

        let message_count = messages.len();
        let run_count = runs.len();
        let attachment_count = attachments.len();
        let latest_run = runs.last().cloned();

        Some(SelectedThreadContext {
            thread,
            messages,
            attachments,
            runs,
            latest_run,
            message_count,
            run_count,
            attachment_count,
            runtime_state,
            runtime_services,
            workflow_def,
            workflow_run,
            workflow_stage,
            workflow_stages,
            workflow_artifact_count,
            github_item,
            issue_cache,
            pr_cache,
            comment_count,
            review_count,
            review_comment_count,
            knowledge_card_count,
            knowledge_draft_count,
        })
    }

    pub(crate) fn active_session_thread_context(&self) -> Option<SelectedThreadContext> {
        let thread = self.active_session_thread()?;
        self.build_thread_context(thread)
    }

    /// Refresh the cached thread context for the active session tab.
    /// Called from slow tick (1s) so the chat panel has fresh data without
    /// running ~15 DB queries on every 60fps frame.
    pub(crate) fn refresh_session_thread_context(&mut self) {
        let session_id = self.active_session_id().map(str::to_string);
        if session_id != self.cached_session_thread_ctx_session_id {
            // Session changed — clear cache and rebuild
            self.cached_session_thread_ctx = None;
            self.cached_session_thread_ctx_session_id = session_id.clone();
        }
        if session_id.is_some() {
            self.cached_session_thread_ctx = self.active_session_thread_context();
        }
    }

    pub fn selected_thread_context(&self) -> Option<SelectedThreadContext> {
        let thread = self.selected_thread()?.clone();
        self.build_thread_context(thread)
    }

    /// Recompute the cached visible task indices. Must be called after any change
    /// to `self.tasks`, `self.task_filter`, or task sort order.
    pub fn recompute_visible_tasks(&mut self) {
        let filter_lower = self.task_filter.to_lowercase();
        let mut indices: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                filter_lower.is_empty() || t.title.to_lowercase().contains(&filter_lower)
            })
            .map(|(i, _)| i)
            .collect();
        indices.sort_by(|&a, &b| {
            self.tasks[a]
                .status
                .sort_priority()
                .cmp(&self.tasks[b].status.sort_priority())
                .then_with(|| self.tasks[a].sort_order.cmp(&self.tasks[b].sort_order))
        });
        self.cached_visible_indices = indices;
    }

    /// Returns all tasks for the selected project, optionally filtered
    /// by the current search term (`task_filter`). Uses case-insensitive title matching.
    /// Tasks are sorted by status priority, then by `sort_order` within each status group.
    /// Done tasks appear last.
    pub fn visible_tasks(&self) -> Vec<&Task> {
        self.cached_visible_indices
            .iter()
            .map(|&i| &self.tasks[i])
            .collect()
    }

    pub fn visible_my_tasks(&self) -> Vec<&MyTaskItem> {
        self.cached_my_task_indices
            .iter()
            .map(|&i| &self.my_task_items[i])
            .collect()
    }

    /// Number of visible tasks (avoids allocating a Vec).
    pub fn visible_task_count(&self) -> usize {
        self.cached_visible_indices.len()
    }

    pub fn visible_my_task_count(&self) -> usize {
        self.cached_my_task_indices.len()
    }

    /// Get a single visible task by display index (avoids allocating a Vec).
    pub fn visible_task_at(&self, index: usize) -> Option<&Task> {
        self.cached_visible_indices
            .get(index)
            .map(|&i| &self.tasks[i])
    }

    pub fn visible_my_task_at(&self, index: usize) -> Option<&MyTaskItem> {
        self.cached_my_task_indices
            .get(index)
            .map(|&i| &self.my_task_items[i])
    }

    pub(crate) fn selected_my_task_item(&self) -> Option<&MyTaskItem> {
        if self.workbench_view != WorkbenchView::MyTasks || !self.uses_github_my_tasks() {
            return None;
        }

        self.visible_my_task_at(self.task_index)
    }

    pub(crate) fn visible_github_project_indices(&self) -> Vec<usize> {
        let query = self.input_buffer.trim().to_lowercase();
        if query.is_empty() {
            return (0..self.github_projects_v2.len()).collect();
        }

        self.github_projects_v2
            .iter()
            .enumerate()
            .filter(|(_, project)| {
                project.title.to_lowercase().contains(&query)
                    || project.project_number.to_string().contains(&query)
                    || project.display_label().to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Orchestrate workflow stage advancement for all active thread sessions.
    ///
    /// For each thread with a running workflow and a live session tab:
    /// - If session is idle and the current stage prompt has NOT been injected yet,
    ///   inject the stage prompt into the Claude PTY.
    /// - If session is idle and the prompt WAS already injected (Claude finished),
    ///   advance the workflow to the next stage.
    pub fn maybe_advance_workflow_stages(&mut self) {
        // Collect candidate threads: those with workflow runs and active session tabs.
        let candidates: Vec<(Thread, String)> = self
            .threads
            .iter()
            .filter_map(|thread| {
                let run_id = thread.workflow_run_id.as_ref()?;
                // Must have a live session tab
                let session_id = thread.session_id.as_deref()?;
                let has_tab = self.tabs.iter().any(|tab| {
                    matches!(tab, super::Tab::Session { session_id: sid, .. } if sid == session_id)
                });
                if !has_tab {
                    return None;
                }
                Some((thread.clone(), run_id.clone()))
            })
            .collect();

        for (thread, run_id) in candidates {
            let Some(session_id) = thread.session_id.as_deref() else {
                continue;
            };

            // Check if Claude is idle or has exited — either way, the current
            // stage's turn is over and the workflow can advance.
            let db_idle = self
                .sessions
                .iter()
                .find(|s| s.id == session_id)
                .is_some_and(|s| s.claude_status == ClaudeStatus::Idle);
            let pty_idle = self.pty_idle_sessions.contains(session_id);
            let claude_pane_exited = self.tabs.iter().any(|tab| {
                matches!(tab, super::Tab::Session { session_id: sid, terminals, .. }
                    if sid == session_id
                    && terminals
                        .terminal(terminals.claude_pane_id)
                        .is_some_and(|t| t.exited()))
            });
            if !db_idle && !pty_idle && !claude_pane_exited {
                continue;
            }

            // Load workflow state
            let Ok(workflow_run) = self.store.get_workflow_run(&run_id) else {
                continue;
            };
            if workflow_run.status != WorkflowRunStatus::Running {
                continue;
            }
            let Ok(stages) = self.store.list_workflow_stage_runs(&run_id) else {
                continue;
            };
            let Some(current_stage) = stages
                .iter()
                .find(|s| s.status == WorkflowStageStatus::Running)
            else {
                continue;
            };

            // Check if Claude has been working since the prompt was injected.
            // If not, mark it as seen-working now (if session IS working).
            if let Some(seen_working) = self.workflow_stage_injected.get_mut(&current_stage.id) {
                if !*seen_working {
                    let is_working = self
                        .sessions
                        .iter()
                        .find(|s| s.id == session_id)
                        .is_some_and(|s| s.claude_status == ClaudeStatus::Working);
                    if is_working {
                        *seen_working = true;
                    }
                    // Don't advance yet — haven't confirmed Claude processed the prompt
                    continue;
                }
            }

            if self.workflow_stage_injected.contains_key(&current_stage.id) {
                // Prompt was injected AND Claude was seen working AND is now idle → advance
                self.workflow_stage_injected.remove(&current_stage.id);

                // Capture a stage output summary for the chat timeline.
                // Try: 1) thread messages, 2) JSONL cache, 3) PTY screen buffer.
                let summary = self
                    .store
                    .list_thread_messages(&thread.id)
                    .ok()
                    .and_then(|messages| {
                        messages
                            .iter()
                            .rev()
                            .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
                            .map(|m| m.content.clone())
                    })
                    .or_else(|| {
                        // Fallback 1: extract from JSONL conversation cache
                        let sid = thread.session_id.as_deref()?;
                        let cache = self
                            .conversation_cache
                            .as_ref()
                            .filter(|c| c.session_id == sid && !c.entries.is_empty())?;
                        let mut parts = Vec::new();
                        for entry in cache.entries.iter().rev().take(20) {
                            if let crate::conversation::ConversationEntry::AssistantText {
                                text,
                                ..
                            } = entry
                            {
                                if !text.trim().is_empty() {
                                    parts.push(text.clone());
                                    if parts.len() >= 3 {
                                        break;
                                    }
                                }
                            }
                        }
                        if parts.is_empty() {
                            return None;
                        }
                        parts.reverse();
                        Some(parts.join("\n\n"))
                    })
                    .or_else(|| {
                        // Fallback 2: capture from the PTY screen buffer (Codex etc.)
                        let tab = self.tabs.iter().find(|tab| {
                            matches!(tab, super::Tab::Session { session_id: sid, .. } if sid == session_id)
                        })?;
                        let super::Tab::Session { terminals, .. } = tab else {
                            return None;
                        };
                        terminals.with_claude_live_screen(|screen| {
                            let rows = screen.size().0;
                            let cols = screen.size().1;
                            let mut lines: Vec<String> = Vec::new();
                            // Read all non-empty lines from the screen
                            for row in 0..rows {
                                let line = screen
                                    .contents_between(row, 0, row, cols)
                                    .trim()
                                    .to_string();
                                if !line.is_empty() {
                                    lines.push(line);
                                }
                            }
                            // Skip the last few lines (prompt, status bar)
                            if lines.len() > 4 {
                                lines.truncate(lines.len() - 3);
                            }
                            if lines.is_empty() {
                                return None;
                            }
                            Some(lines.join("\n"))
                        })?
                    });

                if let Some(summary) = summary {
                    // Truncate to first 2000 chars to keep DB lean
                    let summary = if summary.len() > 2000 {
                        format!("{}...", &summary[..1997])
                    } else {
                        summary
                    };
                    let _ = self.store.update_workflow_stage_run_status(
                        &current_stage.id,
                        WorkflowStageStatus::Running, // still running, resume_workflow_run will complete it
                        current_stage.gate_state.as_deref(),
                        Some(&summary),
                    );
                }

                match crate::workflows::resume_workflow_run(&self.store, &run_id) {
                    Ok(bundle) => {
                        if bundle.run.status == WorkflowRunStatus::Completed {
                            let name = bundle.definition.name;
                            self.show_toast(
                                format!("Workflow completed: {name}"),
                                ToastStyle::Success,
                            );
                        } else if bundle.run.status == WorkflowRunStatus::WaitingApproval {
                            let stage_name =
                                bundle.run.current_stage.as_deref().unwrap_or("unknown");
                            self.show_toast(
                                format!("Workflow paused: approve '{stage_name}' to continue (g)"),
                                ToastStyle::Info,
                            );
                        }
                        // Refresh to pick up new state immediately
                        let _ = self.refresh_data();
                    }
                    Err(err) => {
                        tracing::warn!("Failed to advance workflow: {err:#}");
                    }
                }
            } else {
                // Stage prompt not yet injected → inject it now
                let prompt = match current_stage.prompt.as_deref() {
                    Some(p) if !p.is_empty() => p.to_string(),
                    _ => {
                        // Gate-only or prompt-less stage — auto-advance
                        let _ = crate::workflows::resume_workflow_run(&self.store, &run_id);
                        let _ = self.refresh_data();
                        continue;
                    }
                };

                // Try sending directly; if Claude pane is dead, restart via shell
                let sent = self.send_prompt_to_live_thread_session(&thread, &prompt)
                    || self.restart_claude_with_message(&thread, &prompt, &[]);
                if sent {
                    // false = haven't seen Claude working yet since injection
                    self.workflow_stage_injected
                        .insert(current_stage.id.clone(), false);

                    // Record a system message for the stage only after
                    // successful injection — otherwise this runs every tick
                    // when the send fails, spamming the conversation.
                    let latest_run_id = self
                        .store
                        .list_thread_runs(&thread.id)
                        .ok()
                        .and_then(|runs| runs.into_iter().last().map(|r| r.id));
                    let _ = self.store.create_thread_message(
                        &thread.id,
                        latest_run_id.as_deref(),
                        "system",
                        &format!("[Workflow] Stage: {}", current_stage.stage_name),
                        &[],
                    );

                    // Mark session as working
                    let _ = self.store.update_session_status(
                        session_id,
                        ClaudeStatus::Working,
                        &format!("Stage: {}", current_stage.stage_name),
                    );
                }
            }
        }
    }

    /// Refresh the JSONL conversation cache for the active session tab.
    ///
    /// If the active tab is not a session in Conversation view, or the JSONL
    /// file hasn't changed since the last parse, this is a no-op.
    pub(crate) fn refresh_conversation_cache(&mut self) {
        // Only refresh when viewing a session tab
        let Some(session_id) = self.active_session_id().map(str::to_string) else {
            return;
        };

        // Look up the session to get worktree_path and claude_session_id
        let (worktree_path, claude_session_id) =
            if let Some(s) = self.sessions.iter().find(|s| s.id == session_id) {
                (s.worktree_path.clone(), s.claude_session_id.clone())
            } else if let Ok(s) = self.store.get_session(&session_id) {
                // `s` is owned, so move fields directly instead of cloning.
                (s.worktree_path, s.claude_session_id)
            } else {
                return;
            };

        // Check if cache already exists for the same session and file hasn't grown.
        // Use file size instead of mtime — mtime has 1-second resolution on macOS
        // and misses writes within the same second.
        if let Some(ref cache) = self.conversation_cache
            && cache.session_id == session_id
            && let Some(ref jsonl_path) = cache.jsonl_path
        {
            let current_size = std::fs::metadata(jsonl_path).ok().map(|m| m.len());
            if current_size == Some(cache.file_offset) {
                return; // File hasn't grown since last parse
            }
        }

        // Resolve the JSONL path
        let jsonl_path =
            crate::conversation::resolve_jsonl_path(&worktree_path, claude_session_id.as_deref());

        let Some(ref path) = jsonl_path else {
            // No JSONL file found — store cache with None path so we retry later
            self.conversation_cache = Some(super::ConversationCache {
                session_id,
                entries: Vec::new(),
                file_offset: 0,
                file_mtime: None,
                jsonl_path: None,
            });
            return;
        };

        let current_mtime = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());

        // Determine the offset to resume from
        let existing_offset = self
            .conversation_cache
            .as_ref()
            .filter(|c| {
                c.session_id == session_id && c.jsonl_path.as_deref() == Some(path.as_path())
            })
            .map_or(0, |c| c.file_offset);

        // Parse incremental entries
        let Ok((new_entries, new_offset)) = crate::conversation::parse_jsonl(path, existing_offset)
        else {
            return;
        };

        if let Some(ref mut cache) = self.conversation_cache
            && cache.session_id == session_id
            && cache.jsonl_path.as_deref() == Some(path.as_path())
        {
            // Append to existing cache
            if !new_entries.is_empty() {
                cache.entries.extend(new_entries);
                // Invalidate rendered line cache — new data arrived
                self.cached_chat_lines = None;
            }
            cache.file_offset = new_offset;
            cache.file_mtime = current_mtime;
        } else {
            // New cache
            self.conversation_cache = Some(super::ConversationCache {
                session_id,
                entries: new_entries,
                file_offset: new_offset,
                file_mtime: current_mtime,
                jsonl_path: jsonl_path.clone(),
            });
            self.cached_chat_lines = None;
        }

        // Detect quick-reply choices from the latest assistant message
        if let Some(ref cache) = self.conversation_cache {
            self.quick_reply_choices = detect_quick_reply_choices(&cache.entries);
        }
    }
}

/// Detect numbered choices at the end of the last assistant message.
///
/// Looks for patterns like:
/// ```text
/// 1. Merge from main first
/// 2. Add tests for the Flyway backfill
/// 3. Both
/// ```
///
/// Returns the list of choice texts (e.g. ["Merge from main first", "Add tests...", "Both"]).
fn detect_quick_reply_choices(entries: &[crate::conversation::ConversationEntry]) -> Vec<String> {
    // Find the last AssistantText entry (skip ToolUse/ToolResult/TurnEnd at the end)
    let last_text = entries.iter().rev().find_map(|entry| {
        if let crate::conversation::ConversationEntry::AssistantText { text, .. } = entry {
            Some(text.as_str())
        } else {
            None
        }
    });

    let Some(text) = last_text else {
        return Vec::new();
    };

    // Check if the last entry is a UserMessage — if so, the user already replied
    let last_is_user = entries.iter().rev().any(|entry| {
        matches!(
            entry,
            crate::conversation::ConversationEntry::UserMessage { .. }
                | crate::conversation::ConversationEntry::ToolUse { .. }
        )
    }) && entries
        .iter()
        .rev()
        .take_while(|entry| {
            !matches!(
                entry,
                crate::conversation::ConversationEntry::AssistantText { .. }
            )
        })
        .any(|entry| {
            matches!(
                entry,
                crate::conversation::ConversationEntry::UserMessage { .. }
            )
        });

    if last_is_user {
        return Vec::new();
    }

    // Parse numbered options from the end of the text
    let lines: Vec<&str> = text.lines().collect();
    let mut choices = Vec::new();

    // Scan from the bottom for numbered lines: "1. ...", "2. ...", etc.
    for line in lines.iter().rev() {
        let trimmed = line.trim();
        // Match patterns: "1. text", "2. text", etc.
        if let Some(rest) = trimmed
            .strip_prefix(|c: char| c.is_ascii_digit())
            .and_then(|s| s.strip_prefix(". "))
        {
            if !rest.is_empty() {
                choices.push(rest.to_string());
            }
        } else if !choices.is_empty() {
            // Hit a non-numbered line after collecting some choices — stop
            break;
        }
        // Skip empty lines between choices and the question
    }

    choices.reverse();

    // Only return if we found at least 2 choices
    if choices.len() >= 2 {
        choices
    } else {
        Vec::new()
    }
}

fn parse_payload(json_payload: Option<&str>) -> Option<Value> {
    json_payload.and_then(|payload| serde_json::from_str(payload).ok())
}

fn parse_author_login(json_payload: Option<&str>) -> Option<String> {
    let payload = parse_payload(json_payload)?;
    payload
        .get("author")
        .and_then(|author| author.get("login"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            payload
                .get("user")
                .and_then(|user| user.get("login"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn parse_requested_reviewer_logins(json_payload: Option<&str>) -> Vec<String> {
    let Some(payload) = parse_payload(json_payload) else {
        return Vec::new();
    };

    if let Some(reviewers) = payload.get("requested_reviewers").and_then(Value::as_array) {
        return reviewers
            .iter()
            .filter_map(|reviewer| reviewer.get("login").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
    }

    payload
        .get("reviewRequests")
        .and_then(|review_requests| review_requests.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| {
                    node.get("requestedReviewer")
                        .and_then(|reviewer| reviewer.get("login"))
                        .and_then(Value::as_str)
                })
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn review_item_needs_my_review(item: &ReviewQueueItem, current_login: &str) -> bool {
    let explicitly_requested = item
        .requested_reviewer_logins
        .iter()
        .any(|login| login.eq_ignore_ascii_case(current_login));
    let assigned_to_me = item
        .github_item
        .assignee_logins
        .iter()
        .any(|login| login.eq_ignore_ascii_case(current_login));
    let review_required = item
        .pr_cache
        .review_decision
        .as_deref()
        .is_some_and(|decision| decision.eq_ignore_ascii_case("review_required"));

    explicitly_requested || (assigned_to_me && review_required)
}

fn review_priority(review_decision: Option<&String>) -> u8 {
    match review_decision.map(String::as_str) {
        Some(decision) if decision.eq_ignore_ascii_case("changes_requested") => 0,
        Some(decision) if decision.eq_ignore_ascii_case("review_required") => 1,
        Some(decision) if decision.eq_ignore_ascii_case("approved") => 3,
        Some(_) | None => 2,
    }
}
