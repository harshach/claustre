use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::store::TaskStatus;

use super::{
    App, GitStatsResult, PrPollResult, PrStatus, SessionOpResult, ToastStyle, check_pr_status,
    compute_pane_sizes_for_resize, fetch_and_cache_usage, generate_ai_title, parse_git_diff_stat,
};

impl App {
    pub(super) fn spawn_github_auth(&mut self) {
        if self.github_auth_in_progress.load(Ordering::SeqCst) {
            return;
        }

        let config = self.config.github_app.clone();
        let use_app_auth = crate::github_app::app_auth_available(&config);
        let flag = self.github_auth_in_progress.clone();
        let tx = self.github_auth_tx.clone();
        self.github_auth_error = None;
        flag.store(true, Ordering::SeqCst);

        std::thread::spawn(move || {
            if use_app_auth {
                let result = crate::github_app::authenticate_device_flow(&config, |event| {
                    let _ = tx.send(event);
                });
                if let Err(error) = result {
                    let _ = tx.send(crate::github_app::GitHubAuthMessage::Failed(
                        error.to_string(),
                    ));
                }
            } else {
                match crate::github_app::github_cli_connect() {
                    Ok(login) => {
                        let _ = tx.send(crate::github_app::GitHubAuthMessage::CliSuccess(login));
                    }
                    Err(error) => {
                        let _ = tx.send(crate::github_app::GitHubAuthMessage::Failed(
                            error.to_string(),
                        ));
                    }
                }
            }
            flag.store(false, Ordering::SeqCst);
        });
    }

    pub(super) fn spawn_github_installations_fetch(&mut self) {
        if self.github_installations_in_progress.load(Ordering::SeqCst) {
            return;
        }

        let config = self.config.github_app.clone();
        let flag = self.github_installations_in_progress.clone();
        let tx = self.github_installations_tx.clone();
        flag.store(true, Ordering::SeqCst);

        std::thread::spawn(move || {
            let result = (|| -> Result<Vec<crate::github_app::GitHubInstallation>> {
                let token = crate::github_app::ensure_access_token(&config)?
                    .ok_or_else(|| anyhow::anyhow!("GitHub App is not authenticated"))?;
                crate::github_app::list_installations(&token)
            })()
            .map_err(|error| error.to_string());
            let _ = tx.send(result);
            flag.store(false, Ordering::SeqCst);
        });
    }

    pub(super) fn poll_github_auth_results(&mut self) {
        while let Ok(message) = self.github_auth_rx.try_recv() {
            match message {
                crate::github_app::GitHubAuthMessage::Prompt(prompt) => {
                    self.github_auth_prompt = Some(prompt);
                    self.github_auth_error = None;
                }
                crate::github_app::GitHubAuthMessage::Success(session) => {
                    self.github_auth_prompt = None;
                    self.github_auth_error = None;
                    self.github_status = crate::github_app::local_status(&self.config.github_app)
                        .unwrap_or_default();
                    let login = session
                        .user_login
                        .as_deref()
                        .unwrap_or("unknown user")
                        .to_string();
                    self.show_toast(
                        format!("GitHub App authenticated as {login}"),
                        ToastStyle::Success,
                    );
                    self.spawn_github_installations_fetch();
                }
                crate::github_app::GitHubAuthMessage::CliSuccess(login) => {
                    self.github_auth_prompt = None;
                    self.github_auth_error = None;
                    self.github_status = crate::github_app::local_status(&self.config.github_app)
                        .unwrap_or_default();
                    self.show_toast(
                        format!(
                            "GitHub connected via CLI as {}",
                            login.as_deref().unwrap_or("unknown user")
                        ),
                        ToastStyle::Success,
                    );
                }
                crate::github_app::GitHubAuthMessage::Failed(error) => {
                    self.github_auth_error = Some(error.clone());
                    self.show_toast(format!("GitHub auth failed: {error}"), ToastStyle::Error);
                }
                crate::github_app::GitHubAuthMessage::Disconnected => {
                    self.github_auth_prompt = None;
                    self.github_auth_error = None;
                    self.github_status = crate::github_app::local_status(&self.config.github_app)
                        .unwrap_or_default();
                }
            }
        }
    }

