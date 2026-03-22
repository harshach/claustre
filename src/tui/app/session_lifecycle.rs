use std::time::Instant;

use anyhow::Result;

use crate::pty::SessionTerminals;
use crate::store::{Task, TaskStatus, ThreadStatus};

use super::{
    App, SessionOpResult, SessionTabView, TOAST_DURATION, Tab, ToastStyle,
    compute_pane_sizes_for_resize, fallback_title,
};

impl App {
    fn ensure_session_thread_workspace(&mut self, session_id: &str) -> bool {
        if self
            .threads
            .iter()
            .any(|thread| thread.session_id.as_deref() == Some(session_id))
        {
            return true;
        }

        let Ok(session) = self.store.get_session(session_id) else {
            return false;
        };
        let linked_task = self
            .store
            .list_tasks_for_project(&session.project_id)
            .ok()
            .and_then(|tasks| {
                tasks
                    .into_iter()
                    .find(|task| task.session_id.as_deref() == Some(session_id))
            });

        if crate::threads::ensure_thread_bridge_for_session(
            &self.store,
            &session,
            linked_task.as_ref(),
        )
        .is_ok()
        {
            let _ = self.refresh_data();
            true
        } else {
            false
        }
    }

    /// Auto-launch pending autonomous tasks found at startup.
    /// Processes one task at a time, waiting for the previous session op to complete.
    pub(super) fn auto_launch_pending_tasks(&mut self) {
        if self.session_op_in_progress || self.startup_auto_launch.is_empty() {
            return;
        }
        let Some((project_id, task)) = self.startup_auto_launch.pop_front() else {
            return;
        };
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

    /// Spawn a background thread to create a session (worktree + config + DB).
    /// The TUI stays responsive while the potentially slow git commands run.
    /// When complete, the main thread spawns PTY terminals and adds the tab.
    pub(super) fn spawn_create_session(
        &mut self,
        project_id: String,
        branch_name: String,
        task: Task,
        base_branch: Option<String>,
    ) {
        self.session_op_in_progress = true;
        self.show_toast("Launching session...", ToastStyle::Info);
        let tx = self.session_op_tx.clone();
        let remote_enabled = self.config.remote_enabled;
        let claude_config = self.config.claude.clone();
        std::thread::spawn(move || {
            let result = match crate::store::Store::open() {
                Ok(store) => {
                    match crate::session::create_session(
                        &store,
                        &project_id,
                        &branch_name,
                        Some(&task),
                        base_branch.as_deref(),
                        remote_enabled,
                        &claude_config,
                    ) {
                        Ok(setup) => {
                            if setup.claude_cmd.is_none() {
                                SessionOpResult::CreatedNoTask {
                                    message: "Session created (no task)".into(),
                                }
                            } else {
                                SessionOpResult::Created(Box::new(setup))
                            }
                        }
                        Err(e) => SessionOpResult::Error {
                            message: format!("Launch failed: {e}"),
                        },
                    }
                }
                Err(e) => SessionOpResult::Error {
                    message: format!("Launch failed (DB): {e}"),
                },
            };
            let _ = tx.send(result);
        });
    }

    /// Unified entry point for launching a task as a session.
    ///
    /// Handles the full lifecycle: promotes Draft → Pending if needed, ensures a
    /// Haiku-generated title exists (spawning generation + queuing auto-launch if not),
    /// and finally creates the session once the title is ready.
    ///
    /// Called from both the `l` key handler and autonomous task creation.
    pub(super) fn launch_task(&mut self, task_id: String, project_id: String) -> Result<()> {
        let task = self.store.get_task(&task_id)?;

        // Promote draft → pending
        if task.status == crate::store::TaskStatus::Draft {
            self.store
                .update_task_status(&task_id, crate::store::TaskStatus::Pending)?;
        }

        // Title generation already in progress — just queue for launch when ready
        if self.pending_titles.contains(&task_id) {
            self.pending_auto_launch.insert(task_id, project_id);
            return Ok(());
        }

        // Task still has a fallback title — generate a proper one first, then launch
        if task.title == fallback_title(&task.description) {
            let description = task.description;
            self.spawn_title_generation(task_id.clone(), description);
            self.pending_auto_launch.insert(task_id, project_id);
            return Ok(());
        }

        // Title is ready — launch the session directly.
        // If task specifies a branch, reuse it; otherwise generate a new one.
        // If task specifies a base branch, the worktree is created from it and the PR targets it.
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
        Ok(())
    }

    /// Spawn a background thread to tear down a session (worktree cleanup + DB update).
    /// The TUI removes the session tab (dropping PTY handles) before calling this.
    /// Also cleans up any thread pointing to this session (NULLs `session_id`, sets Done).
    pub(super) fn spawn_teardown_session(&mut self, session_id: String) {
        // Remove the session tab immediately (drops PTY handles, kills child processes)
        self.remove_session_tab(&session_id);

        self.session_op_in_progress = true;
        let tx = self.session_op_tx.clone();
        std::thread::spawn(move || {
            let result = match crate::store::Store::open() {
                Ok(store) => match crate::session::teardown_session(&store, &session_id) {
                    Ok(()) => {
                        // Clean up any thread linked to this session
                        if let Ok(Some(thread)) = store.find_thread_for_session(&session_id) {
                            if matches!(thread.status, ThreadStatus::Running | ThreadStatus::Ready)
                            {
                                let _ = store.update_thread_status(&thread.id, ThreadStatus::Done);
                            }
                            let _ = store.update_thread_session_and_worktree(
                                &thread.id,
                                None,
                                thread.worktree_path.as_deref(),
                                thread.branch_name.as_deref(),
                                thread.runtime_profile.as_deref(),
                            );
                        }
                        SessionOpResult::TornDown {
                            message: "Session torn down".into(),
                        }
                    }
                    Err(e) => SessionOpResult::Error {
                        message: format!("Teardown failed: {e}"),
                    },
                },
                Err(e) => SessionOpResult::Error {
                    message: format!("Teardown failed (DB): {e}"),
                },
            };
            let _ = tx.send(result);
        });
    }

    /// Close the active thread session: set thread to Done, clear `session_id`, tear down.
    /// Always requires double-press: first `q` shows confirmation, second `q` actually closes.
    pub fn close_active_thread_session(&mut self) -> Result<()> {
        let Some(thread) = self.active_session_thread() else {
            return Ok(());
        };
        let Some(session_id) = self.active_session_id().map(String::from) else {
            return Ok(());
        };

        // If there's unsent compose text, first `q` clears it as a warning
        if !self.thread_compose_buffer.is_empty()
            && self.thread_compose_thread_id.as_deref() == Some(&thread.id)
        {
            self.thread_compose_buffer.clear();
            self.thread_compose_cursor = 0;
            self.thread_compose_thread_id = None;
            self.show_toast(
                "Compose text cleared. Press q again to close session.",
                ToastStyle::Info,
            );
            return Ok(());
        }

        // Require confirmation: first q shows warning toast, second q closes.
        // We use the toast_message as the confirmation flag — if the toast
        // already shows the close warning, proceed with closing.
        let is_confirming = self
            .toast_message
            .as_deref()
            .is_some_and(|msg| msg.contains("Press q again to close"));
        if !is_confirming {
            self.show_toast(
                "Press q again to close this session and mark thread as done",
                ToastStyle::Info,
            );
            return Ok(());
        }

        // Mark thread as Done
        self.store
            .update_thread_status(&thread.id, ThreadStatus::Done)?;

        let thread_title = thread.title;
        self.inspector_expanded = false;
        self.spawn_teardown_session(session_id);
        self.show_toast(format!("Thread closed: {thread_title}"), ToastStyle::Info);
        Ok(())
    }

    /// If a task has `review_loop` enabled and just transitioned to `InReview`,
    /// split down a pane in its session tab and run `claustre review-loop`.
    pub(super) fn maybe_spawn_review_loop(&mut self, task_id: &str) {
        if self.review_loop_spawned.contains(task_id) {
            return;
        }

        let Some(task) = self.tasks.iter().find(|t| t.id == task_id) else {
            return;
        };

        if !task.review_loop || task.status != TaskStatus::InReview {
            return;
        }

        let session_id = match task.session_id {
            Some(ref id) => id.clone(),
            None => return,
        };

        // Find the session tab
        let Some(tab_idx) = self.tabs.iter().position(
            |tab| matches!(tab, Tab::Session { session_id: sid, .. } if *sid == session_id),
        ) else {
            return;
        };

        // Build the review-loop command
        let claustre_exe =
            std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("claustre"));
        let mut cmd = portable_pty::CommandBuilder::new(claustre_exe.to_string_lossy().as_ref());
        cmd.arg("review-loop");
        cmd.arg("--session-id");
        cmd.arg(&session_id);

        // Set working directory to the worktree
        if let Tab::Session { terminals, .. } = &self.tabs[tab_idx] {
            cmd.cwd(&terminals.worktree_path);
        }

        // Set environment
        cmd.env("CLAUSTRE_SESSION", "1");

        let term_size = crossterm::terminal::size().unwrap_or((80, 24));
        let rows = term_size.1.saturating_sub(2);
        let cols = term_size.0;

        if let Tab::Session { terminals, .. } = &mut self.tabs[tab_idx] {
            // Focus the claude pane (first pane, typically) before splitting
            if let Err(e) = terminals.split_with_command(
                crate::pty::SplitDirection::Vertical,
                rows,
                cols,
                cmd,
                "Review Loop",
            ) {
                self.show_toast(format!("Review loop failed: {e}"), ToastStyle::Error);
                return;
            }
            let sizes = compute_pane_sizes_for_resize(&terminals.layout, term_size.0, term_size.1);
            let _ = terminals.resize_panes_with_clear(&sizes);
        }

        self.review_loop_spawned.insert(task_id.to_string());
        self.show_toast("Review loop started", ToastStyle::Info);
    }

