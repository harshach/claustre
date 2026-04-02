use anyhow::{Context, Result, bail};

use crate::store::TaskStatus;

use super::{
    App, InputMode, SessionTabView, Tab, ToastStyle, compute_pane_sizes_for_resize,
    screen_shows_idle_prompt, screen_shows_permission_prompt, screen_shows_question_prompt,
};

enum PtyDetectedState {
    Paused,
    Waiting,
    Idle,
}

pub(super) fn classify_restored_claude_status(
    terminals: &crate::pty::SessionTerminals,
) -> Option<(crate::store::ClaudeStatus, &'static str)> {
    terminals.with_claude_live_screen(super::classify_restored_agent_screen)
}

impl App {
    pub(super) fn active_session_shows_conversation(&self) -> bool {
        matches!(
            self.tabs.get(self.active_tab),
            Some(Tab::Session {
                view_mode: SessionTabView::Conversation,
                ..
            })
        )
    }

    fn active_session_needs_editor_surface(&self, view_mode: SessionTabView) -> bool {
        matches!(view_mode, SessionTabView::Editor)
    }

    /// Whether the user is typing in a compose/text-input field.
    /// Used by the event loop to skip expensive PTY processing during typing.
    pub(super) fn is_compose_input_mode(&self) -> bool {
        matches!(
            self.input_mode,
            InputMode::ThreadCompose | InputMode::CommentCompose
        )
    }

    /// Restore a session tab for an active session whose PTY was lost (e.g. after
    /// Claustre was closed and reopened). If a session-host is still running,
    /// reconnects via `RemoteTerminal`; otherwise spawns a new session-host.
    pub(super) fn restore_session_tab(&mut self, session: &crate::store::Session) -> Result<()> {
        let worktree = std::path::Path::new(&session.worktree_path);
        if !worktree.exists() {
            self.show_toast("Worktree no longer exists on disk", ToastStyle::Error);
            bail!("worktree no longer exists on disk");
        }

        let linked_task = self
            .store
            .list_tasks_for_project(&session.project_id)?
            .into_iter()
            .find(|task| task.session_id.as_deref() == Some(session.id.as_str()));
        let linked_thread = crate::threads::ensure_thread_bridge_for_session(
            &self.store,
            session,
            linked_task.as_ref(),
        )?;

        let term_size = crossterm::terminal::size().unwrap_or((80, 24));
        let cols = term_size.0;
        let rows = term_size.1.saturating_sub(2);

        let provider_kind = linked_thread.provider_kind;
        let provider_profile = linked_thread.provider_profile.as_deref();
        let wrapped = crate::session::wrap_cmd_with_shell_fallback(
            crate::threads::build_resume_agent_command(
                &self.config,
                provider_kind,
                provider_profile,
                session,
            ),
        );

        // Try to reconnect to an existing session-host, or spawn a new one.
        let claude_terminal: Box<dyn crate::pty::Terminal> = if session_host_alive(&session.id) {
            // Session-host is still running — reconnect without spawning.
            Box::new(
                crate::pty::RemoteTerminal::connect(&session.id, rows, cols / 2)
                    .context("failed to reconnect to session-host")?,
            )
        } else {
            // Session-host is not running — spawn a new one with the resume command.
            super::polling::spawn_session_host_terminal(
                &session.id,
                &session.worktree_path,
                &wrapped,
                rows,
                cols / 2,
            )?
        };

        let mut terminals = if let Some(ref layout_config) = self.config.layout {
            crate::pty::SessionTerminals::from_layout(
                claude_terminal,
                &session.worktree_path,
                layout_config,
                rows,
                cols,
            )?
        } else {
            let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            let mut shell_cmd = portable_pty::CommandBuilder::new(&shell_path);
            shell_cmd.cwd(&session.worktree_path);
            let shell_terminal = crate::pty::EmbeddedTerminal::spawn(shell_cmd, rows, cols / 2)?;
            crate::pty::SessionTerminals::from_parts(
                shell_terminal,
                claude_terminal,
                &session.worktree_path,
            )
        };

        let sizes = compute_pane_sizes_for_resize(&terminals.layout, term_size.0, term_size.1);
        let _ = terminals.resize_panes_with_clear(&sizes);
        let restored_session_status = classify_restored_claude_status(&terminals);
        let label = session.tab_label.clone();
        let tab_idx = self.add_session_tab(session.id.clone(), Box::new(terminals), label);
        // Switch to the restored canonical tab.
        self.active_tab = tab_idx;

        // Restore session + task status based on task state
        if let Some(task) = self.tasks.iter().find(|t| {
            t.session_id.as_deref() == Some(&session.id) && t.status == TaskStatus::Interrupted
        }) {
            if task.pr_url.is_some() {
                // Interrupted task has an open PR — restore to in_review
                self.store
                    .update_task_status(&task.id, TaskStatus::InReview)?;
                self.store.update_session_status(
                    &session.id,
                    crate::store::ClaudeStatus::Done,
                    "PR in review",
                )?;
            } else {
                self.store
                    .update_task_status(&task.id, TaskStatus::Working)?;
                self.store.update_session_status(
                    &session.id,
                    crate::store::ClaudeStatus::Working,
                    "Restored",
                )?;
            }
        } else if self.tasks.iter().any(|t| {
            t.session_id.as_deref() == Some(&session.id)
                && matches!(
                    t.status,
                    TaskStatus::InReview | TaskStatus::Conflict | TaskStatus::CiFailed
                )
        }) {
            // Task already in review/conflict/ci_failed — session stays Done
            self.store.update_session_status(
                &session.id,
                crate::store::ClaudeStatus::Done,
                "PR in review",
            )?;
        } else if session.claude_status == crate::store::ClaudeStatus::Interrupted {
            // Only override status when recovering from a crash (interrupted).
            // Otherwise preserve the current DB status (e.g. Idle set by the
            // Notification hook). Prefer the restored screen state so a live
            // Claude prompt becomes "ready" immediately instead of inheriting
            // a stale "working" badge from the interrupted row.
            let (status, message) = restored_session_status
                .unwrap_or((crate::store::ClaudeStatus::Working, "Restored"));
            self.store
                .update_session_status(&session.id, status, message)?;
        }
        self.refresh_data()?;

        self.show_toast("Session tab restored", ToastStyle::Success);
        Ok(())
    }