    pub(super) fn poll_github_installation_results(&mut self) {
        while let Ok(result) = self.github_installations_rx.try_recv() {
            match result {
                Ok(mut installations) => {
                    installations.sort_by(|left, right| {
                        left.account
                            .login
                            .cmp(&right.account.login)
                            .then(left.id.cmp(&right.id))
                    });
                    self.github_installations = installations;
                    if let Some(default_installation) = self
                        .config
                        .github_app
                        .default_installation_id
                        .as_deref()
                        .and_then(|id| id.parse::<i64>().ok())
                        && let Some(index) = self
                            .github_installations
                            .iter()
                            .position(|installation| installation.id == default_installation)
                    {
                        self.github_installation_index = index;
                    } else if self.github_installation_index >= self.github_installations.len() {
                        self.github_installation_index =
                            self.github_installations.len().saturating_sub(1);
                    }
                    self.show_toast(
                        format!(
                            "Loaded {} GitHub installation(s)",
                            self.github_installations.len()
                        ),
                        ToastStyle::Success,
                    );
                }
                Err(error) => {
                    self.show_toast(
                        format!("Failed to load GitHub installations: {error}"),
                        ToastStyle::Error,
                    );
                }
            }
        }
    }

    /// Spawn a background thread to fetch usage from the Anthropic OAuth API
    /// and write the result to the shared cache file.
    pub(super) fn spawn_usage_fetch(&self) {
        let flag = self.usage_fetch_in_progress.clone();
        flag.store(true, Ordering::SeqCst);

        std::thread::spawn(move || {
            let _result = fetch_and_cache_usage();
            flag.store(false, Ordering::SeqCst);
        });
    }

    /// Spawn a background thread to check for updates and auto-install if available.
    #[allow(dead_code)]
    pub(super) fn spawn_update_check(&self) {
        if self.update_check_in_progress.load(Ordering::Relaxed) {
            return;
        }
        let flag = self.update_check_in_progress.clone();
        flag.store(true, Ordering::Relaxed);
        let tx = self.update_tx.clone();

        std::thread::spawn(move || {
            let result = crate::update::check_and_update();
            let _ = tx.send(result);
            flag.store(false, Ordering::Relaxed);
        });
    }

    /// Periodically re-check for updates (every 30 minutes).
    /// Skips if an update was already found or a check is in progress.
    /// Check if the system clipboard contains an image.
    /// Called on slow ticks (~1s) to keep `clipboard_has_image` current
    /// so the compose area can show an indicator.
    pub(super) fn check_clipboard_for_image(&mut self) {
        self.clipboard_has_image = arboard::Clipboard::new()
            .ok()
            .and_then(|mut cb| cb.get_image().ok())
            .is_some();
    }

    pub(super) fn maybe_poll_update_check(&mut self) {
        const UPDATE_POLL_INTERVAL: Duration = Duration::from_secs(30 * 60);

        if !self.config.auto_update {
            return;
        }
        if self.updated_version.is_some() || self.available_version.is_some() {
            return;
        }
        if self.last_update_check.elapsed() < UPDATE_POLL_INTERVAL {
            return;
        }
        self.last_update_check = std::time::Instant::now();
        self.spawn_update_check();
    }