    pub fn show_toast(&mut self, message: impl Into<String>, style: ToastStyle) {
        self.toast_message = Some(message.into());
        self.toast_style = style;
        self.toast_expires = Some(Instant::now() + TOAST_DURATION);
    }

    pub(super) fn tick_toast(&mut self) {
        if let Some(expires) = self.toast_expires
            && std::time::Instant::now() > expires
        {
            self.toast_message = None;
            self.toast_expires = None;
        }
    }

    /// Add a session tab with its terminals and switch to it.
    pub fn add_session_tab(
        &mut self,
        session_id: String,
        terminals: Box<SessionTerminals>,
        label: String,
    ) {
        // Ensure a thread workspace exists for session bridging (side-effectful).
        let _ = self.ensure_session_thread_workspace(&session_id)
            || self
                .store
                .find_thread_for_session(&session_id)
                .ok()
                .flatten()
                .is_some();
        let view_mode = SessionTabView::Terminal;
        self.tabs.push(Tab::Session {
            session_id,
            terminals,
            label,
            view_mode,
        });
        // Don't auto-switch to the new tab — stay on dashboard
    }

    /// Remove a session tab by session ID. Returns to dashboard if it was active.
    ///
    /// Before removing, requests shutdown on the Claude pane's terminal so that
    /// a `RemoteTerminal` can signal the session-host to exit gracefully.
    pub fn remove_session_tab(&mut self, session_id: &str) {
        if let Some(idx) = self
            .tabs
            .iter()
            .position(|t| matches!(t, Tab::Session { session_id: sid, .. } if sid == session_id))
        {
            // Request graceful shutdown on the Claude pane before dropping.
            if let Tab::Session { terminals, .. } = &mut self.tabs[idx] {
                let claude_id = terminals.claude_pane_id;
                if let Some(term) = terminals.terminal_mut(claude_id) {
                    term.request_shutdown();
                }
            }
            self.tabs.remove(idx);
            if self.active_tab >= idx && self.active_tab > 0 {
                self.active_tab -= 1;
            }
        }
    }

    /// Switch to the session tab matching the given session ID, if it exists.
    /// Returns `true` if the tab was found and activated.
    pub fn goto_session_tab(&mut self, session_id: &str) -> bool {
        if let Some(idx) = self
            .tabs
            .iter()
            .position(|t| matches!(t, Tab::Session { session_id: sid, .. } if sid == session_id))
        {
            self.active_tab = idx;
            if self.ensure_session_thread_workspace(session_id)
                && let Some(Tab::Session { view_mode, .. }) = self.tabs.get_mut(idx)
            {
                *view_mode = SessionTabView::Conversation;
            }
            true
        } else {
            false
        }
    }
}