    /// Restore tabs for active sessions on TUI startup.
    ///
    /// Scans DB for active (non-closed) sessions and opens a tab for each one
    /// that doesn't already have one, spawning `claude --continue` in the
    /// worktree as a normal local PTY.
    pub(super) fn reconnect_running_sessions(&mut self) {
        let Ok(projects) = self.store.list_projects() else {
            return;
        };
        for project in &projects {
            let Ok(sessions) = self.store.list_active_sessions_for_project(&project.id) else {
                continue;
            };
            for session in &sessions {
                // Skip if already have a tab for this session
                if self.tabs.iter().any(
                    |t| matches!(t, Tab::Session { session_id: sid, .. } if sid == &session.id),
                ) {
                    continue;
                }

                if let Err(e) = self.restore_session_tab(session) {
                    eprintln!("reconnect: failed to restore session {}: {e}", session.id);
                }
            }
        }
    }

    /// Switch to the next tab (wrapping around to Dashboard).
    pub(super) fn next_tab(&mut self) {
        if self.tabs.len() > 1 {
            self.active_tab = (self.active_tab + 1) % self.tabs.len();
            // Always reset input mode when switching tabs to prevent
            // compose/edit modes from leaking across tabs.
            self.input_mode = InputMode::Normal;
        }
    }