    /// Drain update check results from the background thread.
    pub(super) fn poll_update_results(&mut self) {
        while let Ok(result) = self.update_rx.try_recv() {
            match result {
                crate::update::UpdateCheckResult::Updated { new_version } => {
                    self.updated_version = Some(new_version.clone());
                    self.show_toast(
                        format!("Updated to {new_version} — restart to apply"),
                        ToastStyle::Success,
                    );
                }
                crate::update::UpdateCheckResult::UpToDate => {}
                crate::update::UpdateCheckResult::Available {
                    new_version,
                    reason,
                } => {
                    self.available_version = Some(new_version);
                    self.show_toast(format!("Auto-update failed: {reason}"), ToastStyle::Error);
                }
                crate::update::UpdateCheckResult::Failed { reason } => {
                    self.show_toast(format!("Update check failed: {reason}"), ToastStyle::Error);
                }
            }
        }
    }

    /// Spawn a background thread to generate a title for a task via Claude Haiku.
    /// When the title is ready, it's sent through the channel and picked up on the next tick.
    pub(super) fn spawn_title_generation(&mut self, task_id: String, prompt: String) {
        self.pending_titles.insert(task_id.clone());
        let tx = self.title_tx.clone();
        std::thread::spawn(move || {
            let title = generate_ai_title(&prompt);
            let _ = tx.send((task_id, title));
        });
    }

    /// Drain background title results and update tasks in the DB.
    /// If any completed titles belong to autonomous tasks awaiting launch, launch them now.
    pub(super) fn poll_title_results(&mut self) -> Result<()> {
        while let Ok((task_id, title)) = self.title_rx.try_recv() {
            self.pending_titles.remove(&task_id);
            self.store.update_task_title(&task_id, &title)?;

            if let Some(project_id) = self.pending_auto_launch.remove(&task_id) {
                let task = self.store.get_task(&task_id)?;
                let branch_name = task
                    .branch
                    .as_deref()
                    .filter(|b| !b.is_empty())
                    .map_or_else(
                        || crate::session::generate_branch_name(&task.title),
                        String::from,
                    );
                let base_branch = task
                    .base
                    .as_deref()
                    .filter(|b| !b.is_empty())
                    .map(String::from);
                self.spawn_create_session(project_id, branch_name, task, base_branch);
            }
        }
        Ok(())
    }

    /// Poll PR status for all `in_review` and `conflict` tasks that have a PR URL.
    /// Detects merges, new conflicts, and conflict resolution.
    /// Spawns a background thread every ~15 seconds.
    pub(super) fn maybe_poll_pr_merges(&mut self) {
        const PR_POLL_INTERVAL: Duration = Duration::from_secs(15);

        if self.last_pr_poll.elapsed() < PR_POLL_INTERVAL {
            return;
        }
        self.last_pr_poll = std::time::Instant::now();

        if self.pr_poll_in_progress.load(Ordering::SeqCst) {
            return;
        }

        let Ok(tasks) = self.store.list_in_review_tasks_with_pr() else {
            return;
        };
        if tasks.is_empty() {
            return;
        }

        // Collect task info for the background thread
        let check_list: Vec<_> = tasks
            .into_iter()
            .filter_map(|t| {
                let url = t.pr_url?;
                Some((t.id, t.session_id, url, t.title, t.status, t.ci_status))
            })
            .collect();

        if check_list.is_empty() {
            return;
        }

        let flag = self.pr_poll_in_progress.clone();
        flag.store(true, Ordering::SeqCst);
        let tx = self.pr_poll_tx.clone();

        std::thread::spawn(move || {
            for (task_id, session_id, pr_url, title, task_status, current_ci) in check_list {
                let pr_status = check_pr_status(&pr_url);

                // Derive CI status from the PR check result
                let new_ci = match pr_status {
                    PrStatus::CiRunning => Some(crate::store::CiStatus::Running),
                    PrStatus::CiPassed => Some(crate::store::CiStatus::Passed),
                    PrStatus::CiFailed => Some(crate::store::CiStatus::Failed),
                    _ => None,
                };

                // Send ci_status update if it changed
                if let Some(ci) = new_ci
                    && new_ci != current_ci
                {
                    let _ = tx.send(PrPollResult::CiStatusChanged {
                        task_id: task_id.clone(),
                        ci_status: ci,
                    });
                }

                // Handle task status transitions.
                // For `working` tasks, only track ci_status changes (handled above)
                // — don't transition the task status since the user is actively
                // working on fixes. Task status transitions only apply to
                // in_review / conflict / ci_failed tasks.
                if task_status != TaskStatus::Working {
                    match pr_status {
                        PrStatus::Merged => {
                            let _ = tx.send(PrPollResult::Merged {
                                task_id,
                                session_id,
                                task_title: title,
                            });
                        }
                        PrStatus::Conflicting if task_status != TaskStatus::Conflict => {
                            let _ = tx.send(PrPollResult::Conflict {
                                task_id,
                                task_title: title,
                            });
                        }
                        PrStatus::CiFailed if task_status != TaskStatus::CiFailed => {
                            let _ = tx.send(PrPollResult::CiFailed {
                                task_id,
                                task_title: title,
                            });
                        }
                        PrStatus::Open | PrStatus::CiRunning | PrStatus::CiPassed
                            if task_status == TaskStatus::Conflict =>
                        {
                            let _ = tx.send(PrPollResult::ConflictResolved {
                                task_id,
                                task_title: title,
                            });
                        }
                        PrStatus::Open | PrStatus::CiRunning | PrStatus::CiPassed
                            if task_status == TaskStatus::CiFailed =>
                        {
                            let _ = tx.send(PrPollResult::CiRecovered {
                                task_id,
                                task_title: title,
                            });
                        }
                        _ => {}
                    }
                }
            }
            flag.store(false, Ordering::SeqCst);
        });
    }

