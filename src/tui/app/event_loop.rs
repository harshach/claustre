use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use ratatui::DefaultTerminal;

use super::super::event::{self, AppEvent};
use super::super::ui;
use super::{App, DASHBOARD_TICK, SESSION_TICK, SLOW_TICK};

/// How often to run screen detection (paused/waiting/idle) and activity previews.
/// These scan PTY screen contents (expensive), so we avoid running them at 60 FPS.
const DETECT_INTERVAL: Duration = Duration::from_millis(250);

impl App {
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut last_detect = Instant::now();
        let mut mouse_capture_enabled = false;

        loop {
            let wants_mouse_capture = self.should_capture_mouse();
            if wants_mouse_capture != mouse_capture_enabled {
                if wants_mouse_capture {
                    let _ = execute!(std::io::stdout(), EnableMouseCapture);
                } else {
                    let _ = execute!(std::io::stdout(), DisableMouseCapture);
                }
                mouse_capture_enabled = wants_mouse_capture;
            }

            // Adaptive tick rate: fast when viewing PTY, slow on dashboard.
            let tick_rate = if self.active_tab > 0 {
                SESSION_TICK
            } else {
                DASHBOARD_TICK
            };

            // Skip PTY processing when the user is typing in a compose field.
            // Only the text input changed — the terminal pane content is stale-OK
            // for one frame. PTY catches up on the next tick or when compose exits.
            // This eliminates the biggest source of per-keystroke latency.
            let in_compose = self.is_compose_input_mode();
            if !in_compose {
                self.process_active_pty_output();
            }

            if !in_compose {
                self.prepare_active_render_scrollback();
            }
            terminal.draw(|frame| {
                self.last_terminal_area = frame.area();
                ui::draw(frame, self);
            })?;
            if !in_compose {
                self.restore_active_live_scrollback();
            }

            let prev_tab = self.active_tab;

            match event::poll(tick_rate)? {
                AppEvent::Key(key) => {
                    self.mark_user_input_activity();
                    // When on a session tab, route most keys to the PTY
                    if self.active_tab > 0 {
                        self.handle_session_tab_key(key.code, key.modifiers)?;
                        // Drain any additional queued input events before redrawing.
                        // Mouse and resize events are handled inline so they
                        // aren't silently discarded.
                        while let Ok(extra) = event::poll(Duration::from_millis(0)) {
                            match extra {
                                AppEvent::Key(k) => {
                                    self.handle_session_tab_key(k.code, k.modifiers)?;
                                }
                                AppEvent::Paste(text) => {
                                    self.handle_session_tab_paste(&text)?;
                                }
                                AppEvent::Mouse(mouse) => {
                                    self.handle_mouse(mouse)?;
                                }
                                AppEvent::Resize(cols, rows) => {
                                    self.handle_resize(cols, rows);
                                }
                                AppEvent::Tick => break,
                            }
                        }
                        // Process active session's PTY output immediately so the
                        // next frame reflects the keystroke echo.
                        self.process_active_pty_output();
                    } else {
                        self.handle_dashboard_key(key.code, key.modifiers)?;
                    }
                }
                AppEvent::Paste(text) => {
                    self.mark_user_input_activity();
                    if self.active_tab > 0 {
                        self.handle_session_tab_paste(&text)?;
                        self.process_active_pty_output();
                    } else {
                        self.handle_dashboard_paste(&text)?;
                    }
                }
                AppEvent::Mouse(mouse) => {
                    self.handle_mouse(mouse)?;
                    // On session tabs, drain queued events and process PTY
                    // output so the next frame reflects all pending scroll
                    // and input changes without intermediate redraws.
                    if self.active_tab > 0 {
                        while let Ok(extra) = event::poll(Duration::from_millis(0)) {
                            match extra {
                                AppEvent::Key(k) => {
                                    self.handle_session_tab_key(k.code, k.modifiers)?;
                                }
                                AppEvent::Paste(text) => {
                                    self.handle_session_tab_paste(&text)?;
                                }
                                AppEvent::Mouse(m) => {
                                    self.handle_mouse(m)?;
                                }
                                AppEvent::Resize(cols, rows) => {
                                    self.handle_resize(cols, rows);
                                }
                                AppEvent::Tick => break,
                            }
                        }
                        self.process_active_pty_output();
                    }
                }
                AppEvent::Tick => {
                    if self.loading {
                        self.refresh_data()?;
                        self.loading = false;
                    }

                    let defer_background_work = self.should_defer_background_work();

                    // Screen detection (paused/waiting/idle) and activity previews
                    // are expensive (full screen content scan per session). Run them
                    // at a reduced cadence instead of every 16 ms tick.
                    if !defer_background_work && last_detect.elapsed() >= DETECT_INTERVAL {
                        last_detect = Instant::now();
                        // Process ALL sessions so detection sees fresh screens.
                        self.process_pty_output();
                        self.detect_paused_sessions();
                        self.cache_pty_activity_previews();
                    }

                    self.drain_queued_compose_message();

                    // Fast-path tick work (channel drains — all non-blocking try_recv)
                    self.tick_toast();
                    self.poll_title_results()?;
                    self.poll_session_ops();
                    self.poll_github_mutations();
                    self.poll_github_sync_results();
                    self.auto_launch_pending_tasks();
                    self.poll_pr_merge_results()?;
                    self.poll_git_stats_results();
                    self.poll_scanner_results();
                    self.poll_github_auth_results();
                    self.poll_github_installation_results();
                    self.poll_update_results();
                    // Refresh JSONL conversation on every tick when on a
                    // session tab so new messages appear quickly.  The method
                    // does an mtime check internally and is a no-op when the
                    // file hasn't changed.
                    if !defer_background_work
                        && self.active_tab > 0
                        && self.active_session_shows_conversation()
                    {
                        self.refresh_conversation_cache();
                    }

                    // Slow-path tick work (DB refresh, background polls)
                    // Throttled on all tabs: dashboard ticks are now 200 ms,
                    // so we gate the heavy work behind the same elapsed check.
                    let run_slow =
                        !defer_background_work && self.last_slow_tick.elapsed() >= SLOW_TICK;
                    if run_slow {
                        self.last_slow_tick = Instant::now();
                        // Drain background session PTY output so channels don't
                        // grow unbounded while the user is on another tab.
                        self.process_pty_output();
                        self.maybe_poll_pr_merges();
                        self.maybe_poll_git_stats();
                        self.maybe_scan_external_sessions();
                        self.maybe_poll_update_check();
                        self.maybe_poll_github_sync();
                        self.maybe_teardown_push_mode_sessions();
                        self.check_clipboard_for_image();
                        self.refresh_data()?;
                        if self.active_tab > 0 && self.active_session_shows_conversation() {
                            self.refresh_conversation_cache();
                        }
                        self.refresh_session_thread_context();
                        self.maybe_advance_workflow_stages();
                    }
                }
                AppEvent::Resize(cols, rows) => {
                    self.handle_resize(cols, rows);
                }
            }

            // When switching to a session tab, flush all pending PTY output
            // without a byte budget so the first frame shows fully current
            // content.  This eliminates the visible catch-up lag caused by
            // output accumulating during the slower dashboard tick interval.
            if self.active_tab != prev_tab && self.active_tab > 0 {
                self.flush_all_pty_output();
            }

            if self.should_quit {
                if mouse_capture_enabled {
                    let _ = execute!(std::io::stdout(), DisableMouseCapture);
                }
                return Ok(());
            }
        }
    }
}