    /// Switch to the previous tab (wrapping around to last session).
    pub(super) fn prev_tab(&mut self) {
        if self.tabs.len() > 1 {
            if self.active_tab == 0 {
                self.active_tab = self.tabs.len() - 1;
            } else {
                self.active_tab -= 1;
            }
            self.input_mode = InputMode::Normal;
        }
    }

    /// Process PTY output for all session tabs (budget-limited per pane).
    /// Used by slow-tick and detection passes that need fresh state on all sessions.
    pub(super) fn process_pty_output(&mut self) {
        for tab in &mut self.tabs {
            if let Tab::Session { terminals, .. } = tab {
                terminals.process_output();
            }
        }
    }

    /// Process PTY output only for the currently active session tab.
    /// Background sessions accumulate in their channels and drain on slow ticks.
    /// This is the fast-path called before every render at 60 FPS.
    pub(super) fn process_active_pty_output(&mut self) {
        let session_state = self.tabs.get(self.active_tab).and_then(|tab| match tab {
            Tab::Session { view_mode, .. } => Some((
                *view_mode,
                self.active_session_needs_editor_surface(*view_mode),
            )),
            Tab::Dashboard => None,
        });
        if let Some(tab) = self.tabs.get_mut(self.active_tab)
            && let Tab::Session { terminals, .. } = tab
            && let Some((view_mode, needs_editor_surface)) = session_state
        {
            match view_mode {
                SessionTabView::Editor => terminals.process_editor_output(),
                SessionTabView::Conversation | SessionTabView::Terminal => {
                    terminals.process_layout_output();
                    if needs_editor_surface {
                        terminals.process_editor_output();
                    }
                }
            }
        }
    }

    /// Flush all pending PTY output for every session without a byte budget.
    /// Called when switching to a session tab so the first rendered frame
    /// shows fully up-to-date content instead of stale data from the last
    /// (potentially 1-second-old) dashboard tick.
    pub(super) fn flush_all_pty_output(&mut self) {
        for tab in &mut self.tabs {
            if let Tab::Session { terminals, .. } = tab {
                terminals.process_output_full();
            }
        }
    }

    /// Prepare only the active session tab for rendering.
    /// Only the visible tab needs its parser at the user's scroll offset.
    pub(super) fn prepare_active_render_scrollback(&mut self) {
        let session_state = self.tabs.get(self.active_tab).and_then(|tab| match tab {
            Tab::Session { view_mode, .. } => Some((
                *view_mode,
                self.active_session_needs_editor_surface(*view_mode),
            )),
            Tab::Dashboard => None,
        });
        if let Some(tab) = self.tabs.get_mut(self.active_tab)
            && let Tab::Session { terminals, .. } = tab
            && let Some((view_mode, needs_editor_surface)) = session_state
        {
            match view_mode {
                SessionTabView::Editor => terminals.prepare_editor_for_render(),
                SessionTabView::Conversation | SessionTabView::Terminal => {
                    terminals.prepare_layout_for_render();
                    if needs_editor_surface {
                        terminals.prepare_editor_for_render();
                    }
                }
            }
        }
    }

    /// Restore only the active session tab's parser to the live screen.
    pub(super) fn restore_active_live_scrollback(&mut self) {
        let session_state = self.tabs.get(self.active_tab).and_then(|tab| match tab {
            Tab::Session { view_mode, .. } => Some((
                *view_mode,
                self.active_session_needs_editor_surface(*view_mode),
            )),
            Tab::Dashboard => None,
        });
        if let Some(tab) = self.tabs.get_mut(self.active_tab)
            && let Tab::Session { terminals, .. } = tab
            && let Some((view_mode, needs_editor_surface)) = session_state
        {
            match view_mode {
                SessionTabView::Editor => terminals.restore_editor_after_render(),
                SessionTabView::Conversation | SessionTabView::Terminal => {
                    terminals.restore_layout_after_render();
                    if needs_editor_surface {
                        terminals.restore_editor_after_render();
                    }
                }
            }
        }
    }