    /// Drain PR poll results and handle merges, conflicts, and conflict resolution.
    ///
    /// PR poll results come from a background thread that snapshots task state
    /// before checking GitHub. By the time we process results the task status
    /// may have changed (e.g. user resumed, killed the session, or another
    /// poll result already transitioned the task). We use `try_update_task_status`
    /// so stale results are silently skipped instead of crashing the TUI.
    pub(super) fn poll_pr_merge_results(&mut self) -> Result<()> {
        while let Ok(result) = self.pr_poll_rx.try_recv() {
            match result {
                PrPollResult::Merged {
                    task_id,
                    session_id,
                    task_title,
                } => {
                    if self
                        .store
                        .try_update_task_status(&task_id, crate::store::TaskStatus::Done)?
                    {
                        if let Some(ref sid) = session_id {
                            self.spawn_teardown_session(sid.clone());
                        }
                        self.show_toast(
                            format!("PR merged — task done: {task_title}"),
                            ToastStyle::Success,
                        );
                    }
                }
                PrPollResult::Conflict {
                    task_id,
                    task_title,
                } => {
                    if self
                        .store
                        .try_update_task_status(&task_id, crate::store::TaskStatus::Conflict)?
                    {
                        self.show_toast(
                            format!("PR has conflicts: {task_title}"),
                            ToastStyle::Error,
                        );
                    }
                }
                PrPollResult::ConflictResolved {
                    task_id,
                    task_title,
                } => {
                    if self
                        .store
                        .try_update_task_status(&task_id, crate::store::TaskStatus::InReview)?
                    {
                        self.show_toast(
                            format!("Conflicts resolved: {task_title}"),
                            ToastStyle::Success,
                        );
                    }
                }
                PrPollResult::CiFailed {
                    task_id,
                    task_title,
                } => {
                    if self
                        .store
                        .try_update_task_status(&task_id, crate::store::TaskStatus::CiFailed)?
                    {
                        self.show_toast(
                            format!("CI checks failed: {task_title}"),
                            ToastStyle::Error,
                        );
                    }
                }
                PrPollResult::CiRecovered {
                    task_id,
                    task_title,
                } => {
                    if self
                        .store
                        .try_update_task_status(&task_id, crate::store::TaskStatus::InReview)?
                    {
                        // Clear the stale ci_status so the dashboard no longer shows "CI failed"
                        self.store.update_task_ci_status(&task_id, None)?;
                        self.show_toast(
                            format!("CI checks passing: {task_title}"),
                            ToastStyle::Success,
                        );
                    }
                }
                PrPollResult::CiStatusChanged { task_id, ci_status } => {
                    self.store
                        .update_task_ci_status(&task_id, Some(ci_status))?;
                }
            }
        }
        Ok(())
    }

    /// Poll git diff stats for all active sessions every ~5 seconds.
    pub(super) fn maybe_poll_git_stats(&mut self) {
        const GIT_STATS_INTERVAL: Duration = Duration::from_secs(5);

        if self.last_git_stats_poll.elapsed() < GIT_STATS_INTERVAL {
            return;
        }
        self.last_git_stats_poll = std::time::Instant::now();

        if self.git_stats_in_progress.load(Ordering::SeqCst) {
            return;
        }

        // Collect all active sessions with their worktree paths and default branches
        let worktrees: Vec<(String, String, String)> = self
            .project_summaries
            .iter()
            .flat_map(|(_, summary)| {
                let branch = summary.default_branch.clone();
                summary
                    .active_sessions
                    .iter()
                    .map(move |s| (s.id.clone(), s.worktree_path.clone(), branch.clone()))
            })
            .collect();

        if worktrees.is_empty() {
            return;
        }

        let flag = self.git_stats_in_progress.clone();
        flag.store(true, Ordering::SeqCst);
        let tx = self.git_stats_tx.clone();

        std::thread::spawn(move || {
            for (session_id, worktree_path, default_branch) in worktrees {
                if let Some(stats) = parse_git_diff_stat(&worktree_path, &default_branch) {
                    let _ = tx.send(GitStatsResult {
                        session_id,
                        files_changed: stats.0,
                        lines_added: stats.1,
                        lines_removed: stats.2,
                    });
                }
            }
            flag.store(false, Ordering::SeqCst);
        });
    }

    /// Drain git stats results and persist to the database.
    pub(super) fn poll_git_stats_results(&mut self) {
        while let Ok(result) = self.git_stats_rx.try_recv() {
            let _ = self.store.update_session_git_stats(
                &result.session_id,
                result.files_changed,
                result.lines_added,
                result.lines_removed,
            );
        }
    }

    /// Spawn a background scan for external Claude sessions every 60s.
    pub(super) fn maybe_scan_external_sessions(&mut self) {
        const SCAN_INTERVAL: Duration = Duration::from_secs(60);

        if self.last_scan.elapsed() < SCAN_INTERVAL {
            return;
        }
        self.last_scan = std::time::Instant::now();

        if self.scanner_in_progress.load(Ordering::SeqCst) {
            return;
        }

        let project_paths = self.store.list_all_project_repo_paths().unwrap_or_default();
        let known = self.store.external_session_scan_info().unwrap_or_default();

        let flag = self.scanner_in_progress.clone();
        flag.store(true, Ordering::SeqCst);
        let tx = self.scanner_tx.clone();

        std::thread::spawn(move || {
            if let Ok(result) = crate::scanner::scan_external_sessions(&project_paths, &known) {
                let _ = tx.send(result);
            }
            flag.store(false, Ordering::SeqCst);
        });
    }

    /// Drain scanner results, upsert new data, prune stale entries, and refresh the list.
    pub(super) fn poll_scanner_results(&mut self) {
        while let Ok(result) = self.scanner_rx.try_recv() {
            for session in &result.updated {
                let _ = self.store.upsert_external_session(session);
            }
            // Remove sessions that are no longer active (file not modified recently)
            let _ = self.store.prune_stale_external_sessions(&result.active_ids);
            // Refresh the in-memory list from DB
            self.external_sessions = self.store.list_external_sessions().unwrap_or_default();
        }
    }