    /// Detect sessions where Claude is blocked on user input by scanning PTY screens.
    ///
    /// Populates three sets:
    /// - `paused_sessions` — Claude is waiting for tool-approval ("Allow Bash?" dialog)
    /// - `waiting_sessions` — Claude asked a question via `AskUserQuestion` and awaits an answer
    /// - `pty_idle_sessions` — Claude's PTY shows the idle prompt (❯), fallback for hook failure
    ///
    /// Paused/waiting are in-memory overrides: the DB still shows `working`.
    /// Idle detection also updates the DB to fix stuck `working` status when hooks fail.
    pub(super) fn detect_paused_sessions(&mut self) {
        self.paused_sessions.clear();
        self.waiting_sessions.clear();
        self.pty_idle_sessions.clear();
        let mut stuck_session_label: Option<String> = None;
        for tab in &self.tabs {
            if let Tab::Session {
                session_id,
                terminals,
                label,
                ..
            } = tab
            {
                let _label = label;
                // Check sessions that have a working task OR a working session status
                let is_working_session = self.tasks.iter().any(|t| {
                    t.status == TaskStatus::Working
                        && t.session_id.as_deref() == Some(session_id.as_str())
                }) || self.sessions.iter().any(|s| {
                    s.id == *session_id && s.claude_status == crate::store::ClaudeStatus::Working
                });
                if !is_working_session {
                    continue;
                }

                // Use the live screen (scrollback 0) for detection so
                // prompts are not missed when the user has scrolled back.
                let detected = terminals.with_claude_live_screen(|screen| {
                    if screen_shows_permission_prompt(screen) {
                        Some(PtyDetectedState::Paused)
                    } else if screen_shows_question_prompt(screen) {
                        Some(PtyDetectedState::Waiting)
                    } else if screen_shows_idle_prompt(screen) {
                        Some(PtyDetectedState::Idle)
                    } else {
                        None
                    }
                });
                match detected {
                    Some(Some(PtyDetectedState::Paused)) => {
                        self.paused_sessions.insert(session_id.clone());
                        self.working_no_indicator_since.remove(session_id);
                    }
                    Some(Some(PtyDetectedState::Waiting)) => {
                        self.waiting_sessions.insert(session_id.clone());
                        self.working_no_indicator_since.remove(session_id);
                    }
                    Some(Some(PtyDetectedState::Idle)) => {
                        self.pty_idle_sessions.insert(session_id.clone());
                        self.working_no_indicator_since.remove(session_id);
                    }
                    Some(None) => {
                        let shell_prompt_visible = terminals
                            .with_claude_live_screen(|screen| {
                                super::screen_shows_shell_prompt(screen)
                            })
                            .unwrap_or(false);
                        if shell_prompt_visible {
                            self.working_no_indicator_since.remove(session_id);
                            continue;
                        }
                        // No Claude indicator on screen. Could be Claude actively
                        // working (tool output streaming) or Claude has exited
                        // (with a prompt style we do not explicitly recognize).
                        // Keep timing this state for "stuck" diagnostics, but
                        // do not declare it idle. Misclassifying shell fallback
                        // as ready causes native compose to write into the shell
                        // instead of restarting Claude.
                        let entry = self
                            .working_no_indicator_since
                            .entry(session_id.clone())
                            .or_insert_with(std::time::Instant::now);
                        let elapsed = entry.elapsed();
                        // Flag sessions stuck for 5+ minutes for a toast after the loop
                        if elapsed > std::time::Duration::from_secs(300)
                            && elapsed < std::time::Duration::from_secs(303)
                        {
                            stuck_session_label = Some(_label.clone());
                        }
                    }
                    None => {
                        // with_claude_live_screen returned None — no screen available
                    }
                }
            }
        }

        // Show stuck session warning toast (deferred from loop to avoid borrow conflict)
        if let Some(label) = stuck_session_label {
            self.show_toast(
                format!(
                    "Session '{label}' may be stuck — no activity for 5 min. Press Ctrl+O to check"
                ),
                ToastStyle::Error,
            );
        }

        // Fire OS notifications for sessions that just became paused or waiting.
        // Only notify once per paused/waiting transition (tracked via notified_paused_sessions).
        if self.config.notifications.rules.approval_required {
            for sid in &self.paused_sessions {
                if !self.notified_paused_sessions.contains(sid) {
                    self.notified_paused_sessions.insert(sid.clone());
                    let label = self
                        .tabs
                        .iter()
                        .find_map(|tab| match tab {
                            Tab::Session {
                                session_id, label, ..
                            } if session_id == sid => Some(label.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| "Session".to_string());
                    crate::config::NotificationConfig::system_notify_static(
                        &label,
                        "Permission required — check terminal",
                        None,
                    );
                }
            }
        }
        if self.config.notifications.rules.ask_user {
            for sid in &self.waiting_sessions {
                if !self.notified_paused_sessions.contains(sid) {
                    self.notified_paused_sessions.insert(sid.clone());
                    let label = self
                        .tabs
                        .iter()
                        .find_map(|tab| match tab {
                            Tab::Session {
                                session_id, label, ..
                            } if session_id == sid => Some(label.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| "Session".to_string());
                    crate::config::NotificationConfig::system_notify_static(
                        &label,
                        "Waiting for your answer",
                        None,
                    );
                }
            }
        }
        // Clear notification tracking for sessions no longer paused/waiting
        self.notified_paused_sessions.retain(|sid| {
            self.paused_sessions.contains(sid) || self.waiting_sessions.contains(sid)
        });

        // Clean up stale entries for sessions that are no longer working
        self.working_no_indicator_since.retain(|sid, _| {
            self.sessions
                .iter()
                .any(|s| s.id == *sid && s.claude_status == crate::store::ClaudeStatus::Working)
        });
    }

    /// Cache a live activity preview for each active session's Claude pane.
    /// Extracts the last few non-empty lines from the PTY screen.
    pub(super) fn cache_pty_activity_previews(&mut self) {
        self.pty_activity_preview.clear();
        for tab in &self.tabs {
            if let Tab::Session {
                session_id,
                terminals,
                ..
            } = tab
            {
                let preview = terminals.with_claude_live_screen(|screen| {
                    let rows = screen.size().0;
                    let cols = screen.size().1;
                    let mut lines: Vec<String> = Vec::new();
                    // Read from the bottom up to find the last non-empty lines
                    for row in (0..rows).rev() {
                        let line = screen
                            .contents_between(row, 0, row, cols)
                            .trim()
                            .to_string();
                        if !line.is_empty() {
                            lines.push(line);
                        }
                        if lines.len() >= 3 {
                            break;
                        }
                    }
                    lines.reverse();
                    lines
                });
                if let Some(lines) = preview
                    && !lines.is_empty()
                {
                    self.pty_activity_preview.insert(session_id.clone(), lines);
                }
            }
        }
    }
}

/// Check whether a session-host process is still alive for the given session.
///
/// Verifies both that the socket file exists and that the PID recorded in the
/// PID file is still a running process.
fn session_host_alive(session_id: &str) -> bool {
    let Ok(socket_path) = crate::config::session_socket_path(session_id) else {
        return false;
    };
    if !socket_path.exists() {
        return false;
    }

    let Ok(pid_path) = crate::config::session_pid_path(session_id) else {
        return false;
    };
    let Ok(pid_str) = std::fs::read_to_string(&pid_path) else {
        return false;
    };
    let Ok(pid) = pid_str.trim().parse::<i32>() else {
        return false;
    };

    // SAFETY: kill(pid, 0) checks process existence without sending a signal.
    // It is a standard POSIX operation with no memory-safety implications.
    unsafe { libc::kill(pid, 0) == 0 }
}