    /// Drain background GitHub mutation results and update UI accordingly.
    pub(super) fn poll_github_mutations(&mut self) {
        while let Ok(result) = self.github_mutation_rx.try_recv() {
            match result {
                super::GitHubMutationResult::CommentCreated { message } => {
                    self.show_toast(message, super::ToastStyle::Success);
                    self.refresh_board_selected_comments();
                }
                super::GitHubMutationResult::StateChanged { message }
                | super::GitHubMutationResult::IssueCreated { message }
                | super::GitHubMutationResult::IssueEdited { message }
                | super::GitHubMutationResult::FieldUpdated { message } => {
                    self.show_toast(message, super::ToastStyle::Success);
                    // Use the non-blocking cache read instead of a full GitHub sync
                    // to avoid freezing the TUI on the main thread.
                    self.load_board_issues_from_cache();
                    self.refresh_board_selected_comments();
                }
                super::GitHubMutationResult::Error { message } => {
                    self.show_toast(message, super::ToastStyle::Error);
                }
            }
        }
    }

    /// Drain background session operation results, spawn PTYs for new sessions, and show toasts.
    pub(super) fn poll_session_ops(&mut self) {
        while let Ok(result) = self.session_op_rx.try_recv() {
            match result {
                SessionOpResult::Created(setup) => {
                    self.spawn_session_tab(*setup);
                }
                SessionOpResult::ThreadLaunched { result } => {
                    let crate::threads::LaunchThreadResult {
                        thread,
                        workflow: _,
                        session_setup,
                    } = *result;
                    let _ = self.refresh_data();

                    // Step 1: spawn a new session tab from the setup if available
                    if let Some(setup) = session_setup {
                        let sid = setup.session.id.clone();
                        self.spawn_session_tab(setup);
                        let _ = self.goto_session_tab(&sid);
                        self.active_tab = 0;
                    }

                    // Step 2: ensure a live session tab exists behind the scenes
                    // so the thread workspace can reopen terminal/editor on demand.
                    let fresh_sid = self
                        .store
                        .get_thread(&thread.id)
                        .ok()
                        .and_then(|t| t.session_id)
                        .or_else(|| thread.session_id.clone());
                    if let Some(ref sid) = fresh_sid {
                        let has_live_session = self
                            .tabs
                            .iter()
                            .any(|tab| matches!(tab, super::Tab::Session { session_id, .. } if session_id == sid));
                        if !has_live_session
                            && let Ok(session) = self.store.get_session(sid)
                            && session.closed_at.is_none()
                        {
                            let _ = self.restore_session_tab(&session);
                        }
                    }

                    // Step 3: land in the native thread workspace first.
                    let switched = self.select_thread_workspace(&thread.id);
                    if switched {
                        self.show_toast(
                            format!("Opened thread workspace via {}", thread.provider_kind),
                            ToastStyle::Success,
                        );
                    } else {
                        self.show_toast(
                            format!(
                                "Thread launched via {} (workspace selection failed — press Space+h to open Threads)",
                                thread.provider_kind
                            ),
                            ToastStyle::Info,
                        );
                    }
                }
                SessionOpResult::CreatedNoTask { message }
                | SessionOpResult::TornDown { message, .. } => {
                    self.show_toast(message, ToastStyle::Success);
                }
                SessionOpResult::Error { message } => {
                    tracing::error!("session op failed: {message}");
                    self.show_toast(message, ToastStyle::Error);
                    // Clear any pending relaunch — the operation failed
                    self.pending_relaunch = None;
                }
            }
            self.session_op_in_progress = false;
            let _ = self.refresh_data();
        }

        // If a teardown just completed and a relaunch is queued, launch the task now
        if !self.session_op_in_progress
            && let Some((task_id, project_id)) = self.pending_relaunch.take()
            && let Err(e) = self.launch_task(task_id, project_id)
        {
            self.show_toast(format!("Relaunch failed: {e}"), ToastStyle::Error);
        }
    }

    fn spawn_session_tab(&mut self, setup: crate::session::SessionSetup) {
        let term_size = crossterm::terminal::size().unwrap_or((80, 24));
        let cols = term_size.0;
        let rows = term_size.1.saturating_sub(2);

        let wrapped = setup.claude_cmd.unwrap_or_else(|| {
            crate::session::wrap_cmd_with_shell_fallback(vec!["claude".to_string()])
        });

        let agent_result: anyhow::Result<Box<dyn crate::pty::Terminal>> =
            spawn_session_host_terminal(
                &setup.session.id,
                &setup.worktree_path,
                &wrapped,
                rows,
                cols / 2,
            )
            .or_else(|host_err| {
                // Fallback to in-process EmbeddedTerminal if session-host fails
                eprintln!("session-host failed ({host_err}), falling back to embedded PTY");
                let mut cmd = portable_pty::CommandBuilder::new(&wrapped[0]);
                for arg in &wrapped[1..] {
                    cmd.arg(arg);
                }
                cmd.cwd(&setup.worktree_path);
                crate::pty::EmbeddedTerminal::spawn(cmd, rows, cols / 2)
                    .map(|term| Box::new(term) as Box<dyn crate::pty::Terminal>)
            });

        let terminals_result = match agent_result {
            Ok(agent) => {
                if let Some(ref layout_config) = self.config.layout {
                    crate::pty::SessionTerminals::from_layout(
                        agent,
                        &setup.worktree_path,
                        layout_config,
                        rows,
                        cols,
                    )
                } else {
                    let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
                    let mut shell_cmd = portable_pty::CommandBuilder::new(&shell_path);
                    shell_cmd.cwd(&setup.worktree_path);
                    crate::pty::EmbeddedTerminal::spawn(shell_cmd, rows, cols / 2).map(|shell| {
                        crate::pty::SessionTerminals::from_parts(shell, agent, &setup.worktree_path)
                    })
                }
            }
            Err(error) => Err(error),
        };

        match terminals_result {
            Ok(mut terminals) => {
                let sizes =
                    compute_pane_sizes_for_resize(&terminals.layout, term_size.0, term_size.1);
                let _ = terminals.resize_panes_with_clear(&sizes);
                self.add_session_tab(
                    setup.session.id.clone(),
                    Box::new(terminals),
                    setup.tab_label,
                );
            }
            Err(error) => {
                self.show_toast(format!("Session launch failed: {error}"), ToastStyle::Error);
            }
        }
    }

    /// Spawn a background thread to sync GitHub data for the selected project.
    pub(super) fn spawn_github_sync(&self) {
        if self.github_sync_in_progress.load(Ordering::SeqCst) {
            return;
        }
        let Some(project) = self.selected_project().cloned() else {
            return;
        };
        if !project.is_git_linked {
            return;
        }
        let flag = self.github_sync_in_progress.clone();
        flag.store(true, Ordering::SeqCst);
        let tx = self.github_sync_tx.clone();
        let db_path = crate::config::db_path().ok();
        let selected_project_hint = self.config.github_app.default_project_id.clone();

        std::thread::spawn(move || {
            let result = (|| -> Result<crate::github::GitHubProjectBoardSnapshot, String> {
                let path = db_path.ok_or_else(|| "cannot resolve db path".to_string())?;
                let store = crate::store::Store::open_at(&path).map_err(|e| format!("db: {e}"))?;
                // Full repo sync: issues, PRs, comments, Projects v2 items, linked PRs
                let _ = crate::github::sync_project_repo(&store, &project)
                    .map_err(|e| tracing::warn!("repo sync: {e:#}"));
                // Then load the board snapshot from the updated cache
                crate::github::load_project_board_from_cache(
                    &store,
                    &project,
                    selected_project_hint.as_deref(),
                )
                .map_err(|e| format!("{e:#}"))
            })();
            let _ = tx.send(result);
            flag.store(false, Ordering::SeqCst);
        });
    }

    /// Periodically sync GitHub data in the background (5 minutes).
    pub(super) fn maybe_poll_github_sync(&mut self) {
        const GITHUB_SYNC_INTERVAL: Duration = Duration::from_secs(5 * 60);

        if self.last_github_sync.elapsed() < GITHUB_SYNC_INTERVAL {
            return;
        }
        self.last_github_sync = std::time::Instant::now();

        // Only auto-sync for git-linked projects
        let Some(project) = self.selected_project() else {
            return;
        };
        if !project.is_git_linked {
            return;
        }

        self.spawn_github_sync();
    }

    /// Drain results from a background GitHub sync.
    pub(super) fn poll_github_sync_results(&mut self) {
        while let Ok(result) = self.github_sync_rx.try_recv() {
            let was_manual = self.github_sync_manual;
            self.github_sync_manual = false;
            match result {
                Ok(snapshot) => {
                    let selected_project_hint = self.config.github_app.default_project_id.clone();
                    self.apply_board_snapshot(snapshot, selected_project_hint.as_deref());
                    if was_manual {
                        self.show_toast("GitHub sync complete", ToastStyle::Success);
                    }
                }
                Err(error) => {
                    if was_manual {
                        self.show_toast(format!("GitHub sync failed: {error}"), ToastStyle::Error);
                    } else {
                        tracing::warn!("Background GitHub sync failed: {error}");
                    }
                }
            }
        }
    }
}

/// Spawn a `claustre session-host` subprocess for the given session, wait for
/// its Unix socket to appear, and connect a `RemoteTerminal`.
///
/// Falls back to a direct `EmbeddedTerminal::spawn` if the session-host fails
/// to start within the timeout.
pub(super) fn spawn_session_host_terminal(
    session_id: &str,
    worktree_path: &str,
    cmd_args: &[String],
    rows: u16,
    cols: u16,
) -> Result<Box<dyn crate::pty::Terminal>> {
    let claustre_exe = std::env::current_exe().context("failed to resolve claustre binary path")?;
    let socket_path = crate::config::session_socket_path(session_id)?;

    // Remove stale socket if it exists
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    // Spawn session-host as a detached subprocess.
    let mut host_cmd = std::process::Command::new(&claustre_exe);
    host_cmd
        .arg("session-host")
        .arg("--session-id")
        .arg(session_id)
        .arg("--worktree-path")
        .arg(worktree_path)
        .arg("--");
    for arg in cmd_args {
        host_cmd.arg(arg);
    }
    let stderr_cfg = crate::config::base_dir()
        .ok()
        .and_then(|dir| std::fs::File::create(dir.join("session-host.log")).ok())
        .map_or_else(std::process::Stdio::null, std::process::Stdio::from);
    host_cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(stderr_cfg);

    // SAFETY: setsid() is safe — it creates a new session so the child survives parent exit.
    // pre_exec runs between fork and exec in the child process.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid() is an async-signal-safe POSIX function with no memory
        // safety implications. Calling it between fork and exec is permitted.
        unsafe {
            host_cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    host_cmd
        .spawn()
        .context("failed to spawn claustre session-host")?;

    // Wait for socket to appear (poll ~50ms intervals, max 2s)
    let max_wait = Duration::from_secs(2);
    let poll_interval = Duration::from_millis(50);
    let start = std::time::Instant::now();
    while start.elapsed() < max_wait {
        if socket_path.exists() {
            break;
        }
        std::thread::sleep(poll_interval);
    }

    if !socket_path.exists() {
        anyhow::bail!(
            "session-host socket did not appear at {} within {}s",
            socket_path.display(),
            max_wait.as_secs()
        );
    }

    let remote = crate::pty::RemoteTerminal::connect(session_id, rows, cols)
        .context("failed to connect to session-host")?;
    Ok(Box::new(remote))
}
