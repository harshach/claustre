use std::fmt::Write as _;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Rect};

use crate::pty::SplitDirection;

use super::super::form::apply_text_edit;
use super::super::ui;
use super::{
    App, BoardScope, DeleteTarget, Focus, InputMode, InspectorTab, LaunchThreadDraft,
    LaunchThreadField, PaletteAction, PendingBoardIssueLaunch, PendingReviewLaunchMode,
    PendingReviewPrLaunch, ReviewQueueTab, SessionOpResult, SessionTabView, SettingsEditTarget,
    SettingsSection, SidebarItem, Tab, ToastStyle, WorkbenchDragTarget, WorkbenchView,
    compute_pane_sizes_for_resize, fallback_title,
};

const LAUNCH_THREAD_PROVIDERS: [crate::store::ProviderKind; 2] = [
    crate::store::ProviderKind::Claude,
    crate::store::ProviderKind::Codex,
];

#[derive(Debug, Clone, Copy)]
enum ThreadRuntimeAction {
    Up,
    Down,
    Restart,
    Health,
}

impl App {
    fn maybe_prefetch_github_installations(&mut self) {
        if self.workbench_view == WorkbenchView::Settings
            && SettingsSection::ALL
                .get(self.settings_section_index)
                .copied()
                == Some(SettingsSection::GitHub)
            && self.github_status.authenticated
            && self.github_installations.is_empty()
        {
            self.spawn_github_installations_fetch();
        }
    }

    /// Dispatch a key event to the correct dashboard handler based on `input_mode`.
    pub(super) fn handle_dashboard_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match self.input_mode {
            InputMode::Normal => self.handle_normal_key(code, modifiers)?,
            InputMode::LaunchThread => self.handle_launch_thread_key(code, modifiers)?,
            InputMode::ThreadCompose => self.handle_thread_compose_key(code, modifiers)?,
            InputMode::ThreadProviderPicker => {
                self.handle_thread_provider_picker_key(code)?;
            }
            InputMode::SettingsEdit => self.handle_settings_edit_key(code, modifiers)?,
            InputMode::SettingsPicker => self.handle_settings_picker_key(code)?,
            InputMode::GitHubInstallationPicker => {
                self.handle_github_installation_picker_key(code)?;
            }
            InputMode::GitHubProjectPicker => {
                self.handle_github_project_picker_key(code, modifiers)?;
            }
            InputMode::ProjectPicker => {
                self.handle_project_picker_key(code)?;
            }
            InputMode::NewTask => self.handle_input_key(code, modifiers)?,
            InputMode::EditTask => self.handle_edit_task_key(code, modifiers)?,
            InputMode::NewProject => self.handle_new_project_key(code, modifiers)?,
            InputMode::ConfirmDelete => self.handle_confirm_delete_key(code)?,
            InputMode::CommandPalette => self.handle_palette_key(code, modifiers)?,
            InputMode::SkillPanel => self.handle_skill_panel_key(code)?,
            InputMode::SkillSearch => self.handle_skill_search_key(code, modifiers)?,
            InputMode::SkillAdd => self.handle_skill_add_key(code, modifiers)?,
            InputMode::HelpOverlay => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('?' | 'q')) {
                    self.input_mode = InputMode::Normal;
                }
            }
            InputMode::TaskDetails => match code {
                KeyCode::Esc | KeyCode::Char('v' | 'q') => {
                    self.input_mode = InputMode::Normal;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.task_details_scroll = self.task_details_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.task_details_scroll = self.task_details_scroll.saturating_sub(1);
                }
                _ => {}
            },
            InputMode::TaskFilter => self.handle_task_filter_key(code, modifiers)?,
            InputMode::SubtaskPanel => self.handle_subtask_panel_key(code, modifiers)?,
            InputMode::ConfigureWizard => self.handle_configure_key(code)?,
            InputMode::BoardView => self.handle_board_key(code, modifiers)?,
            InputMode::MilestoneFilter => self.handle_board_sprint_picker_key(code)?,
            InputMode::BoardFilter => self.handle_board_filter_key(code, modifiers)?,
            InputMode::CommentCompose => self.handle_comment_compose_key(code, modifiers)?,
            InputMode::BoardIssueDrawer => self.handle_board_drawer_key(code, modifiers)?,
            InputMode::MyTaskDrawer => self.handle_my_task_drawer_key(code, modifiers)?,
            InputMode::FieldPicker => self.handle_field_picker_key(code)?,
            InputMode::CreateGitHubIssue | InputMode::EditGitHubIssue => {
                self.handle_github_issue_form_key(code, modifiers)?;
            }
            InputMode::ReviewDrawer => self.handle_review_drawer_key(code, modifiers)?,
        }
        Ok(())
    }

    /// Handle keys when a session tab is active.
    /// Intercept registered session keys; forward everything else to the PTY.
    pub(super) fn handle_session_tab_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        // Handle confirmation dialogs (close session, etc.)
        if self.input_mode == InputMode::ConfirmDelete {
            return self.handle_confirm_delete_key(code);
        }

        // Handle permission dialog: Enter = allow, Esc = deny
        if self.active_session_is_paused() {
            return self.handle_permission_dialog_key(code);
        }

        if self.active_session_uses_thread_workspace() {
            return self.handle_thread_session_tab_key(code, modifiers);
        }

        // Ctrl+Q = close session permanently (with confirmation)
        if modifiers == KeyModifiers::CONTROL && matches!(code, KeyCode::Char('q')) {
            self.prompt_close_session();
            return Ok(());
        }

        // Ctrl+O toggles back to Conversation view (if session has a thread)
        if modifiers == KeyModifiers::CONTROL
            && matches!(code, KeyCode::Char('o'))
            && self.active_session_thread().is_some()
        {
            if self.toggle_active_session_view() {
                self.show_toast("Switched to conversation view", ToastStyle::Info);
            }
            return Ok(());
        }

        if let Some(action) = self.keymap.lookup_session(code, modifiers) {
            self.execute_session_action(action)?;
            return Ok(());
        }

        // Forward to focused PTY, clear selection, and snap back to live screen
        if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
            terminals.selection = None;
            if let Some(term) = terminals.focused_terminal() {
                term.reset_scrollback();
                let key_bytes = keycode_to_bytes(code, modifiers);
                if key_bytes.len > 0 {
                    let _ = term.send_bytes(key_bytes.as_bytes());
                }
            }
        }
        Ok(())
    }

    fn active_session_uses_thread_workspace(&self) -> bool {
        matches!(
            self.tabs.get(self.active_tab),
            Some(Tab::Session {
                view_mode: SessionTabView::Conversation,
                ..
            })
        ) && self.active_session_thread().is_some()
    }

    fn toggle_active_session_view(&mut self) -> bool {
        let Some(Tab::Session { view_mode, .. }) = self.tabs.get_mut(self.active_tab) else {
            return false;
        };

        *view_mode = match *view_mode {
            SessionTabView::Conversation => SessionTabView::Terminal,
            SessionTabView::Terminal => SessionTabView::Conversation,
        };
        true
    }

    /// Execute a session-mode action (dashboard return, pane focus, splits, close).
    fn execute_session_action(&mut self, action: super::super::keymap::Action) -> Result<()> {
        use super::super::keymap::Action;
        match action {
            Action::ReturnToDashboard => {
                self.input_mode = InputMode::Normal;
                self.active_tab = 0;
            }
            Action::FocusPrevPane => {
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
                    terminals.focus_prev();
                }
            }
            Action::FocusNextPane => {
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
                    terminals.focus_next();
                }
            }
            Action::ScrollToBottom => {
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                    && let Some(term) = terminals.focused_terminal()
                {
                    term.reset_scrollback();
                }
            }
            Action::ScrollPageUp => {
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                    && let Some(term) = terminals.focused_terminal()
                {
                    let rows = usize::from(term.screen().size().0);
                    let half = rows / 2;
                    term.scroll_up(half);
                }
            }
            Action::ScrollPageDown => {
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                    && let Some(term) = terminals.focused_terminal()
                {
                    let rows = usize::from(term.screen().size().0);
                    let half = rows / 2;
                    term.scroll_down(half);
                }
            }
            Action::PrevTab => self.prev_tab(),
            Action::NextTab => self.next_tab(),
            Action::SplitRight => {
                let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                let rows = term_size.1.saturating_sub(2);
                let cols = term_size.0;
                let split_err = if let Some(Tab::Session { terminals, .. }) =
                    self.tabs.get_mut(self.active_tab)
                {
                    let err = terminals
                        .split_focused(SplitDirection::Horizontal, rows, cols)
                        .err();
                    let sizes =
                        compute_pane_sizes_for_resize(&terminals.layout, term_size.0, term_size.1);
                    let _ = terminals.resize_panes_with_clear(&sizes);
                    err
                } else {
                    None
                };
                if let Some(e) = split_err {
                    self.show_toast(format!("Split failed: {e}"), ToastStyle::Error);
                }
            }
            Action::SplitDown => {
                let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                let rows = term_size.1.saturating_sub(2);
                let cols = term_size.0;
                let split_err = if let Some(Tab::Session { terminals, .. }) =
                    self.tabs.get_mut(self.active_tab)
                {
                    let err = terminals
                        .split_focused(SplitDirection::Vertical, rows, cols)
                        .err();
                    let sizes =
                        compute_pane_sizes_for_resize(&terminals.layout, term_size.0, term_size.1);
                    let _ = terminals.resize_panes_with_clear(&sizes);
                    err
                } else {
                    None
                };
                if let Some(e) = split_err {
                    self.show_toast(format!("Split failed: {e}"), ToastStyle::Error);
                }
            }
            Action::ClosePane => {
                let close_result = if let Some(Tab::Session { terminals, .. }) =
                    self.tabs.get_mut(self.active_tab)
                {
                    let closed = terminals.close_focused();
                    if closed {
                        let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                        let sizes = compute_pane_sizes_for_resize(
                            &terminals.layout,
                            term_size.0,
                            term_size.1,
                        );
                        // Use clearing variant: panes that changed width after
                        // the closed pane's space was reclaimed need their
                        // screen buffer cleared so old text wrapped at the
                        // previous width doesn't persist.
                        let _ = terminals.resize_panes_with_clear(&sizes);
                    }
                    Some(closed)
                } else {
                    None
                };
                if close_result == Some(false) {
                    self.show_toast("Cannot close this pane", ToastStyle::Info);
                }
            }
            // Normal-mode-only actions are no-ops in session mode
            _ => {}
        }
        Ok(())
    }

    fn execute_thread_session_action(
        &mut self,
        action: super::super::keymap::Action,
    ) -> Result<()> {
        use super::super::keymap::Action;

        match action {
            Action::ReturnToDashboard => {
                self.input_mode = InputMode::Normal;
                self.active_tab = 0;
            }
            Action::PrevTab => self.prev_tab(),
            Action::NextTab => self.next_tab(),
            Action::FocusPrevPane | Action::FocusNextPane => {
                self.focus = if self.focus == Focus::Inspector {
                    Focus::Tasks
                } else {
                    Focus::Inspector
                };
            }
            Action::ScrollPageUp => {
                self.inspector_scroll = self.inspector_scroll.saturating_sub(8);
            }
            Action::ScrollPageDown => {
                self.inspector_scroll = self.inspector_scroll.saturating_add(8);
            }
            Action::ScrollToBottom => {
                self.inspector_scroll = u16::MAX;
            }
            Action::SplitRight | Action::SplitDown | Action::ClosePane => {
                self.show_toast(
                    "Open the raw terminal view (Ctrl+O) to manage PTY panes",
                    ToastStyle::Info,
                );
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_thread_session_tab_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        // Ctrl+Q = close session permanently (with confirmation)
        if modifiers == KeyModifiers::CONTROL && matches!(code, KeyCode::Char('q')) {
            self.prompt_close_session();
            return Ok(());
        }

        if modifiers == KeyModifiers::CONTROL && matches!(code, KeyCode::Char('o')) {
            if self.toggle_active_session_view() {
                self.show_toast("Switched to terminal view", ToastStyle::Info);
            }
            return Ok(());
        }

        if self.input_mode == InputMode::ThreadCompose {
            return self.handle_thread_compose_key(code, modifiers);
        }
        if self.input_mode == InputMode::ThreadProviderPicker {
            return self.handle_thread_provider_picker_key(code);
        }

        if let Some(action) = self.keymap.lookup_session(code, modifiers) {
            self.execute_thread_session_action(action)?;
            return Ok(());
        }

        // Ctrl+C in conversation view: interrupt Claude and force-send queued message
        if code == KeyCode::Char('c') && modifiers == KeyModifiers::CONTROL {
            if self.interrupt_active_session_claude() {
                if let Some((thread_id, content)) = self.queued_compose_message.take() {
                    // User wants to steer the agent — interrupt and send the queued message
                    self.show_toast(
                        "Interrupted Claude — sending your message",
                        ToastStyle::Info,
                    );
                    // Small delay for the interrupt to take effect, then send
                    // The message goes into the PTY buffer; Claude reads it
                    // after the interrupt is processed.
                    if let Ok(thread) = self.store.get_thread(&thread_id) {
                        let _ = self.send_to_active_session_claude(&content)
                            || self.send_prompt_to_live_thread_session(&thread, &content);
                        if let Some(ref mut cache) = self.conversation_cache {
                            cache.entries.push(
                                crate::conversation::ConversationEntry::UserMessage {
                                    timestamp: chrono::Utc::now()
                                        .format("%Y-%m-%dT%H:%M:%S")
                                        .to_string(),
                                    text: content,
                                },
                            );
                        }
                    }
                } else {
                    self.show_toast("Sent interrupt (Ctrl+C) to Claude", ToastStyle::Info);
                }
            }
            return Ok(());
        }

        if modifiers.is_empty() {
            match code {
                KeyCode::Char('P' | 'p') => {
                    self.open_project_picker()?;
                    return Ok(());
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    self.focus = if self.focus == Focus::Inspector {
                        Focus::Tasks
                    } else {
                        Focus::Inspector
                    };
                    return Ok(());
                }
                KeyCode::Char('j') | KeyCode::Down if self.focus == Focus::Inspector => {
                    self.inspector_scroll = self.inspector_scroll.saturating_add(1);
                    return Ok(());
                }
                KeyCode::Char('k') | KeyCode::Up if self.focus == Focus::Inspector => {
                    self.inspector_scroll = self.inspector_scroll.saturating_sub(1);
                    return Ok(());
                }
                KeyCode::Char('i') => {
                    if let Some(thread_id) = self.active_session_thread().map(|thread| thread.id) {
                        self.start_thread_compose_for(&thread_id, false)?;
                    }
                    return Ok(());
                }
                // Quick-reply: when choices are detected and conversation pane
                // is focused, number keys select and send a choice.
                KeyCode::Char(c @ '1'..='9')
                    if self.focus != Focus::Inspector && !self.quick_reply_choices.is_empty() =>
                {
                    let idx = (c as usize) - ('1' as usize);
                    if idx < self.quick_reply_choices.len() {
                        // Send the full choice text (e.g. "3. Both") for clarity
                        let choice = format!("{}. {}", idx + 1, self.quick_reply_choices[idx]);
                        self.thread_compose_buffer = choice.clone();
                        self.thread_compose_cursor = choice.len();
                        self.input_buffer = choice;
                        self.input_cursor = self.input_buffer.len();
                        if let Some(thread_id) =
                            self.active_session_thread().map(|thread| thread.id.clone())
                        {
                            self.thread_compose_thread_id = Some(thread_id);
                        }
                        self.submit_thread_compose_message()?;
                    }
                    return Ok(());
                }
                KeyCode::Char('1') => {
                    self.inspector_tab = InspectorTab::Issue;
                    self.focus = Focus::Inspector;
                    self.inspector_scroll = 0;
                    return Ok(());
                }
                KeyCode::Char('2') => {
                    self.inspector_tab = InspectorTab::Diff;
                    self.focus = Focus::Inspector;
                    self.inspector_scroll = 0;
                    return Ok(());
                }
                KeyCode::Char('3') => {
                    self.inspector_tab = InspectorTab::Runtime;
                    self.focus = Focus::Inspector;
                    self.inspector_scroll = 0;
                    return Ok(());
                }
                KeyCode::Char('4') => {
                    self.inspector_tab = InspectorTab::Tests;
                    self.focus = Focus::Inspector;
                    self.inspector_scroll = 0;
                    return Ok(());
                }
                KeyCode::Char('5') => {
                    self.inspector_tab = InspectorTab::Plan;
                    self.focus = Focus::Inspector;
                    self.inspector_scroll = 0;
                    return Ok(());
                }
                KeyCode::Char('6') => {
                    self.inspector_tab = InspectorTab::Attachments;
                    self.focus = Focus::Inspector;
                    self.inspector_scroll = 0;
                    return Ok(());
                }
                KeyCode::Char('u') => {
                    self.run_active_thread_runtime_action(ThreadRuntimeAction::Up)?;
                    return Ok(());
                }
                KeyCode::Char('d') => {
                    self.run_active_thread_runtime_action(ThreadRuntimeAction::Down)?;
                    return Ok(());
                }
                KeyCode::Char('r') => {
                    self.run_active_thread_runtime_action(ThreadRuntimeAction::Restart)?;
                    return Ok(());
                }
                KeyCode::Char('h') => {
                    self.run_active_thread_runtime_action(ThreadRuntimeAction::Health)?;
                    return Ok(());
                }
                KeyCode::Char('m') => {
                    self.open_thread_provider_picker()?;
                    return Ok(());
                }
                KeyCode::Char('f') => {
                    self.inspector_expanded = !self.inspector_expanded;
                    if self.inspector_expanded {
                        self.focus = Focus::Inspector;
                    }
                    return Ok(());
                }
                KeyCode::Char('g') => {
                    self.approve_active_workflow_gate()?;
                    return Ok(());
                }
                KeyCode::Char('q') => {
                    self.close_active_thread_session()?;
                    return Ok(());
                }
                _ => {}
            }
        }

        if modifiers.is_empty()
            && matches!(code, KeyCode::Char(_))
            && let Some(thread_id) = self.active_session_thread().map(|thread| thread.id)
        {
            self.start_thread_compose_for(&thread_id, false)?;
            self.handle_thread_compose_key(code, modifiers)?;
        }

        Ok(())
    }

    /// Forward pasted text to the focused PTY on a session tab.
    pub(super) fn handle_session_tab_paste(&mut self, text: &str) -> Result<()> {
        if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
            terminals.selection = None;
            if let Some(term) = terminals.focused_terminal() {
                term.reset_scrollback();
                // Send as bracketed paste so the embedded shell/editor handles it correctly
                let bracketed = format!("\x1b[200~{text}\x1b[201~");
                let _ = term.send_bytes(bracketed.as_bytes());
            }
        }
        Ok(())
    }

    /// Handle pasted text on the dashboard by inserting at cursor in the active input buffer.
    pub(super) fn handle_dashboard_paste(&mut self, text: &str) -> Result<()> {
        match self.input_mode {
            InputMode::NewTask | InputMode::EditTask
                if self.new_task_field == 0
                    || self.new_task_field == 2
                    || self.new_task_field == 3 =>
            {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
            }
            InputMode::NewProject => {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
                if self.new_project_field == 1 {
                    self.update_path_suggestions();
                }
            }
            InputMode::CommandPalette => {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
                self.filter_palette();
                self.palette_index = 0;
            }
            InputMode::SkillSearch => {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
                self.search_results.clear();
                self.skill_status_message.clear();
            }
            InputMode::SkillAdd
            | InputMode::SubtaskPanel
            | InputMode::SettingsEdit
            | InputMode::LaunchThread
            | InputMode::ThreadCompose => {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
                if self.input_mode == InputMode::ThreadCompose {
                    self.thread_compose_buffer = self.input_buffer.clone();
                    self.thread_compose_cursor = self.input_cursor;
                }
            }
            InputMode::TaskFilter => {
                self.task_filter
                    .insert_str(self.task_filter_cursor.min(self.task_filter.len()), text);
                self.task_filter_cursor =
                    (self.task_filter_cursor + text.len()).min(self.task_filter.len());
                self.recompute_visible_tasks();
                self.task_index = 0;
            }
            InputMode::BoardFilter => {
                self.board_filter
                    .insert_str(self.board_filter_cursor.min(self.board_filter.len()), text);
                self.board_filter_cursor =
                    (self.board_filter_cursor + text.len()).min(self.board_filter.len());
                self.apply_board_filter();
            }
            // Normal, ConfirmDelete, SkillPanel, HelpOverlay: no text input
            _ => {}
        }
        Ok(())
    }

    /// Handle terminal resize events — resize all PTYs to match new dimensions.
    ///
    /// Uses ratatui's layout engine to compute exact inner areas for each pane,
    /// ensuring PTY sizes always match the rendered areas.  After resizing, a
    /// single `process_pty_output()` pass runs so the parser state (scroll
    /// offset, screen content) is synced before the next frame is drawn.
    pub(super) fn handle_resize(&mut self, cols: u16, rows: u16) {
        for tab in &mut self.tabs {
            if let Tab::Session { terminals, .. } = tab {
                let sizes = compute_pane_sizes_for_resize(&terminals.layout, cols, rows);
                let _ = terminals.resize_panes_with_clear(&sizes);
            }
        }
        // Process any pending PTY output immediately so the next draw uses
        // up-to-date parser state (scroll offset, screen content).
        self.process_pty_output();
    }

    /// Compute inner areas (content inside borders) for all panes in the current session tab.
    /// Returns a list of `(PaneId, inner_rect)` in absolute screen coordinates.
    fn session_pane_inner_areas(&self) -> Vec<(crate::pty::PaneId, Rect)> {
        use super::collect_pane_inner_areas;

        let size = self.last_terminal_area;
        let has_tab_bar = self.tabs.len() > 1;
        let tab_bar_height = u16::from(has_tab_bar);

        let term_area = Rect {
            x: 0,
            y: tab_bar_height,
            width: size.width,
            height: size.height.saturating_sub(tab_bar_height + 1),
        };

        if let Some(Tab::Session { terminals, .. }) = self.tabs.get(self.active_tab) {
            collect_pane_inner_areas(&terminals.layout, term_area)
        } else {
            vec![]
        }
    }

    /// Translate absolute screen coordinates to vt100 terminal coordinates for a pane.
    /// Returns `(PaneId, vt100_row, vt100_col)` or `None` if outside all panes.
    fn screen_to_terminal_coords(
        &self,
        screen_col: u16,
        screen_row: u16,
    ) -> Option<(crate::pty::PaneId, u16, u16)> {
        for (id, inner) in &self.session_pane_inner_areas() {
            if screen_col >= inner.x
                && screen_col < inner.x + inner.width
                && screen_row >= inner.y
                && screen_row < inner.y + inner.height
            {
                return Some((*id, screen_row - inner.y, screen_col - inner.x));
            }
        }
        None
    }

    fn workbench_area(&self) -> Rect {
        if self.tabs.len() > 1 {
            Rect::new(
                0,
                1,
                self.last_terminal_area.width,
                self.last_terminal_area.height.saturating_sub(1),
            )
        } else {
            self.last_terminal_area
        }
    }

    fn resize_workbench_pane(&mut self, delta: i16) {
        let area = self.workbench_area();
        if area.width == 0 || area.height == 0 {
            return;
        }
        let show_inspector = ui::workbench_uses_persistent_inspector(self.workbench_view);

        let body_width = ui::compute_workbench_layout(
            area,
            self.workbench_sidebar_width,
            self.workbench_inspector_width,
            show_inspector,
        )
        .body
        .width;
        if body_width == 0 {
            return;
        }

        let current_sidebar = self.workbench_sidebar_width;
        let current_inspector = self.workbench_inspector_width;
        let step = delta.unsigned_abs();

        let desired = if !show_inspector || self.focus == Focus::Projects {
            if delta.is_negative() {
                current_sidebar.saturating_sub(step)
            } else {
                current_sidebar.saturating_add(step)
            }
        } else if delta.is_negative() {
            current_inspector.saturating_sub(step)
        } else {
            current_inspector.saturating_add(step)
        };

        let (sidebar_width, inspector_width) = if !show_inspector || self.focus == Focus::Projects {
            ui::normalize_workbench_widths(body_width, desired, current_inspector)
        } else {
            ui::normalize_workbench_widths(body_width, current_sidebar, desired)
        };

        self.workbench_sidebar_width = sidebar_width;
        self.workbench_inspector_width = inspector_width;
    }

    fn begin_workbench_drag(&mut self, target: WorkbenchDragTarget) {
        self.workbench_drag_target = Some(target);
        self.focus = match target {
            WorkbenchDragTarget::SidebarDivider => Focus::Projects,
            WorkbenchDragTarget::InspectorDivider => Focus::Inspector,
        };
    }

    fn update_workbench_drag(&mut self, mouse_col: u16) {
        let Some(target) = self.workbench_drag_target else {
            return;
        };
        let area = self.workbench_area();
        let show_inspector = ui::workbench_uses_persistent_inspector(self.workbench_view);
        let layout = ui::compute_workbench_layout(
            area,
            self.workbench_sidebar_width,
            self.workbench_inspector_width,
            show_inspector,
        );
        if layout.body.width == 0 {
            return;
        }

        match target {
            WorkbenchDragTarget::SidebarDivider => {
                let desired = mouse_col
                    .saturating_sub(layout.body.x)
                    .saturating_add(1)
                    .min(layout.body.width.saturating_sub(2));
                let (sidebar_width, inspector_width) = ui::normalize_workbench_widths(
                    layout.body.width,
                    desired,
                    self.workbench_inspector_width,
                );
                self.workbench_sidebar_width = sidebar_width;
                self.workbench_inspector_width = inspector_width;
            }
            WorkbenchDragTarget::InspectorDivider => {
                let body_right = layout.body.x.saturating_add(layout.body.width);
                let desired = body_right
                    .saturating_sub(mouse_col)
                    .max(1)
                    .min(layout.body.width.saturating_sub(2));
                let (sidebar_width, inspector_width) = ui::normalize_workbench_widths(
                    layout.body.width,
                    self.workbench_sidebar_width,
                    desired,
                );
                self.workbench_sidebar_width = sidebar_width;
                self.workbench_inspector_width = inspector_width;
            }
        }
    }

    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        let col = mouse.column;
        let row = mouse.row;
        let size = self.last_terminal_area;

        if size.width == 0 || size.height == 0 {
            return Ok(());
        }

        // --- Session tab mouse handling ---
        //
        // When the focused PTY application has enabled mouse tracking (e.g.
        // Claude Code running in the alternate screen), mouse events are
        // encoded as escape sequences and forwarded to the PTY so the app
        // can handle its own scrolling and interactive elements.
        //
        // When mouse tracking is disabled (e.g. a plain shell), Claustre
        // handles events itself for scrollback and text selection.
        if self.active_tab > 0 {
            // Tab bar click: always handled by Claustre regardless of mouse mode
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                let has_tab_bar = self.tabs.len() > 1;
                if has_tab_bar && row == 0 {
                    let layout = ui::compute_tab_layout(&self.tabs, self.active_tab, size.width);
                    for entry in &layout.entries {
                        if col >= entry.x_start && col < entry.x_start + entry.width {
                            self.active_tab = entry.tab_index;
                            return Ok(());
                        }
                    }
                    return Ok(());
                }
            }

            // Determine target pane and check mouse protocol.
            // `should_forward_mouse()` returns false when the process has
            // exited — preventing scroll events from being silently consumed
            // by a dead process while the parser retains stale mouse mode.
            let coords = self.screen_to_terminal_coords(col, row);
            let mouse_forwarded = if let Some((pane_id, vt_row, vt_col)) = coords
                && let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                && let Some(term) = terminals.terminal(pane_id)
                && term.should_forward_mouse()
            {
                let encoding = term.mouse_protocol_encoding();
                if let Some(bytes) = encode_mouse_event(&mouse.kind, vt_col, vt_row, encoding) {
                    // Focus the clicked pane on button press
                    if matches!(mouse.kind, MouseEventKind::Down(_)) {
                        terminals.focused = pane_id;
                    }
                    terminals.selection = None;
                    if let Some(term) = terminals.terminal_mut(pane_id) {
                        let _ = term.send_bytes(&bytes);
                    }
                }
                true
            } else {
                false
            };

            if mouse_forwarded {
                return Ok(());
            }

            // Mouse tracking disabled — use Claustre's own handling
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    // Click inside a terminal pane: start selection
                    if let Some((pane, vt_row, vt_col)) = coords {
                        if let Some(Tab::Session { terminals, .. }) =
                            self.tabs.get_mut(self.active_tab)
                        {
                            terminals.focused = pane;
                            terminals.selection = Some(crate::pty::Selection {
                                pane,
                                start: (vt_row, vt_col),
                                end: (vt_row, vt_col),
                            });
                        }
                    } else {
                        // Click outside panes: clear selection
                        if let Some(Tab::Session { terminals, .. }) =
                            self.tabs.get_mut(self.active_tab)
                        {
                            terminals.selection = None;
                        }
                    }
                    return Ok(());
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    // Compute pane areas before mutable borrow
                    let pane_areas = self.session_pane_inner_areas();
                    if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                        && let Some(ref mut sel) = terminals.selection
                        && let Some((_, inner)) = pane_areas.iter().find(|(id, _)| *id == sel.pane)
                    {
                        let vt_row = row
                            .saturating_sub(inner.y)
                            .min(inner.height.saturating_sub(1));
                        let vt_col = col
                            .saturating_sub(inner.x)
                            .min(inner.width.saturating_sub(1));
                        sel.end = (vt_row, vt_col);
                    }
                    return Ok(());
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    // Copy selected text to clipboard only if user actually
                    // dragged (start != end).  A plain click (down + up on the
                    // same cell) should behave like a normal terminal: reposition
                    // focus without copying anything.
                    if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                    {
                        // Copy selection (Selection is Copy) to release the
                        // immutable borrow on `terminals`, allowing mutable
                        // access to the terminal for scrollback adjustment.
                        let should_clear = if let Some(sel) = terminals.selection {
                            if sel.start == sel.end {
                                // Plain click — no drag occurred
                                true
                            } else if let Some(term) = terminals.terminal_mut(sel.pane) {
                                // Set the parser to the user's scroll offset so
                                // screen.cell() reads from the scrolled viewport
                                // (not the live screen).  Without this, copying
                                // while scrolled back extracts text from the
                                // bottom of the output instead of the visible
                                // region.
                                term.prepare_for_render();
                                let text = sel.extract_text(term.screen());
                                term.restore_after_render();
                                if !text.is_empty()
                                    && let Ok(mut clipboard) = arboard::Clipboard::new()
                                {
                                    let _ = clipboard.set_text(&text);
                                }
                                false
                            } else {
                                true
                            }
                        } else {
                            false
                        };
                        if should_clear {
                            terminals.selection = None;
                        }
                    }
                    return Ok(());
                }
                MouseEventKind::ScrollUp => {
                    if let Some((pane_id, _, _)) = coords
                        && let Some(Tab::Session { terminals, .. }) =
                            self.tabs.get_mut(self.active_tab)
                        && let Some(term) = terminals.terminal_mut(pane_id)
                    {
                        term.scroll_up(5);
                    }
                    return Ok(());
                }
                MouseEventKind::ScrollDown => {
                    if let Some((pane_id, _, _)) = coords
                        && let Some(Tab::Session { terminals, .. }) =
                            self.tabs.get_mut(self.active_tab)
                        && let Some(term) = terminals.terminal_mut(pane_id)
                    {
                        term.scroll_down(5);
                    }
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }

        // --- Dashboard events ---
        if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left))
            && self.workbench_drag_target.is_some()
        {
            self.workbench_drag_target = None;
            return Ok(());
        }

        if matches!(mouse.kind, MouseEventKind::Drag(MouseButton::Left))
            && self.workbench_drag_target.is_some()
        {
            self.update_workbench_drag(col);
            return Ok(());
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {}
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                if self.input_mode == InputMode::Normal {
                    // Position-aware scroll: check if cursor is over the inspector
                    let layout = ui::compute_workbench_layout(
                        self.workbench_area(),
                        self.workbench_sidebar_width,
                        self.workbench_inspector_width,
                        ui::workbench_uses_persistent_inspector(self.workbench_view),
                    );
                    let over_inspector = layout.inspector.width > 0
                        && col >= layout.inspector.x
                        && col < layout.inspector.x.saturating_add(layout.inspector.width)
                        && row >= layout.inspector.y
                        && row < layout.inspector.y.saturating_add(layout.inspector.height);

                    if over_inspector {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => {
                                self.inspector_scroll = self.inspector_scroll.saturating_sub(3);
                            }
                            MouseEventKind::ScrollDown => {
                                self.inspector_scroll = self.inspector_scroll.saturating_add(3);
                            }
                            _ => {}
                        }
                    } else {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => self.move_up(),
                            MouseEventKind::ScrollDown => self.move_down(),
                            _ => {}
                        }
                    }
                }
                return Ok(());
            }
            _ => return Ok(()),
        }

        let has_tab_bar = self.tabs.len() > 1;

        // --- Tab bar click (top row, only when visible) ---
        if has_tab_bar && row == 0 {
            // Use the same layout computation as draw_tab_bar
            let layout = ui::compute_tab_layout(&self.tabs, self.active_tab, size.width);
            for entry in &layout.entries {
                if col >= entry.x_start && col < entry.x_start + entry.width {
                    self.active_tab = entry.tab_index;
                    return Ok(());
                }
            }
            return Ok(());
        }

        // --- Dashboard: only handle clicks in Normal mode ---
        if self.input_mode != InputMode::Normal {
            return Ok(());
        }

        let layout = ui::compute_workbench_layout(
            self.workbench_area(),
            self.workbench_sidebar_width,
            self.workbench_inspector_width,
            ui::workbench_uses_persistent_inspector(self.workbench_view),
        );

        let body_bottom = layout.body.y.saturating_add(layout.body.height);
        if row < layout.body.y || row >= body_bottom {
            return Ok(());
        }

        let sidebar_divider_start = layout.sidebar.x.saturating_add(layout.sidebar.width);
        let inspector_divider_start = layout.main.x.saturating_add(layout.main.width);
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && col.saturating_add(1) >= sidebar_divider_start
            && col <= sidebar_divider_start
        {
            self.begin_workbench_drag(WorkbenchDragTarget::SidebarDivider);
            return Ok(());
        }
        if ui::workbench_uses_persistent_inspector(self.workbench_view)
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && col.saturating_add(1) >= inspector_divider_start
            && col <= inspector_divider_start
        {
            self.begin_workbench_drag(WorkbenchDragTarget::InspectorDivider);
            return Ok(());
        }

        if col >= layout.sidebar.x && col < layout.sidebar.x.saturating_add(layout.sidebar.width) {
            self.focus = Focus::Projects;
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                let inner = Rect::new(
                    layout.sidebar.x.saturating_add(1),
                    layout.sidebar.y.saturating_add(1),
                    layout.sidebar.width.saturating_sub(2),
                    layout.sidebar.height.saturating_sub(2),
                );
                if row >= inner.y
                    && row < inner.y.saturating_add(inner.height)
                    && col >= inner.x
                    && col < inner.x.saturating_add(inner.width)
                {
                    let clicked_row = (row - inner.y) as usize;
                    let nav_header = 1usize;
                    let nav_start = nav_header;
                    let nav_end = nav_start + WorkbenchView::ALL.len();
                    if (nav_start..nav_end).contains(&clicked_row) {
                        self.sidebar_cursor = clicked_row - nav_start;
                    } else {
                        let repo_start = nav_end + 2;
                        let repo_end = repo_start + self.projects.len();
                        if (repo_start..repo_end).contains(&clicked_row) {
                            self.sidebar_cursor =
                                Self::sidebar_nav_count() + (clicked_row - repo_start);
                        }
                    }
                }
            }
            return Ok(());
        }

        if col >= layout.inspector.x
            && col < layout.inspector.x.saturating_add(layout.inspector.width)
        {
            self.focus = Focus::Inspector;
            return Ok(());
        }

        if col >= layout.main.x && col < layout.main.x.saturating_add(layout.main.width) {
            self.focus = Focus::Tasks;
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if self.workbench_view == WorkbenchView::Settings {
                    let main_inner = Rect::new(
                        layout.main.x.saturating_add(1),
                        layout.main.y.saturating_add(1),
                        layout.main.width.saturating_sub(2),
                        layout.main.height.saturating_sub(2),
                    );
                    let sections = ratatui::layout::Layout::default()
                        .direction(ratatui::layout::Direction::Horizontal)
                        .constraints([Constraint::Length(28), Constraint::Min(0)])
                        .split(main_inner);
                    if col >= sections[0].x
                        && col < sections[0].x.saturating_add(sections[0].width)
                        && row >= sections[0].y
                        && row < sections[0].y.saturating_add(sections[0].height)
                    {
                        let inner = Rect::new(
                            sections[0].x.saturating_add(1),
                            sections[0].y,
                            sections[0].width.saturating_sub(1),
                            sections[0].height,
                        );
                        if row >= inner.y
                            && row < inner.y.saturating_add(inner.height)
                            && col >= inner.x
                            && col < inner.x.saturating_add(inner.width)
                        {
                            let clicked_row = (row - inner.y) as usize;
                            if clicked_row > 0 {
                                let idx = clicked_row - 1;
                                if idx < SettingsSection::ALL.len() {
                                    self.settings_section_index = idx;
                                    self.maybe_prefetch_github_installations();
                                }
                            }
                        }
                    }
                } else if matches!(
                    self.workbench_view,
                    WorkbenchView::MyTasks | WorkbenchView::Reviews
                ) {
                    let inner = Rect::new(
                        layout.main.x.saturating_add(1),
                        layout.main.y.saturating_add(1),
                        layout.main.width.saturating_sub(2),
                        layout.main.height.saturating_sub(2),
                    );
                    if row >= inner.y && row < inner.y.saturating_add(inner.height) {
                        let clicked_row = (row - inner.y) as usize;
                        if self.workbench_view == WorkbenchView::Reviews {
                            if clicked_row == 0 {
                                let tab_row = row;
                                let authored_width = 18u16;
                                let needs_width = 18u16;
                                let tabs_x = inner.x.saturating_add(2);
                                if col >= tabs_x && col < tabs_x.saturating_add(authored_width) {
                                    self.review_queue_tab = ReviewQueueTab::Authored;
                                    self.review_index = 0;
                                } else if col
                                    >= tabs_x.saturating_add(authored_width).saturating_add(1)
                                    && col
                                        < tabs_x
                                            .saturating_add(authored_width)
                                            .saturating_add(1)
                                            .saturating_add(needs_width)
                                {
                                    self.review_queue_tab = ReviewQueueTab::NeedsReview;
                                    self.review_index = 0;
                                } else {
                                    let _ = tab_row;
                                }
                            } else {
                                let list_row = clicked_row - 1;
                                if list_row < self.review_items().len() {
                                    self.review_index = list_row;
                                }
                            }
                        } else {
                            let visible_count = if self.uses_github_my_tasks() {
                                self.visible_my_task_count()
                            } else {
                                self.visible_task_count()
                            };
                            if clicked_row < visible_count {
                                self.task_index = clicked_row;
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub(super) fn handle_normal_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        // N (Shift+N) to launch ad-hoc session from any view
        if matches!(code, KeyCode::Char('N')) && self.focus == Focus::Tasks {
            self.open_adhoc_launch_modal()?;
            return Ok(());
        }
        // R (Shift+R) to sync GitHub from any view
        if matches!(code, KeyCode::Char('R')) && self.focus == Focus::Tasks {
            self.refresh_board_issues();
            return Ok(());
        }
        if modifiers.is_empty()
            && self.workbench_view == WorkbenchView::Reviews
            && self.focus == Focus::Tasks
            && matches!(code, KeyCode::Char('t'))
        {
            self.review_queue_tab = match self.review_queue_tab {
                ReviewQueueTab::Authored => ReviewQueueTab::NeedsReview,
                ReviewQueueTab::NeedsReview => ReviewQueueTab::Authored,
            };
            self.review_index = 0;
            self.inspector_scroll = 0;
            self.refresh_review_selected_context();
            return Ok(());
        }
        if modifiers.is_empty()
            && self.workbench_view == WorkbenchView::Threads
            && self.focus == Focus::Tasks
            && matches!(code, KeyCode::Char('i'))
        {
            self.start_thread_compose()?;
            return Ok(());
        }
        if modifiers.is_empty()
            && self.workbench_view == WorkbenchView::Threads
            && self.focus == Focus::Tasks
            && matches!(code, KeyCode::Char('m'))
        {
            self.open_thread_provider_picker()?;
            return Ok(());
        }
        if modifiers.is_empty()
            && matches!(code, KeyCode::Char('P' | 'p'))
            && self.workbench_view != WorkbenchView::Settings
        {
            self.open_project_picker()?;
            return Ok(());
        }
        if modifiers.is_empty() && self.handle_settings_shortcuts(code)? {
            return Ok(());
        }
        if modifiers.is_empty() && self.handle_workbench_prefix(code)? {
            return Ok(());
        }
        if let Some(action) = self.keymap.lookup_normal(code, modifiers) {
            self.execute_action(action)?;
            return Ok(());
        }
        if modifiers.is_empty()
            && self.workbench_view == WorkbenchView::Threads
            && self.focus == Focus::Tasks
            && matches!(code, KeyCode::Char(_))
        {
            self.start_thread_compose()?;
            self.handle_thread_compose_key(code, modifiers)?;
        }
        Ok(())
    }

    fn handle_workbench_prefix(&mut self, code: KeyCode) -> Result<bool> {
        let show_inspector = ui::workbench_uses_persistent_inspector(self.workbench_view);
        if self.leader_pending {
            self.leader_pending = false;
            match code {
                KeyCode::Char('t') => self.switch_workbench_view(WorkbenchView::MyTasks),
                KeyCode::Char('b') => self.switch_workbench_view(WorkbenchView::SprintBoard),
                KeyCode::Char('r') => self.switch_workbench_view(WorkbenchView::Reviews),
                KeyCode::Char('a') => self.switch_workbench_view(WorkbenchView::Agents),
                KeyCode::Char('s') => self.switch_workbench_view(WorkbenchView::Settings),
                KeyCode::Char('g') => {
                    self.switch_workbench_view(WorkbenchView::Settings);
                    self.settings_section_index = SettingsSection::ALL
                        .iter()
                        .position(|section| *section == SettingsSection::GitHub)
                        .unwrap_or(self.settings_section_index);
                    self.maybe_prefetch_github_installations();
                }
                KeyCode::Char('p') => {
                    self.switch_workbench_view(WorkbenchView::Settings);
                    self.settings_section_index = SettingsSection::ALL
                        .iter()
                        .position(|section| *section == SettingsSection::AiProviders)
                        .unwrap_or(self.settings_section_index);
                }
                _ => {}
            }
            return Ok(true);
        }

        if show_inspector && self.focus == Focus::Inspector {
            match code {
                KeyCode::Char('f') => {
                    self.inspector_expanded = !self.inspector_expanded;
                    return Ok(true);
                }
                KeyCode::Char('v') if self.workbench_view == WorkbenchView::Reviews => {
                    // Open review drawer from inspector focus
                    self.review_drawer_tab = super::ReviewDrawerTab::Description;
                    self.review_drawer_scroll = 0;
                    self.input_mode = InputMode::ReviewDrawer;
                    return Ok(true);
                }
                KeyCode::Char('1') => {
                    self.inspector_tab = InspectorTab::Issue;
                    self.inspector_scroll = 0;
                    return Ok(true);
                }
                KeyCode::Char('2') => {
                    self.inspector_tab = InspectorTab::Diff;
                    self.inspector_scroll = 0;
                    return Ok(true);
                }
                KeyCode::Char('3') => {
                    self.inspector_tab = InspectorTab::Runtime;
                    self.inspector_scroll = 0;
                    return Ok(true);
                }
                KeyCode::Char('4') => {
                    self.inspector_tab = InspectorTab::Tests;
                    self.inspector_scroll = 0;
                    return Ok(true);
                }
                KeyCode::Char('5') => {
                    self.inspector_tab = InspectorTab::Plan;
                    self.inspector_scroll = 0;
                    return Ok(true);
                }
                KeyCode::Char('6') => {
                    self.inspector_tab = InspectorTab::Attachments;
                    self.inspector_scroll = 0;
                    return Ok(true);
                }
                KeyCode::Esc if self.inspector_expanded => {
                    self.inspector_expanded = false;
                    return Ok(true);
                }
                _ => {}
            }
        }

        match code {
            KeyCode::Char(' ') => {
                self.leader_pending = true;
                Ok(true)
            }
            KeyCode::Tab => {
                self.focus = if show_inspector {
                    match self.focus {
                        Focus::Projects => Focus::Tasks,
                        Focus::Tasks => Focus::Inspector,
                        Focus::Inspector => Focus::Projects,
                    }
                } else {
                    match self.focus {
                        Focus::Projects => Focus::Tasks,
                        Focus::Tasks | Focus::Inspector => Focus::Projects,
                    }
                };
                Ok(true)
            }
            KeyCode::BackTab => {
                self.focus = if show_inspector {
                    match self.focus {
                        Focus::Projects => Focus::Inspector,
                        Focus::Tasks => Focus::Projects,
                        Focus::Inspector => Focus::Tasks,
                    }
                } else {
                    match self.focus {
                        Focus::Projects | Focus::Inspector => Focus::Tasks,
                        Focus::Tasks => Focus::Projects,
                    }
                };
                Ok(true)
            }
            KeyCode::Char('3') if show_inspector => {
                self.focus = Focus::Inspector;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn switch_workbench_view(&mut self, view: WorkbenchView) {
        self.workbench_view = view;
        self.sidebar_cursor = WorkbenchView::ALL
            .iter()
            .position(|candidate| *candidate == view)
            .unwrap_or(0);
        self.inspector_scroll = 0;
        self.focus = Focus::Tasks;
        if view == WorkbenchView::SprintBoard {
            self.board_column_index = 0;
            self.board_issue_index = 0;
            self.board_first_load = true;
            self.board_filter.clear();
            self.board_filter_cursor = 0;
            self.load_board_issues_from_cache();
            self.input_mode = InputMode::BoardView;
        } else if view == WorkbenchView::Settings
            && SettingsSection::ALL
                .get(self.settings_section_index)
                .copied()
                == Some(SettingsSection::GitHub)
            && self.github_status.authenticated
            && self.github_installations.is_empty()
        {
            self.maybe_prefetch_github_installations();
            self.input_mode = InputMode::Normal;
        } else if matches!(
            self.input_mode,
            InputMode::BoardView
                | InputMode::MilestoneFilter
                | InputMode::BoardFilter
                | InputMode::LaunchThread
                | InputMode::ThreadCompose
                | InputMode::SettingsEdit
                | InputMode::GitHubInstallationPicker
                | InputMode::GitHubProjectPicker
        ) {
            self.input_mode = InputMode::Normal;
            self.thread_compose_thread_id = None;
            self.thread_compose_buffer.clear();
            self.thread_compose_cursor = 0;
        }
    }

    fn handle_settings_shortcuts(&mut self, code: KeyCode) -> Result<bool> {
        if self.workbench_view != WorkbenchView::Settings || self.focus != Focus::Tasks {
            return Ok(false);
        }

        let section = SettingsSection::ALL
            .get(self.settings_section_index)
            .copied()
            .unwrap_or(SettingsSection::AiProviders);

        match code {
            KeyCode::Enter => {
                self.activate_settings_section(section)?;
                Ok(true)
            }
            KeyCode::Char('m') if section == SettingsSection::AiProviders => {
                self.open_settings_picker(
                    SettingsEditTarget::ClaudeModel,
                    vec![
                        "claude-opus-4-6".to_string(),
                        "claude-sonnet-4-6".to_string(),
                        "claude-haiku-4-5-20251001".to_string(),
                    ],
                );
                Ok(true)
            }
            KeyCode::Char('e') if section == SettingsSection::AiProviders => {
                self.open_settings_picker(
                    SettingsEditTarget::ClaudeEffort,
                    vec![
                        "min".to_string(),
                        "low".to_string(),
                        "medium".to_string(),
                        "high".to_string(),
                        "max".to_string(),
                    ],
                );
                Ok(true)
            }
            KeyCode::Char('r') if section == SettingsSection::AiProviders => {
                self.config.remote_enabled = !self.config.remote_enabled;
                self.persist_settings_config("Claude remote mode")?;
                Ok(true)
            }
            KeyCode::Char('u') if section == SettingsSection::AiProviders => {
                self.config.auto_update = !self.config.auto_update;
                self.persist_settings_config("Auto-update")?;
                Ok(true)
            }
            KeyCode::Char('p') if section == SettingsSection::Layout => {
                self.start_settings_edit(SettingsEditTarget::KeymapPreset);
                Ok(true)
            }
            KeyCode::Char('l') if section == SettingsSection::Layout => {
                self.start_settings_edit(SettingsEditTarget::KeymapLeader);
                Ok(true)
            }
            KeyCode::Char('e') if section == SettingsSection::Notifications => {
                self.config.notifications.enabled = !self.config.notifications.enabled;
                self.persist_settings_config("Voice notifications")?;
                Ok(true)
            }
            KeyCode::Char('s') if section == SettingsSection::Notifications => {
                self.config.notifications.system = !self.config.notifications.system;
                self.persist_settings_config("System banner notifications")?;
                Ok(true)
            }
            KeyCode::Char('c') if section == SettingsSection::Notifications => {
                self.start_settings_edit(SettingsEditTarget::NotificationCommand);
                Ok(true)
            }
            KeyCode::Char('t') if section == SettingsSection::Notifications => {
                self.start_settings_edit(SettingsEditTarget::NotificationTemplate);
                Ok(true)
            }
            KeyCode::Char('a') if section == SettingsSection::Notifications => {
                self.config.notifications.rules.ask_user =
                    !self.config.notifications.rules.ask_user;
                self.persist_settings_config("Ask-user notifications")?;
                Ok(true)
            }
            KeyCode::Char('p') if section == SettingsSection::Notifications => {
                self.config.notifications.rules.approval_required =
                    !self.config.notifications.rules.approval_required;
                self.persist_settings_config("Approval notifications")?;
                Ok(true)
            }
            KeyCode::Char('r') if section == SettingsSection::Notifications => {
                self.config.notifications.rules.run_complete =
                    !self.config.notifications.rules.run_complete;
                self.persist_settings_config("Run complete notifications")?;
                Ok(true)
            }
            KeyCode::Char('v') if section == SettingsSection::Notifications => {
                self.config.notifications.rules.review_requested =
                    !self.config.notifications.rules.review_requested;
                self.persist_settings_config("Review-requested notifications")?;
                Ok(true)
            }
            KeyCode::Char('f') if section == SettingsSection::Notifications => {
                self.config.notifications.rules.ci_failed =
                    !self.config.notifications.rules.ci_failed;
                self.persist_settings_config("CI-failed notifications")?;
                Ok(true)
            }
            KeyCode::Char('i') if section == SettingsSection::ReviewLoop => {
                self.start_settings_edit(SettingsEditTarget::ReviewLoopInterval);
                Ok(true)
            }
            KeyCode::Char('p') if section == SettingsSection::ReviewLoop => {
                self.start_settings_edit(SettingsEditTarget::ReviewLoopPrompt);
                Ok(true)
            }
            KeyCode::Char('p') if section == SettingsSection::Sandbox => {
                self.start_settings_edit(SettingsEditTarget::RuntimeDefaultProfile);
                Ok(true)
            }
            KeyCode::Char('s') if section == SettingsSection::Sandbox => {
                self.start_settings_edit(SettingsEditTarget::RuntimeSandboxPath);
                Ok(true)
            }
            KeyCode::Char('a') if section == SettingsSection::Sandbox => {
                self.start_settings_edit(SettingsEditTarget::RuntimeAttachmentsDir);
                Ok(true)
            }
            KeyCode::Char('J') if section == SettingsSection::Workflows => {
                let count = self.store.list_workflow_defs().map_or(0, |d| d.len());
                if count > 0 {
                    self.settings_workflow_index =
                        (self.settings_workflow_index + 1).min(count.saturating_sub(1));
                }
                Ok(true)
            }
            KeyCode::Char('K') if section == SettingsSection::Workflows => {
                self.settings_workflow_index = self.settings_workflow_index.saturating_sub(1);
                Ok(true)
            }
            KeyCode::Char('n') if section == SettingsSection::Workflows => {
                // Create a minimal custom workflow with one stage
                let name = format!("custom_{}", chrono::Utc::now().timestamp());
                let definition = crate::workflows::WorkflowDefinition {
                    name: name.clone(),
                    description: Some("New custom workflow".to_string()),
                    stages: vec![crate::workflows::WorkflowStageDefinition {
                        name: "plan".to_string(),
                        prompt_template: Some("/plan".to_string()),
                        provider: Some("claude".to_string()),
                        provider_profile: Some("default".to_string()),
                        runtime_profile: None,
                        gate: None,
                        outputs: vec![],
                    }],
                };
                match crate::workflows::save_workflow_definition(&definition) {
                    Ok(()) => {
                        let _ = crate::workflows::sync_workflow_definitions(&self.store, None);
                        self.refresh_available_workflow_names();
                        self.show_toast(format!("Created workflow: {name}"), ToastStyle::Success);
                    }
                    Err(err) => {
                        self.show_toast(
                            format!("Failed to create workflow: {err:#}"),
                            ToastStyle::Error,
                        );
                    }
                }
                Ok(true)
            }
            KeyCode::Char('d') if section == SettingsSection::Workflows => {
                // Delete the currently selected custom workflow
                let defs = self.store.list_workflow_defs().unwrap_or_default();
                if let Some(def) = defs.get(self.settings_workflow_index) {
                    if crate::workflows::is_builtin_workflow(&def.name) {
                        self.show_toast("Cannot delete built-in workflows", ToastStyle::Info);
                    } else {
                        let name = def.name.clone();
                        match crate::workflows::delete_workflow_definition(&name) {
                            Ok(true) => {
                                let _ =
                                    crate::workflows::sync_workflow_definitions(&self.store, None);
                                self.refresh_available_workflow_names();
                                // Adjust selection if we deleted the last item
                                let new_count =
                                    self.store.list_workflow_defs().map_or(0, |d| d.len());
                                if self.settings_workflow_index >= new_count && new_count > 0 {
                                    self.settings_workflow_index = new_count - 1;
                                }
                                self.show_toast(
                                    format!("Deleted workflow: {name}"),
                                    ToastStyle::Success,
                                );
                            }
                            Ok(false) => {
                                self.show_toast("Workflow file not found", ToastStyle::Info);
                            }
                            Err(err) => {
                                self.show_toast(
                                    format!("Failed to delete: {err:#}"),
                                    ToastStyle::Error,
                                );
                            }
                        }
                    }
                }
                Ok(true)
            }
            KeyCode::Char('e') if section == SettingsSection::Workflows => {
                // Edit the selected workflow's name (custom only)
                let defs = self.store.list_workflow_defs().unwrap_or_default();
                if let Some(def) = defs.get(self.settings_workflow_index) {
                    if crate::workflows::is_builtin_workflow(&def.name) {
                        self.show_toast("Cannot edit built-in workflows", ToastStyle::Info);
                    } else {
                        // Use the input buffer for inline editing
                        self.input_buffer = def.name.clone();
                        self.input_cursor = self.input_buffer.len();
                        self.input_mode = InputMode::SettingsEdit;
                        self.settings_edit_target =
                            Some(SettingsEditTarget::WorkflowName);
                    }
                }
                Ok(true)
            }
            KeyCode::Char('a') if section == SettingsSection::General => {
                self.config.sync.auto_push = !self.config.sync.auto_push;
                self.persist_settings_config("Sync auto-push")?;
                Ok(true)
            }
            KeyCode::Char('r') if section == SettingsSection::General => {
                self.config.rtk.enabled = !self.config.rtk.enabled;
                self.persist_settings_config("RTK integration")?;
                Ok(true)
            }
            KeyCode::Char('a') if section == SettingsSection::GitHub => {
                if crate::github_app::app_auth_available(&self.config.github_app)
                    || self.tool_status.gh
                {
                    self.spawn_github_auth();
                    self.show_toast(
                        if crate::github_app::app_auth_available(&self.config.github_app) {
                            "Starting GitHub App device flow..."
                        } else if self.github_status.gh_authenticated
                            && !self.github_status.gh_has_project_scope
                        {
                            "Refreshing GitHub CLI permissions for Projects v2..."
                        } else {
                            "Starting GitHub CLI browser login..."
                        },
                        ToastStyle::Info,
                    );
                } else {
                    self.show_toast(
                        "Install GitHub CLI or configure a custom GitHub App",
                        ToastStyle::Error,
                    );
                }
                Ok(true)
            }
            KeyCode::Char('f') if section == SettingsSection::GitHub => {
                self.github_status =
                    crate::github_app::local_status(&self.config.github_app).unwrap_or_default();
                if self.github_status.authenticated {
                    self.spawn_github_installations_fetch();
                    self.show_toast("Refreshing GitHub installations...", ToastStyle::Info);
                } else if self.github_status.gh_authenticated {
                    self.show_toast("Refreshed GitHub CLI status", ToastStyle::Success);
                } else {
                    self.show_toast("Connect GitHub first", ToastStyle::Info);
                }
                Ok(true)
            }
            KeyCode::Char('x') if section == SettingsSection::GitHub => {
                self.github_auth_prompt = None;
                self.github_auth_error = None;
                self.github_installations.clear();
                self.github_installation_index = 0;
                if self.github_status.authenticated {
                    crate::github_app::clear_session()?;
                } else if self.github_status.gh_authenticated {
                    crate::github_app::github_cli_disconnect()?;
                } else {
                    self.show_toast("GitHub is already disconnected", ToastStyle::Info);
                    return Ok(true);
                }
                self.github_status =
                    crate::github_app::local_status(&self.config.github_app).unwrap_or_default();
                self.show_toast("GitHub session cleared", ToastStyle::Success);
                Ok(true)
            }
            KeyCode::Char('o') if section == SettingsSection::GitHub => {
                if let Some(url) = crate::github_app::install_url(&self.config.github_app) {
                    crate::github_app::open_in_browser(&url)?;
                    self.show_toast("Opened GitHub App install page", ToastStyle::Success);
                } else {
                    self.show_toast(
                        "No bundled GitHub App install page is available in this build",
                        ToastStyle::Info,
                    );
                }
                Ok(true)
            }
            KeyCode::Char('y') if section == SettingsSection::GitHub => {
                if let Some(project) = self.selected_project().cloned() {
                    let summary = crate::github::sync_project_repo(&self.store, &project)?;
                    self.show_toast(
                        format!(
                            "Synced {} issues, {} PRs, {} projects, {} project items",
                            summary.issues_synced,
                            summary.prs_synced,
                            summary.projects_synced,
                            summary.project_items_synced
                        ),
                        ToastStyle::Success,
                    );
                }
                Ok(true)
            }
            KeyCode::Char('c') if section == SettingsSection::GitHub => {
                self.start_settings_edit(SettingsEditTarget::ClientId);
                Ok(true)
            }
            KeyCode::Char('s') if section == SettingsSection::GitHub => {
                self.start_settings_edit(SettingsEditTarget::AppSlug);
                Ok(true)
            }
            KeyCode::Char('u') if section == SettingsSection::GitHub => {
                self.start_settings_edit(SettingsEditTarget::InstallUrl);
                Ok(true)
            }
            KeyCode::Char('r') if section == SettingsSection::GitHub => {
                self.start_settings_edit(SettingsEditTarget::RelayUrl);
                Ok(true)
            }
            KeyCode::Char('i') if section == SettingsSection::GitHub => {
                self.start_settings_edit(SettingsEditTarget::InstallationId);
                Ok(true)
            }
            KeyCode::Char('g' | 'p') if section == SettingsSection::GitHub => {
                self.open_github_project_picker()?;
                Ok(true)
            }
            KeyCode::Char('.') if section == SettingsSection::GitHub => {
                self.github_settings_show_advanced = !self.github_settings_show_advanced;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn start_settings_edit(&mut self, target: SettingsEditTarget) {
        self.settings_edit_target = Some(target);
        self.input_buffer = target.current_value(&self.config);
        self.input_cursor = self.input_buffer.len();
        self.input_mode = InputMode::SettingsEdit;
    }

    fn open_settings_picker(&mut self, target: SettingsEditTarget, options: Vec<String>) {
        let current = target.current_value(&self.config);
        let index = options
            .iter()
            .position(|opt| opt.eq_ignore_ascii_case(&current))
            .unwrap_or(0);
        self.settings_picker_target = Some(target);
        self.settings_picker_options = options;
        self.settings_picker_index = index;
        self.input_mode = InputMode::SettingsPicker;
    }

    fn open_github_installation_picker(&mut self) {
        if self.github_installations.is_empty() {
            self.spawn_github_installations_fetch();
            self.show_toast("Loading GitHub installations...", ToastStyle::Info);
            return;
        }

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
        }
        self.input_mode = InputMode::GitHubInstallationPicker;
    }

    fn open_github_project_picker(&mut self) -> Result<()> {
        self.load_board_issues_from_cache();

        if self.github_projects_v2.is_empty() {
            self.show_toast("Sync a GitHub project first", ToastStyle::Info);
            return Ok(());
        }

        if let Some(default_project_id) = self.config.github_app.default_project_id.as_deref()
            && let Some(index) = self
                .github_projects_v2
                .iter()
                .position(|project| project.project_number.to_string() == default_project_id)
        {
            self.github_project_index = index;
        }

        self.input_buffer.clear();
        self.input_cursor = 0;
        self.input_mode = InputMode::GitHubProjectPicker;
        Ok(())
    }

    fn open_project_picker(&mut self) -> Result<()> {
        if self.projects.is_empty() {
            self.show_toast("Add a repository first", ToastStyle::Info);
            return Ok(());
        }

        self.project_picker_index = self
            .project_index
            .min(self.projects.len().saturating_sub(1));
        self.input_mode = InputMode::ProjectPicker;
        Ok(())
    }

    fn thread_provider_picker_target(&self) -> Option<crate::store::Thread> {
        if self.active_session_uses_thread_workspace() {
            self.active_session_thread()
        } else {
            self.selected_thread().cloned()
        }
    }

    fn open_thread_provider_picker(&mut self) -> Result<()> {
        let Some(thread) = self.thread_provider_picker_target() else {
            self.show_toast("Select a thread first", ToastStyle::Info);
            return Ok(());
        };

        self.thread_provider_picker_index = LAUNCH_THREAD_PROVIDERS
            .iter()
            .position(|provider| *provider == thread.provider_kind)
            .unwrap_or(0);
        self.input_mode = InputMode::ThreadProviderPicker;
        Ok(())
    }

    fn switch_thread_provider_interactive(
        &mut self,
        thread: &crate::store::Thread,
        provider_kind: crate::store::ProviderKind,
    ) -> Result<()> {
        let provider_profile = thread.provider_profile.as_deref().or(Some("default"));
        crate::threads::switch_thread_provider(
            &self.store,
            &thread.id,
            provider_kind,
            provider_profile,
        )?;
        self.store.create_thread_message(
            &thread.id,
            None,
            "system",
            &format!("Switched thread provider to {provider_kind}."),
            &[],
        )?;

        let mut live_switched = false;
        if let Some(session_id) = thread.session_id.as_deref()
            && let Some(session) = self
                .sessions
                .iter()
                .find(|candidate| candidate.id == session_id && candidate.closed_at.is_none())
                .cloned()
        {
            let command = crate::threads::build_thread_provider_command(
                &self.store,
                &self.config,
                &thread.id,
                provider_kind,
                provider_profile,
            )?;
            let wrapped = crate::session::wrap_cmd_with_shell_fallback(command);
            let mut cmd = portable_pty::CommandBuilder::new(&wrapped[0]);
            for arg in &wrapped[1..] {
                cmd.arg(arg);
            }
            cmd.cwd(&session.worktree_path);

            if let Some(Tab::Session {
                terminals,
                label,
                view_mode,
                ..
            }) = self.tabs.iter_mut().find(
                |tab| matches!(tab, Tab::Session { session_id: sid, .. } if sid == session_id),
            ) {
                terminals.replace_claude_command(cmd, &provider_kind.to_string())?;
                if let Some((base, _)) = label.rsplit_once(" [") {
                    *label = format!("{base} [{provider_kind}]");
                } else {
                    *label = format!("{label} [{provider_kind}]");
                }
                *view_mode = SessionTabView::Conversation;
                live_switched = true;
            }
            self.store.update_session_status(
                &session.id,
                crate::store::ClaudeStatus::Working,
                &format!("Switched to {provider_kind}"),
            )?;
        }

        if let Some(run) = self.store.list_thread_runs(&thread.id)?.into_iter().last() {
            let status = if live_switched {
                crate::store::ThreadRunStatus::Running
            } else {
                crate::store::ThreadRunStatus::Pending
            };
            self.store.update_thread_run_status(&run.id, status, None)?;
        }

        self.refresh_data()?;
        self.input_mode = InputMode::Normal;
        self.show_toast(
            if live_switched {
                format!("Switched live thread to {provider_kind}")
            } else {
                format!("Provider set to {provider_kind} — press l to continue")
            },
            ToastStyle::Success,
        );
        Ok(())
    }

    fn activate_settings_section(&mut self, section: SettingsSection) -> Result<()> {
        match section {
            SettingsSection::AiProviders => {
                self.open_settings_picker(
                    SettingsEditTarget::ClaudeModel,
                    vec![
                        "claude-opus-4-6".to_string(),
                        "claude-sonnet-4-6".to_string(),
                        "claude-haiku-4-5-20251001".to_string(),
                    ],
                );
            }
            SettingsSection::Permissions => {
                self.cached_config_status =
                    Some(crate::configure::load_config_status().map_err(|e| e.to_string()));
                self.input_mode = InputMode::ConfigureWizard;
            }
            SettingsSection::GitHub => {
                if self.github_status.authenticated {
                    self.open_github_installation_picker();
                } else if crate::github_app::app_auth_available(&self.config.github_app)
                    || self.tool_status.gh
                {
                    self.spawn_github_auth();
                    self.show_toast(
                        if crate::github_app::app_auth_available(&self.config.github_app) {
                            "Starting GitHub App device flow..."
                        } else {
                            "Starting GitHub CLI browser login..."
                        },
                        ToastStyle::Info,
                    );
                } else {
                    self.show_toast(
                        "Install GitHub CLI or configure a custom GitHub App",
                        ToastStyle::Error,
                    );
                }
            }
            SettingsSection::Layout => {
                self.start_settings_edit(SettingsEditTarget::KeymapPreset);
            }
            SettingsSection::Notifications => {
                self.config.notifications.enabled = !self.config.notifications.enabled;
                self.persist_settings_config("Voice notifications")?;
            }
            SettingsSection::ReviewLoop => {
                self.start_settings_edit(SettingsEditTarget::ReviewLoopInterval);
            }
            SettingsSection::Workflows => {
                // Enter on Workflows section — same as 'n' (new workflow)
                self.show_toast("Use n to create, d to delete workflows", ToastStyle::Info);
            }
            SettingsSection::Sandbox => {
                self.start_settings_edit(SettingsEditTarget::RuntimeDefaultProfile);
            }
            SettingsSection::General => {
                self.config.sync.auto_push = !self.config.sync.auto_push;
                self.persist_settings_config("Sync auto-push")?;
            }
        }
        Ok(())
    }

    fn persist_settings_config(&mut self, label: &str) -> Result<()> {
        crate::config::save_settings_config(&self.config)?;
        self.show_toast(format!("Saved {label}"), ToastStyle::Success);
        Ok(())
    }

    fn refresh_available_workflow_names(&mut self) {
        self.available_workflow_names = {
            let mut names = vec![String::new()];
            if let Ok(defs) = crate::workflows::load_workflow_definitions(None) {
                names.extend(defs.into_iter().map(|d| d.name));
            }
            names
        };
    }

    fn selected_launch_thread_field(&self) -> LaunchThreadField {
        LaunchThreadField::ALL
            .get(self.launch_thread_field_index)
            .copied()
            .unwrap_or(LaunchThreadField::Provider)
    }

    fn launch_thread_field_value(&self, field: LaunchThreadField) -> String {
        let Some(draft) = self.launch_thread_draft.as_ref() else {
            return String::new();
        };
        match field {
            LaunchThreadField::Provider => draft.provider_kind.to_string(),
            LaunchThreadField::RuntimeProfile => draft.runtime_profile.clone(),
            LaunchThreadField::Workflow => draft.workflow_name.clone(),
            LaunchThreadField::Title => draft.title.clone(),
            LaunchThreadField::ExtraContext => draft.extra_context.clone(),
        }
    }

    fn persist_launch_thread_input(&mut self) {
        let selected_field = self.selected_launch_thread_field();
        let Some(draft) = self.launch_thread_draft.as_mut() else {
            return;
        };
        match selected_field {
            LaunchThreadField::Provider | LaunchThreadField::Workflow => {
                // Picker fields — not text-editable, handled by cycle_* methods
            }
            LaunchThreadField::RuntimeProfile => {
                draft.runtime_profile.clone_from(&self.input_buffer);
            }
            LaunchThreadField::Title => {
                draft.title.clone_from(&self.input_buffer);
            }
            LaunchThreadField::ExtraContext => {
                draft.extra_context.clone_from(&self.input_buffer);
            }
        }
    }

    fn load_launch_thread_input(&mut self) {
        let value = self.launch_thread_field_value(self.selected_launch_thread_field());
        self.input_buffer = value;
        self.input_cursor = self.input_buffer.len();
    }

    fn cycle_launch_thread_provider(&mut self, forward: bool) {
        let Some(draft) = self.launch_thread_draft.as_mut() else {
            return;
        };
        let current = LAUNCH_THREAD_PROVIDERS
            .iter()
            .position(|provider| *provider == draft.provider_kind)
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % LAUNCH_THREAD_PROVIDERS.len()
        } else if current == 0 {
            LAUNCH_THREAD_PROVIDERS.len() - 1
        } else {
            current - 1
        };
        draft.provider_kind = LAUNCH_THREAD_PROVIDERS[next];
        self.load_launch_thread_input();
    }

    fn cycle_launch_thread_workflow(&mut self, forward: bool) {
        let Some(draft) = self.launch_thread_draft.as_mut() else {
            return;
        };
        if self.available_workflow_names.is_empty() {
            return;
        }
        let current = self
            .available_workflow_names
            .iter()
            .position(|name| *name == draft.workflow_name)
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % self.available_workflow_names.len()
        } else if current == 0 {
            self.available_workflow_names.len() - 1
        } else {
            current - 1
        };
        draft
            .workflow_name
            .clone_from(&self.available_workflow_names[next]);
        self.load_launch_thread_input();
    }

    fn open_task_launch_modal(&mut self) -> Result<()> {
        let Some(task) = self.selected_task() else {
            self.show_toast("Select a task first", ToastStyle::Info);
            return Ok(());
        };
        let draft = LaunchThreadDraft {
            task_id: Some(task.id.clone()),
            board_issue: None,
            review_pr: None,
            provider_kind: crate::store::ProviderKind::Claude,
            runtime_profile: self
                .config
                .runtime
                .default_profile
                .clone()
                .unwrap_or_default(),
            workflow_name: crate::threads::suggest_workflow(None, true),
            title: task.title.clone(),
            extra_context: String::new(),
        };
        self.launch_thread_draft = Some(draft);
        self.launch_thread_field_index = 0;
        self.load_launch_thread_input();
        self.input_mode = InputMode::LaunchThread;
        Ok(())
    }

    fn open_board_issue_launch_modal(&mut self) -> Result<()> {
        let Some(issue) = self
            .board_issues
            .get(self.board_column_index)
            .and_then(|column| column.get(self.board_issue_index))
            .cloned()
        else {
            self.show_toast("Select a sprint issue first", ToastStyle::Info);
            return Ok(());
        };

        // If this issue has a linked PR, redirect launch to the PR's branch.
        let linked_pr_launch = if issue.kind == crate::store::GitHubItemKind::Issue
            && issue.linked_pr_item_id.is_some()
        {
            issue.linked_pr_item_id.as_deref().and_then(|pr_item_id| {
                let linked_pr = self.store.get_github_item(pr_item_id).ok()?;
                let pr_cache = self.store.get_github_pr_cache(&linked_pr.id).ok();
                let body = pr_cache
                    .as_ref()
                    .and_then(|cache| cache.body.clone().or(cache.body_text.clone()))
                    .or_else(|| linked_pr.body_text.clone())
                    .unwrap_or_default();
                Some(PendingReviewPrLaunch {
                    github_item_id: linked_pr.id,
                    number: linked_pr.number,
                    title: linked_pr.title,
                    body,
                    url: linked_pr.url,
                    base_ref: pr_cache.as_ref().and_then(|cache| cache.base_ref.clone()),
                    head_ref: pr_cache.as_ref().and_then(|cache| cache.head_ref.clone()),
                    mode: PendingReviewLaunchMode::ContinueWork,
                })
            })
        } else {
            None
        };

        // Suggest workflow before fields are moved into the draft
        let suggested_workflow = if linked_pr_launch.is_some() {
            "review_fix_loop".to_string()
        } else {
            let github_item = self.store.get_github_item(&issue.github_item_id).ok();
            crate::threads::suggest_workflow(github_item.as_ref(), false)
        };

        let (board_issue, review_pr) = if let Some(pr_launch) = linked_pr_launch {
            (None, Some(pr_launch))
        } else {
            (
                Some(PendingBoardIssueLaunch {
                    github_item_id: Some(issue.github_item_id),
                    number: issue.number,
                    title: issue.title.clone(),
                    body: issue.body.unwrap_or_default(),
                    url: issue.url,
                }),
                None,
            )
        };

        let draft = LaunchThreadDraft {
            task_id: None,
            board_issue,
            review_pr,
            provider_kind: crate::store::ProviderKind::Claude,
            runtime_profile: self
                .config
                .runtime
                .default_profile
                .clone()
                .unwrap_or_default(),
            workflow_name: suggested_workflow,
            title: format!("#{} {}", issue.number, issue.title),
            extra_context: String::new(),
        };
        self.launch_thread_draft = Some(draft);
        self.launch_thread_field_index = 0;
        self.load_launch_thread_input();
        self.input_mode = InputMode::LaunchThread;
        Ok(())
    }

    fn open_my_task_launch_modal(&mut self) -> Result<()> {
        let Some(item) = self.selected_my_task_item().cloned() else {
            self.show_toast("Select an assigned GitHub item first", ToastStyle::Info);
            return Ok(());
        };

        // If a thread has a live session tab, switch to it directly.
        // Otherwise fall through to the launch modal (even if a done thread exists).
        if let Some(thread) = item.linked_thread.clone().or_else(|| {
            self.threads
                .iter()
                .find(|thread| {
                    thread.github_item_id.as_deref() == Some(item.github_item.id.as_str())
                })
                .cloned()
        }) && let Some(ref sid) = thread.session_id
            && self.goto_session_tab(sid)
        {
            self.show_toast("Switched to live session", ToastStyle::Info);
            return Ok(());
        }

        let issue_body = item
            .issue_cache
            .as_ref()
            .and_then(|cache| cache.body.clone().or(cache.body_text.clone()))
            .or_else(|| item.github_item.body_text.clone())
            .unwrap_or_default();
        let pr_body = item
            .pr_cache
            .as_ref()
            .and_then(|cache| cache.body.clone().or(cache.body_text.clone()))
            .or_else(|| item.github_item.body_text.clone())
            .unwrap_or_default();

        // If this is an issue with a linked PR, redirect launch to the PR's branch.
        let linked_pr_launch = if item.github_item.kind == crate::store::GitHubItemKind::Issue {
            // Try pre-fetched linked_pr first, then store lookup
            let linked_pr = item.linked_pr.clone().or_else(|| {
                self.store
                    .find_linked_pr_for_issue(&item.github_item.id)
                    .ok()
                    .flatten()
            });
            linked_pr.map(|linked_pr| {
                let pr_cache = self.store.get_github_pr_cache(&linked_pr.id).ok();
                let linked_body = pr_cache
                    .as_ref()
                    .and_then(|cache| cache.body.clone().or(cache.body_text.clone()))
                    .or_else(|| linked_pr.body_text.clone())
                    .unwrap_or_default();
                PendingReviewPrLaunch {
                    github_item_id: linked_pr.id,
                    number: linked_pr.number,
                    title: linked_pr.title,
                    body: linked_body,
                    url: linked_pr.url,
                    base_ref: pr_cache.as_ref().and_then(|cache| cache.base_ref.clone()),
                    head_ref: pr_cache.as_ref().and_then(|cache| cache.head_ref.clone()),
                    mode: PendingReviewLaunchMode::ContinueWork,
                }
            })
        } else {
            None
        };

        // Build rich context from the issue body + any PR review comments
        let mut extra_context = String::new();
        if linked_pr_launch.is_some() {
            // Include the original issue context so the agent has the full picture
            let _ = std::fmt::Write::write_fmt(
                &mut extra_context,
                format_args!(
                    "## GitHub Issue #{}\n{}\n\n",
                    item.github_item.number, issue_body
                ),
            );
            // Include PR review comments if available
            let pr_item_id = linked_pr_launch
                .as_ref()
                .map(|pr| pr.github_item_id.as_str());
            if let Some(pr_id) = pr_item_id
                && !pr_id.is_empty()
            {
                if let Ok(reviews) = self.store.list_github_reviews_for_item(pr_id) {
                    for review in reviews.iter().take(10) {
                        let author = review.author_login.as_deref().unwrap_or("unknown");
                        let state = &review.state;
                        if let Some(body) = review.body_text.as_deref().or(review.body.as_deref())
                            && !body.is_empty()
                        {
                            let _ = std::fmt::Write::write_fmt(
                                &mut extra_context,
                                format_args!("Review by @{author} ({state}):\n{body}\n\n"),
                            );
                        }
                    }
                }
                if let Ok(comments) = self.store.list_github_review_comments_for_item(pr_id) {
                    for comment in comments.iter().take(20) {
                        let author = comment.author_login.as_deref().unwrap_or("unknown");
                        let path = comment.path.as_deref().unwrap_or("");
                        if let Some(body) = comment.body_text.as_deref().or(comment.body.as_deref())
                            && !body.is_empty()
                        {
                            let _ = std::fmt::Write::write_fmt(
                                &mut extra_context,
                                format_args!("Inline comment by @{author} on {path}:\n{body}\n\n"),
                            );
                        }
                    }
                }
            }
        }

        let (board_issue, review_pr) = if let Some(pr_launch) = linked_pr_launch {
            // Issue has a linked PR — launch on the PR's branch instead.
            (None, Some(pr_launch))
        } else {
            (
                (item.github_item.kind == crate::store::GitHubItemKind::Issue).then(|| {
                    PendingBoardIssueLaunch {
                        github_item_id: Some(item.github_item.id.clone()),
                        number: item.github_item.number,
                        title: item.github_item.title.clone(),
                        body: issue_body,
                        url: item.github_item.url.clone(),
                    }
                }),
                (item.github_item.kind == crate::store::GitHubItemKind::PullRequest).then(|| {
                    PendingReviewPrLaunch {
                        github_item_id: item.github_item.id.clone(),
                        number: item.github_item.number,
                        title: item.github_item.title.clone(),
                        body: pr_body,
                        url: item.github_item.url.clone(),
                        base_ref: item
                            .pr_cache
                            .as_ref()
                            .and_then(|cache| cache.base_ref.clone()),
                        head_ref: item
                            .pr_cache
                            .as_ref()
                            .and_then(|cache| cache.head_ref.clone()),
                        mode: PendingReviewLaunchMode::ContinueWork,
                    }
                }),
            )
        };

        let suggested_workflow = crate::threads::suggest_workflow(Some(&item.github_item), false);
        let draft = LaunchThreadDraft {
            task_id: None,
            board_issue,
            review_pr,
            provider_kind: crate::store::ProviderKind::Claude,
            runtime_profile: self
                .config
                .runtime
                .default_profile
                .clone()
                .unwrap_or_default(),
            workflow_name: suggested_workflow,
            title: item.title_line(),
            extra_context,
        };
        self.launch_thread_draft = Some(draft);
        self.launch_thread_field_index = 0;
        self.load_launch_thread_input();
        self.input_mode = InputMode::LaunchThread;
        Ok(())
    }

    fn open_adhoc_launch_modal(&mut self) -> Result<()> {
        let Some(_project) = self.selected_project() else {
            self.show_toast("Select a project first", ToastStyle::Info);
            return Ok(());
        };
        let draft = LaunchThreadDraft {
            task_id: None,
            board_issue: None,
            review_pr: None,
            provider_kind: crate::store::ProviderKind::Claude,
            runtime_profile: self
                .config
                .runtime
                .default_profile
                .clone()
                .unwrap_or_default(),
            workflow_name: "plan_first_tdd".to_string(),
            title: String::new(),
            extra_context: String::new(),
        };
        self.launch_thread_draft = Some(draft);
        // Start on the Title field so user can name their feature
        self.launch_thread_field_index = 3;
        self.load_launch_thread_input();
        self.input_mode = InputMode::LaunchThread;
        Ok(())
    }

    fn open_review_launch_modal(&mut self) -> Result<()> {
        let Some(review_item) = self.selected_review_item().cloned() else {
            self.show_toast("Select a pull request first", ToastStyle::Info);
            return Ok(());
        };

        let mode = match self.review_queue_tab {
            ReviewQueueTab::Authored => PendingReviewLaunchMode::ContinueWork,
            ReviewQueueTab::NeedsReview => PendingReviewLaunchMode::Review,
        };
        let title = match mode {
            PendingReviewLaunchMode::ContinueWork => {
                format!(
                    "PR #{} {}",
                    review_item.github_item.number, review_item.github_item.title
                )
            }
            PendingReviewLaunchMode::Review => format!(
                "Review PR #{} {}",
                review_item.github_item.number, review_item.github_item.title
            ),
        };
        let body = review_item
            .pr_cache
            .body_text
            .clone()
            .or_else(|| review_item.pr_cache.body.clone())
            .or_else(|| review_item.github_item.body_text.clone())
            .unwrap_or_default();
        let base_ref = review_item.pr_cache.base_ref;
        let head_ref = review_item.pr_cache.head_ref;

        // Build extra_context from pre-fetched review comments
        let extra_context = self.format_review_comments_context(&mode);
        let suggested_workflow =
            crate::threads::suggest_workflow(Some(&review_item.github_item), false);

        let draft = LaunchThreadDraft {
            task_id: None,
            board_issue: None,
            review_pr: Some(PendingReviewPrLaunch {
                github_item_id: review_item.github_item.id.clone(),
                number: review_item.github_item.number,
                title: review_item.github_item.title.clone(),
                body,
                url: review_item.github_item.url,
                base_ref,
                head_ref,
                mode,
            }),
            provider_kind: crate::store::ProviderKind::Claude,
            runtime_profile: self
                .config
                .runtime
                .default_profile
                .clone()
                .unwrap_or_default(),
            workflow_name: suggested_workflow,
            title,
            extra_context,
        };
        self.launch_thread_draft = Some(draft);
        self.launch_thread_field_index = 0;
        self.load_launch_thread_input();
        self.input_mode = InputMode::LaunchThread;
        Ok(())
    }

    /// Format pre-fetched review comments, reviews, and inline review comments
    /// into a context string suitable for the `extra_context` field on a
    /// `LaunchThreadDraft`.
    fn format_review_comments_context(&self, mode: &PendingReviewLaunchMode) -> String {
        if self.review_selected_reviews.is_empty()
            && self.review_selected_comments.is_empty()
            && self.review_selected_review_comments.is_empty()
        {
            return String::new();
        }

        let mut ctx = String::new();
        let header = match mode {
            PendingReviewLaunchMode::ContinueWork => "## PR Review Feedback\n\n",
            PendingReviewLaunchMode::Review => "## Existing Review Activity\n\n",
        };
        ctx.push_str(header);

        // Top-level reviews (APPROVED, CHANGES_REQUESTED, COMMENTED, etc.)
        for review in &self.review_selected_reviews {
            let author = review.author_login.as_deref().unwrap_or("unknown");
            let state = &review.state;
            let _ = writeln!(ctx, "### @{author} ({state})");
            if let Some(body) = review.body_text.as_deref().or(review.body.as_deref())
                && !body.is_empty()
            {
                ctx.push_str(body);
                ctx.push('\n');
            }
            ctx.push('\n');
        }

        // Top-level comments
        for comment in &self.review_selected_comments {
            let author = comment.author_login.as_deref().unwrap_or("unknown");
            let _ = writeln!(ctx, "**@{author}**");
            if let Some(body) = comment.body_text.as_deref().or(comment.body.as_deref()) {
                ctx.push_str(body);
                ctx.push('\n');
            }
            ctx.push('\n');
        }

        // Inline review comments (file-specific)
        if !self.review_selected_review_comments.is_empty() {
            ctx.push_str("### Inline Comments\n\n");
            for rc in &self.review_selected_review_comments {
                let author = rc.author_login.as_deref().unwrap_or("unknown");
                let path = rc.path.as_deref().unwrap_or("unknown file");
                let line_info = rc
                    .line
                    .map_or_else(String::new, |line| format!(" line {line}"));
                let _ = writeln!(ctx, "**@{author}** on `{path}`{line_info}");
                if let Some(hunk) = &rc.diff_hunk {
                    let _ = writeln!(
                        ctx,
                        "> {}",
                        hunk.lines().take(3).collect::<Vec<_>>().join("\n> ")
                    );
                }
                if let Some(body) = rc.body_text.as_deref().or(rc.body.as_deref()) {
                    ctx.push_str(body);
                    ctx.push('\n');
                }
                ctx.push('\n');
            }
        }

        ctx
    }

    fn focus_thread_workspace(&mut self, thread_id: &str) {
        // Find the session tab for this thread and switch to it.
        let session_id = self
            .threads
            .iter()
            .find(|t| t.id == thread_id)
            .and_then(|t| t.session_id.clone())
            .or_else(|| {
                self.store
                    .get_thread(thread_id)
                    .ok()
                    .and_then(|t| t.session_id)
            });

        if let Some(ref sid) = session_id
            && let Some(tab_idx) = self.tabs.iter().position(
                |tab| matches!(tab, super::Tab::Session { session_id, .. } if session_id == sid),
            )
        {
            self.active_tab = tab_idx;
        }
        // If no session tab exists, stay where we are — don't fall back to the
        // inline Threads view which can't send messages or interact with Claude.
    }

    fn start_thread_compose_for(&mut self, thread_id: &str, focus_workspace: bool) -> Result<()> {
        let Some(index) = self
            .threads
            .iter()
            .position(|candidate| candidate.id == thread_id)
        else {
            self.show_toast("Select a thread first", ToastStyle::Info);
            return Ok(());
        };

        self.thread_index = index;
        // When focus_workspace is true, try to switch to the thread's session
        // tab instead of the inline Threads view (which can't send messages).
        if focus_workspace {
            let thread = self.threads.get(index).cloned();
            if let Some(ref t) = thread
                && let Some(ref sid) = t.session_id
                && let Some(tab_idx) = self.tabs.iter().position(
                    |tab| matches!(tab, super::Tab::Session { session_id, .. } if session_id == sid),
                )
            {
                self.active_tab = tab_idx;
            }
        }
        self.focus = Focus::Tasks;
        self.thread_compose_thread_id = Some(thread_id.to_string());
        self.input_buffer = self.thread_compose_buffer.clone();
        self.input_cursor = self.thread_compose_cursor.min(self.input_buffer.len());
        self.input_mode = InputMode::ThreadCompose;
        Ok(())
    }

    pub(super) fn start_thread_compose(&mut self) -> Result<()> {
        let Some(thread) = self.selected_thread().cloned() else {
            self.show_toast("Select a thread first", ToastStyle::Info);
            return Ok(());
        };

        self.start_thread_compose_for(&thread.id, true)
    }

    fn live_session_for_thread(
        &self,
        thread: &crate::store::Thread,
    ) -> Option<crate::store::Session> {
        thread.session_id.as_deref().and_then(|session_id| {
            self.sessions
                .iter()
                .find(|session| session.id == session_id && session.closed_at.is_none())
                .cloned()
        })
    }

    fn spawn_thread_relaunch(&mut self, thread_id: String) {
        self.session_op_in_progress = true;
        self.show_toast("Continuing thread...", ToastStyle::Info);

        let tx = self.session_op_tx.clone();
        let cfg = self.config.clone();
        std::thread::spawn(move || {
            let result = match crate::store::Store::open() {
                Ok(store) => match crate::threads::relaunch_thread(&store, &cfg, &thread_id) {
                    Ok(result) => SessionOpResult::ThreadLaunched {
                        result: Box::new(result),
                    },
                    Err(error) => SessionOpResult::Error {
                        message: format!("Thread relaunch failed: {error}"),
                    },
                },
                Err(error) => SessionOpResult::Error {
                    message: format!("Thread relaunch failed (DB): {error}"),
                },
            };
            let _ = tx.send(result);
        });
    }

    fn open_thread_live_session(&mut self, thread: &crate::store::Thread) -> Result<()> {
        self.focus_thread_workspace(&thread.id);
        if let Some(session) = self.live_session_for_thread(thread) {
            if !self.goto_session_tab(&session.id) {
                self.restore_session_tab(&session)?;
            } else if let Some(Tab::Session { view_mode, .. }) = self.tabs.iter_mut().find(
                |tab| matches!(tab, Tab::Session { session_id: sid, .. } if sid == &session.id),
            ) {
                *view_mode = SessionTabView::Conversation;
            }
            self.show_toast("Opened live conversation session", ToastStyle::Success);
        } else {
            self.show_toast(
                "Thread is not live — press l to continue it first",
                ToastStyle::Info,
            );
        }
        Ok(())
    }

    fn continue_thread(&mut self, thread: crate::store::Thread) -> Result<()> {
        self.focus_thread_workspace(&thread.id);
        if self.live_session_for_thread(&thread).is_some() {
            self.show_toast(
                format!(
                    "Thread is live via {} — just start typing here, or press o for the terminal",
                    thread.provider_kind
                ),
                ToastStyle::Success,
            );
            return Ok(());
        }

        if self.session_op_in_progress {
            self.show_toast("Session operation in progress...", ToastStyle::Info);
            return Ok(());
        }

        self.spawn_thread_relaunch(thread.id);
        Ok(())
    }

    fn run_active_thread_runtime_action(&mut self, action: ThreadRuntimeAction) -> Result<()> {
        let Some(thread) = self.active_session_thread() else {
            self.show_toast("No live thread runtime is available", ToastStyle::Info);
            return Ok(());
        };
        let Some(worktree_path) = thread.worktree_path.clone() else {
            self.show_toast("Thread has no worktree yet", ToastStyle::Info);
            return Ok(());
        };

        let project = self.store.get_project(&thread.project_id)?;
        let worktree_root = std::path::Path::new(&worktree_path);
        let repo_root = std::path::Path::new(&project.repo_path);
        let runtime_cfg = &self.config.runtime;

        let result = match action {
            ThreadRuntimeAction::Up => crate::runtime::up_profile(
                &self.store,
                &thread.id,
                worktree_root,
                repo_root,
                runtime_cfg,
                thread.runtime_profile.as_deref(),
            )
            .map(|states| (states.len(), "Started runtime services".to_string())),
            ThreadRuntimeAction::Down => crate::runtime::down_profile(
                &self.store,
                &thread.id,
                worktree_root,
                repo_root,
                runtime_cfg,
            )
            .map(|states| (states.len(), "Stopped runtime services".to_string())),
            ThreadRuntimeAction::Restart => crate::runtime::restart_profile(
                &self.store,
                &thread.id,
                worktree_root,
                repo_root,
                runtime_cfg,
            )
            .map(|states| (states.len(), "Restarted runtime services".to_string())),
            ThreadRuntimeAction::Health => crate::runtime::health_check_profile(
                &self.store,
                &thread.id,
                worktree_root,
                repo_root,
                runtime_cfg,
            )
            .map(|states| (states.len(), "Runtime health checks completed".to_string())),
        };

        match result {
            Ok((count, message)) => {
                self.refresh_data()?;
                self.show_toast(format!("{message} ({count})"), ToastStyle::Success);
            }
            Err(error) => {
                self.show_toast(format!("Runtime action failed: {error}"), ToastStyle::Error);
            }
        }

        Ok(())
    }

    /// Check if the active session has a permission prompt waiting.
    fn active_session_is_paused(&self) -> bool {
        self.active_session_id()
            .is_some_and(|sid| self.paused_sessions.contains(sid))
    }

    /// Handle keys when permission dialog is showing: Enter = allow, Esc = deny.
    fn handle_permission_dialog_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Enter | KeyCode::Char('y') => {
                // Send Enter to the PTY to accept the default (Yes) option
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
                    let pane_id = terminals.claude_pane_id;
                    if let Some(term) = terminals.terminal_mut(pane_id) {
                        let _ = term.send_bytes(b"\r");
                    }
                }
                self.show_toast("Permission granted", ToastStyle::Success);
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                // Send Escape then select No
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
                    let pane_id = terminals.claude_pane_id;
                    if let Some(term) = terminals.terminal_mut(pane_id) {
                        // Navigate to "No" option and confirm
                        let _ = term.send_bytes(b"\x1b[B\x1b[B\r"); // Down, Down, Enter
                    }
                }
                self.show_toast("Permission denied", ToastStyle::Info);
            }
            _ => {}
        }
        Ok(())
    }

    /// Prompt to close the active session permanently (with confirmation).
    fn prompt_close_session(&mut self) {
        let Some(session_id) = self.active_session_id().map(String::from) else {
            self.show_toast("No active session", ToastStyle::Info);
            return;
        };
        let label = self
            .tabs
            .get(self.active_tab)
            .map(|tab| match tab {
                Tab::Session { label, .. } => label.clone(),
                Tab::Dashboard => "Dashboard".to_string(),
            })
            .unwrap_or_default();
        self.confirm_target = label;
        self.confirm_entity_id = session_id;
        self.confirm_delete_kind = DeleteTarget::Session;
        self.input_mode = InputMode::ConfirmDelete;
    }


    /// Drain a queued compose message when Claude becomes idle.
    /// Called on every tick after `detect_paused_sessions`.
    pub(super) fn drain_queued_compose_message(&mut self) {
        let Some((ref thread_id, _)) = self.queued_compose_message else {
            return;
        };
        // Find the session for this thread
        let session_id = self
            .threads
            .iter()
            .find(|t| t.id == *thread_id)
            .and_then(|t| t.session_id.clone());
        let Some(ref sid) = session_id else {
            return;
        };
        // Only drain when Claude is idle (DB status is Idle or in pty_idle_sessions)
        let is_idle = self
            .sessions
            .iter()
            .any(|s| s.id == *sid && s.claude_status == crate::store::ClaudeStatus::Idle)
            || self.pty_idle_sessions.contains(sid.as_str());
        if !is_idle {
            return;
        }
        let (thread_id, content) = self.queued_compose_message.take().expect("checked above");
        if let Ok(thread) = self.store.get_thread(&thread_id) {
            let sent = self.send_to_active_session_claude(&content)
                || self.send_prompt_to_live_thread_session(&thread, &content);
            if sent {
                self.show_toast("Queued message sent to agent", ToastStyle::Success);
                if let Some(ref mut cache) = self.conversation_cache {
                    cache
                        .entries
                        .push(crate::conversation::ConversationEntry::UserMessage {
                            timestamp: chrono::Utc::now()
                                .format("%Y-%m-%dT%H:%M:%S")
                                .to_string(),
                            text: content,
                        });
                }
            } else {
                // Claude dead — try restart
                let _ = self.restart_claude_with_message(&thread, &content);
                self.show_toast("Restarting Claude with queued message...", ToastStyle::Info);
            }
        }
    }

    /// Check the Claude pane's screen state in the active session tab.
    /// Returns: `Some(true)` = Claude is at idle prompt (❯), `Some(false)` = Claude
    /// is busy or showing other content, `None` = no session tab / pane not found.
    fn is_claude_idle_in_active_session(&self) -> Option<bool> {
        let Tab::Session { terminals, .. } = self.tabs.get(self.active_tab)? else {
            return None;
        };
        terminals.with_claude_live_screen(|screen| {
            super::screen_shows_idle_prompt(screen)
        })
    }

    /// Check if Claude has exited in the active session.
    /// Uses `pty_idle_sessions` as a signal — if the session has been idle
    /// (no Claude indicator for 15+ seconds), Claude likely exited.

    /// Send a prompt directly to the active session tab's Claude pane.
    /// This is the most direct path — no thread/session lookup needed.
    fn send_to_active_session_claude(&mut self, prompt: &str) -> bool {
        let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) else {
            return false;
        };
        let pane_id = terminals.claude_pane_id;
        let Some(term) = terminals.terminal_mut(pane_id) else {
            return false;
        };
        if term.exited() {
            return false;
        }
        term.reset_scrollback();
        let payload = format!("\x1b[200~{prompt}\x1b[201~\n");
        term.send_bytes(payload.as_bytes()).is_ok()
    }

    /// Send Ctrl+C (SIGINT) to the Claude pane to interrupt a stuck operation.
    fn interrupt_active_session_claude(&mut self) -> bool {
        let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) else {
            return false;
        };
        let pane_id = terminals.claude_pane_id;
        let Some(term) = terminals.terminal_mut(pane_id) else {
            return false;
        };
        if term.exited() {
            return false;
        }
        // Send ETX (Ctrl+C) which triggers SIGINT in the PTY
        term.send_bytes(b"\x03").is_ok()
    }

    pub(super) fn send_prompt_to_live_thread_session(
        &mut self,
        thread: &crate::store::Thread,
        prompt: &str,
    ) -> bool {
        let Some(session_id) = thread.session_id.as_deref() else {
            return false;
        };
        let Some(Tab::Session { terminals, .. }) = self
            .tabs
            .iter_mut()
            .find(|tab| matches!(tab, Tab::Session { session_id: sid, .. } if sid == session_id))
        else {
            return false;
        };

        let pane_id = terminals.claude_pane_id;
        let Some(term) = terminals.terminal_mut(pane_id) else {
            return false;
        };

        term.reset_scrollback();
        if term.exited() {
            return false;
        }
        let payload = format!("\x1b[200~{prompt}\x1b[201~\n");
        match term.send_bytes(payload.as_bytes()) {
            Ok(()) => true,
            Err(_) => false,
        }
    }

    /// Restart Claude Code in the existing shell PTY when Claude has exited.
    /// Sends `claude --resume <session_id> -p "message"` to the shell, which
    /// resumes the previous conversation with the user's message as prompt.
    fn restart_claude_with_message(
        &mut self,
        thread: &crate::store::Thread,
        message: &str,
    ) -> bool {
        let Some(session_id) = thread.session_id.as_deref() else {
            return false;
        };
        let session = match self.store.get_session(session_id) {
            Ok(s) => s,
            Err(_) => return false,
        };

        // Build resume command: claude --resume <csid> -p "message"
        let mut cmd = crate::threads::build_resume_agent_command(
            &self.config,
            thread.provider_kind,
            thread.provider_profile.as_deref(),
            &session,
        );
        cmd.push("-p".to_string());
        cmd.push(message.to_string());

        // Shell-escape each arg and join into a single command line
        let shell_cmd: String = cmd
            .iter()
            .map(|arg| crate::session::shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");

        // Send to the claude pane's PTY as a raw shell command (not bracketed paste)
        let Some(Tab::Session { terminals, .. }) = self
            .tabs
            .iter_mut()
            .find(|tab| matches!(tab, Tab::Session { session_id: sid, .. } if sid == session_id))
        else {
            return false;
        };
        let pane_id = terminals.claude_pane_id;
        let Some(term) = terminals.terminal_mut(pane_id) else {
            return false;
        };
        if term.exited() {
            return false;
        }

        let payload = format!("{shell_cmd}\r");
        if term.send_bytes(payload.as_bytes()).is_err() {
            return false;
        }

        // Update session status to Working
        let _ = self.store.update_session_status(
            session_id,
            crate::store::ClaudeStatus::Working,
            "Resuming Claude session",
        );
        // Create a new thread run for this resume
        let _ = self.store.create_thread_run(
            &thread.id,
            thread.provider_kind,
            thread.provider_profile.as_deref(),
            crate::store::ThreadRunStatus::Running,
            Some(message),
        );
        // Reset the conversation cache's stored file offset so it re-reads
        // from the beginning when the JSONL grows. Don't set cache to None —
        // the caller adds a user message entry to the cache right after this.
        if let Some(ref mut cache) = self.conversation_cache {
            cache.file_offset = 0;
            cache.entries.clear();
        }
        // Clear the timer-based exit detection since we just restarted Claude
        self.working_no_indicator_since.remove(session_id);
        true
    }

    fn approve_active_workflow_gate(&mut self) -> Result<()> {
        let Some(thread) = self.active_session_thread() else {
            return Ok(());
        };
        let Some(run_id) = thread.workflow_run_id.as_deref() else {
            self.show_toast("No workflow on this thread", ToastStyle::Info);
            return Ok(());
        };
        let Ok(workflow_run) = self.store.get_workflow_run(run_id) else {
            return Ok(());
        };
        if workflow_run.status != crate::store::WorkflowRunStatus::WaitingApproval {
            self.show_toast("No stage waiting for approval", ToastStyle::Info);
            return Ok(());
        }
        let Some(stage_name) = workflow_run.current_stage.as_deref() else {
            return Ok(());
        };
        match crate::workflows::approve_workflow_stage(&self.store, run_id, stage_name) {
            Ok(_bundle) => {
                self.show_toast(format!("Stage approved: {stage_name}"), ToastStyle::Success);
                // Next tick picks up the newly Running stage for prompt injection
            }
            Err(err) => {
                self.show_toast(format!("Approve failed: {err:#}"), ToastStyle::Error);
            }
        }
        Ok(())
    }

    pub(super) fn submit_thread_compose_message(&mut self) -> Result<()> {
        let Some(thread_id) = self
            .thread_compose_thread_id
            .clone()
            .or_else(|| self.selected_thread().map(|thread| thread.id.clone()))
        else {
            self.show_toast("Select a thread first", ToastStyle::Info);
            return Ok(());
        };

        let content = self.thread_compose_buffer.trim().to_string();
        eprintln!("[compose] submit called, content={:?}, thread_id={thread_id}", content.chars().take(40).collect::<String>());
        if content.is_empty() {
            eprintln!("[compose] empty content, aborting");
            self.show_toast("Compose a message first", ToastStyle::Info);
            return Ok(());
        }

        // Record in compose history so user can cycle back with Up arrow
        self.compose_history.push(content.clone());
        self.compose_history_index = None;

        let thread = self.store.get_thread(&thread_id)?;
        let latest_run_id = self
            .store
            .list_thread_runs(&thread.id)?
            .into_iter()
            .last()
            .map(|run| run.id);
        self.store.create_thread_message(
            &thread.id,
            latest_run_id.as_deref(),
            "user",
            &content,
            &[],
        )?;
        self.store
            .update_thread_status(&thread.id, crate::store::ThreadStatus::Running)?;

        // Clear quick-reply choices since the user is responding
        self.quick_reply_choices.clear();

        // Determine Claude's state to pick the right send strategy:
        // 1. Dead (pty_idle + no ❯ prompt) → restart with --resume
        // 2. Working (DB says Working, not idle-detected) → queue for later
        // 3. Idle or unknown → send directly
        let session_id = thread.session_id.as_deref();
        let claude_dead = session_id.is_some_and(|sid| {
            self.pty_idle_sessions.contains(sid)
                && self.is_claude_idle_in_active_session() != Some(true)
        });
        let claude_working = session_id.is_some_and(|sid| {
            self.sessions
                .iter()
                .any(|s| s.id == sid && s.claude_status == crate::store::ClaudeStatus::Working)
                && !self.pty_idle_sessions.contains(sid)
                && self.is_claude_idle_in_active_session() != Some(true)
        });

        eprintln!(
            "[compose] state: session_id={:?}, claude_dead={claude_dead}, claude_working={claude_working}, idle={:?}",
            session_id,
            self.is_claude_idle_in_active_session()
        );

        if claude_dead {
            eprintln!("[compose] path: claude_dead → restart");
            let restarted = self.restart_claude_with_message(&thread, &content);
            if restarted {
                self.show_toast("Restarting Claude with your message...", ToastStyle::Info);
                if let Some(ref mut cache) = self.conversation_cache {
                    cache
                        .entries
                        .push(crate::conversation::ConversationEntry::UserMessage {
                            timestamp: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
                            text: content,
                        });
                }
            } else {
                eprintln!("[compose] restart failed — preserving message in buffer");
                // Keep the message in the compose buffer so user doesn't lose it
                self.thread_compose_buffer = content;
                self.thread_compose_cursor = self.thread_compose_buffer.len();
                self.input_buffer.clone_from(&self.thread_compose_buffer);
                self.input_cursor = self.input_buffer.len();
                self.show_toast(
                    "Claude has exited — press l to relaunch, your message is preserved",
                    ToastStyle::Error,
                );
                self.input_mode = InputMode::ThreadCompose;
                self.thread_compose_thread_id = Some(thread.id.clone());
                return Ok(());
            }
        } else if claude_working {
            eprintln!("[compose] path: claude_working → queue");
            self.queued_compose_message = Some((thread.id.clone(), content));
            self.show_toast(
                "Message queued — will send when Claude finishes",
                ToastStyle::Info,
            );
        } else {
            eprintln!("[compose] path: idle/unknown → send directly");
            let sent_active = self.send_to_active_session_claude(&content);
            let sent = sent_active || self.send_prompt_to_live_thread_session(&thread, &content);
            eprintln!("[compose] send result: sent_active={sent_active}, sent={sent}");
            if sent {
                self.show_toast("Sent to agent", ToastStyle::Success);
                if let Some(ref mut cache) = self.conversation_cache {
                    cache
                        .entries
                        .push(crate::conversation::ConversationEntry::UserMessage {
                            timestamp: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
                            text: content,
                        });
                }
            } else {
                // PTY send failed — try restart as last resort
                let restarted = self.restart_claude_with_message(&thread, &content);
                if restarted {
                    self.show_toast("Resuming Claude session...", ToastStyle::Info);
                } else {
                    self.show_toast(
                        "Claude has exited — relaunch session to continue",
                        ToastStyle::Error,
                    );
                }
            }
        }

        self.thread_compose_buffer.clear();
        self.thread_compose_cursor = 0;
        self.input_buffer.clear();
        self.input_cursor = 0;
        // Stay in compose mode so the user can keep chatting without pressing
        // `i` again after every message — like a real chat interface.
        // Press Esc to exit compose mode.
        self.input_mode = InputMode::ThreadCompose;
        self.thread_compose_thread_id = Some(thread.id.clone());
        self.refresh_data()?;
        // Stay on the current session tab instead of switching to Threads view
        if self
            .active_session_id()
            .is_none_or(|sid| thread.session_id.as_deref() != Some(sid))
        {
            self.focus_thread_workspace(&thread.id);
        }
        Ok(())
    }

    fn launch_thread_from_draft(&mut self) -> Result<()> {
        self.persist_launch_thread_input();
        let Some(draft) = self.launch_thread_draft.clone() else {
            self.show_toast("No launch draft is active", ToastStyle::Error);
            return Ok(());
        };
        let Some(project_id) = self.selected_project().map(|project| project.id.clone()) else {
            self.show_toast("Select a project first", ToastStyle::Info);
            return Ok(());
        };

        anyhow::ensure!(
            !draft.title.trim().is_empty(),
            "Thread title cannot be empty"
        );

        self.input_mode = InputMode::Normal;
        self.launch_thread_draft = None;
        self.session_op_in_progress = true;
        self.show_toast("Launching thread...", ToastStyle::Info);

        let tx = self.session_op_tx.clone();
        let cfg = self.config.clone();
        std::thread::spawn(move || {
            let result = match crate::store::Store::open() {
                Ok(store) => {
                    let outcome = if let Some(task_id) = draft.task_id.as_ref() {
                        crate::threads::launch_thread(
                            &store,
                            &cfg,
                            &crate::threads::LaunchThreadArgs {
                                task_id: task_id.clone(),
                                provider_kind: Some(draft.provider_kind),
                                provider_profile: Some("default".to_string()),
                                runtime_profile: (!draft.runtime_profile.trim().is_empty())
                                    .then(|| draft.runtime_profile.clone()),
                                workflow_name: (!draft.workflow_name.trim().is_empty())
                                    .then(|| draft.workflow_name.clone()),
                                thread_title: Some(draft.title.clone()),
                                initial_prompt: build_launch_prompt_for_task(
                                    &store,
                                    task_id,
                                    &draft.extra_context,
                                )
                                .ok(),
                            },
                        )
                    } else if let Some(review_pr) = draft.review_pr.as_ref() {
                        crate::threads::launch_github_item_thread(
                            &store,
                            &cfg,
                            &crate::threads::LaunchGitHubItemThreadArgs {
                                github_item_id: review_pr.github_item_id.clone(),
                                provider_kind: Some(draft.provider_kind),
                                provider_profile: Some("default".to_string()),
                                runtime_profile: (!draft.runtime_profile.trim().is_empty())
                                    .then(|| draft.runtime_profile.clone()),
                                workflow_name: (!draft.workflow_name.trim().is_empty())
                                    .then(|| draft.workflow_name.clone()),
                                thread_title: Some(draft.title.clone()),
                                initial_prompt: Some(build_launch_prompt_for_review_pr(
                                    review_pr,
                                    &draft.extra_context,
                                )),
                            },
                        )
                    } else if let Some(issue) = draft.board_issue.as_ref() {
                        if let Some(github_item_id) = issue.github_item_id.as_ref() {
                            crate::threads::launch_github_item_thread(
                                &store,
                                &cfg,
                                &crate::threads::LaunchGitHubItemThreadArgs {
                                    github_item_id: github_item_id.clone(),
                                    provider_kind: Some(draft.provider_kind),
                                    provider_profile: Some("default".to_string()),
                                    runtime_profile: (!draft.runtime_profile.trim().is_empty())
                                        .then(|| draft.runtime_profile.clone()),
                                    workflow_name: (!draft.workflow_name.trim().is_empty())
                                        .then(|| draft.workflow_name.clone()),
                                    thread_title: Some(draft.title.clone()),
                                    initial_prompt: Some(combine_prompt(
                                        &issue.body,
                                        &draft.extra_context,
                                    )),
                                },
                            )
                        } else {
                            let task_title = format!("#{} {}", issue.number, issue.title);
                            let base_description = if issue.body.trim().is_empty() {
                                issue.url.clone()
                            } else {
                                format!("{}\n\n{}", issue.body, issue.url)
                            };
                            match store.create_task(
                                &project_id,
                                &task_title,
                                &base_description,
                                crate::store::TaskMode::Supervised,
                                None,
                                None,
                                crate::store::PushMode::Pr,
                                false,
                            ) {
                                Ok(task) => crate::threads::launch_thread(
                                    &store,
                                    &cfg,
                                    &crate::threads::LaunchThreadArgs {
                                        task_id: task.id,
                                        provider_kind: Some(draft.provider_kind),
                                        provider_profile: Some("default".to_string()),
                                        runtime_profile: (!draft.runtime_profile.trim().is_empty())
                                            .then(|| draft.runtime_profile.clone()),
                                        workflow_name: (!draft.workflow_name.trim().is_empty())
                                            .then(|| draft.workflow_name.clone()),
                                        thread_title: Some(draft.title.clone()),
                                        initial_prompt: Some(combine_prompt(
                                            &base_description,
                                            &draft.extra_context,
                                        )),
                                    },
                                ),
                                Err(error) => Err(error),
                            }
                        }
                    } else {
                        crate::threads::launch_ad_hoc_thread(
                            &store,
                            &cfg,
                            &crate::threads::LaunchAdHocThreadArgs {
                                project_id,
                                title: draft.title.clone(),
                                initial_prompt: (!draft.extra_context.trim().is_empty())
                                    .then(|| draft.extra_context.clone()),
                                provider_kind: Some(draft.provider_kind),
                                provider_profile: Some("default".to_string()),
                                runtime_profile: (!draft.runtime_profile.trim().is_empty())
                                    .then(|| draft.runtime_profile.clone()),
                                workflow_name: (!draft.workflow_name.trim().is_empty())
                                    .then(|| draft.workflow_name.clone()),
                            },
                        )
                    };

                    match outcome {
                        Ok(result) => SessionOpResult::ThreadLaunched {
                            result: Box::new(result),
                        },
                        Err(error) => SessionOpResult::Error {
                            message: format!("Thread launch failed: {error}"),
                        },
                    }
                }
                Err(error) => {
                    SessionOpResult::Error {
                        message: format!("Thread launch failed (DB): {error}"),
                    }
                }
            };
            let _ = tx.send(result);
        });

        Ok(())
    }

    pub(super) fn handle_launch_thread_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        if self.launch_thread_draft.is_none() {
            self.input_mode = InputMode::Normal;
            return Ok(());
        }

        match code {
            KeyCode::Esc => {
                self.launch_thread_draft = None;
                self.input_mode = InputMode::Normal;
            }
            KeyCode::BackTab => {
                self.persist_launch_thread_input();
                self.launch_thread_field_index = if self.launch_thread_field_index == 0 {
                    LaunchThreadField::ALL.len() - 1
                } else {
                    self.launch_thread_field_index - 1
                };
                self.load_launch_thread_input();
            }
            KeyCode::Tab => {
                self.persist_launch_thread_input();
                self.launch_thread_field_index =
                    (self.launch_thread_field_index + 1) % LaunchThreadField::ALL.len();
                self.load_launch_thread_input();
            }
            KeyCode::Left | KeyCode::Char('h')
                if self.selected_launch_thread_field() == LaunchThreadField::Provider =>
            {
                self.cycle_launch_thread_provider(false);
            }
            KeyCode::Right | KeyCode::Char('l')
                if self.selected_launch_thread_field() == LaunchThreadField::Provider =>
            {
                self.cycle_launch_thread_provider(true);
            }
            KeyCode::Left | KeyCode::Char('h')
                if self.selected_launch_thread_field() == LaunchThreadField::Workflow =>
            {
                self.cycle_launch_thread_workflow(false);
            }
            KeyCode::Right | KeyCode::Char('l')
                if self.selected_launch_thread_field() == LaunchThreadField::Workflow =>
            {
                self.cycle_launch_thread_workflow(true);
            }
            KeyCode::Enter => {
                self.launch_thread_from_draft()?;
            }
            _ if self.selected_launch_thread_field().is_text() => {
                let _ = apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                );
                self.persist_launch_thread_input();
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn handle_thread_compose_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        // If slash suggestions are showing, handle navigation
        if !self.slash_suggestions.is_empty() {
            match code {
                KeyCode::Tab | KeyCode::Down => {
                    self.slash_suggestion_index = (self.slash_suggestion_index + 1)
                        .min(self.slash_suggestions.len().saturating_sub(1));
                    return Ok(());
                }
                KeyCode::Up => {
                    self.slash_suggestion_index = self.slash_suggestion_index.saturating_sub(1);
                    return Ok(());
                }
                KeyCode::Enter => {
                    // Accept the selected suggestion
                    if let Some(cmd) = self.slash_suggestions.get(self.slash_suggestion_index) {
                        self.input_buffer = format!("/{cmd}");
                        self.input_cursor = self.input_buffer.len();
                        self.thread_compose_buffer = self.input_buffer.clone();
                        self.thread_compose_cursor = self.input_cursor;
                    }
                    self.slash_suggestions.clear();
                    self.slash_suggestion_index = 0;
                    return Ok(());
                }
                KeyCode::Esc => {
                    self.slash_suggestions.clear();
                    self.slash_suggestion_index = 0;
                    return Ok(());
                }
                _ => {
                    // Any other key: dismiss suggestions, continue with normal edit
                    self.slash_suggestions.clear();
                    self.slash_suggestion_index = 0;
                }
            }
        }

        // Let tab-switching and dashboard keys pass through compose mode
        // so the user is never trapped in compose.
        match (code, modifiers) {
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                // Save draft and go to dashboard
                self.thread_compose_buffer = self.input_buffer.clone();
                self.thread_compose_cursor = self.input_cursor.min(self.input_buffer.len());
                self.input_mode = InputMode::Normal;
                self.active_tab = 0;
                return Ok(());
            }
            (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                // Save draft and switch to next tab
                self.thread_compose_buffer = self.input_buffer.clone();
                self.thread_compose_cursor = self.input_cursor.min(self.input_buffer.len());
                self.input_mode = InputMode::Normal;
                if self.tabs.len() > 1 {
                    self.active_tab = (self.active_tab + 1) % self.tabs.len();
                }
                return Ok(());
            }
            (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                // Save draft and switch to previous tab
                self.thread_compose_buffer = self.input_buffer.clone();
                self.thread_compose_cursor = self.input_cursor.min(self.input_buffer.len());
                self.input_mode = InputMode::Normal;
                if self.tabs.len() > 1 {
                    self.active_tab = if self.active_tab == 0 {
                        self.tabs.len() - 1
                    } else {
                        self.active_tab - 1
                    };
                }
                return Ok(());
            }
            _ => {}
        }

        match code {
            KeyCode::Esc => {
                self.thread_compose_buffer = self.input_buffer.clone();
                self.thread_compose_cursor = self.input_cursor.min(self.input_buffer.len());
                self.slash_suggestions.clear();
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Enter => {
                self.thread_compose_buffer = self.input_buffer.clone();
                self.thread_compose_cursor = self.input_cursor.min(self.input_buffer.len());
                self.slash_suggestions.clear();
                self.submit_thread_compose_message()?;
            }
            KeyCode::Char('v') if modifiers == KeyModifiers::CONTROL => {
                self.smart_paste_into_compose()?;
            }
            // Up arrow: cycle to previous compose history entry
            KeyCode::Up if modifiers.is_empty() && !self.compose_history.is_empty() => {
                let new_idx = match self.compose_history_index {
                    None => self.compose_history.len() - 1,
                    Some(0) => 0,
                    Some(i) => i - 1,
                };
                self.compose_history_index = Some(new_idx);
                self.input_buffer.clone_from(&self.compose_history[new_idx]);
                self.input_cursor = self.input_buffer.len();
                self.thread_compose_buffer.clone_from(&self.input_buffer);
                self.thread_compose_cursor = self.input_cursor;
            }
            // Down arrow: cycle to next compose history entry (or clear)
            KeyCode::Down if modifiers.is_empty() && self.compose_history_index.is_some() => {
                let current = self.compose_history_index.unwrap();
                if current + 1 < self.compose_history.len() {
                    let new_idx = current + 1;
                    self.compose_history_index = Some(new_idx);
                    self.input_buffer.clone_from(&self.compose_history[new_idx]);
                } else {
                    // Past the end — clear to empty (new message)
                    self.compose_history_index = None;
                    self.input_buffer.clear();
                }
                self.input_cursor = self.input_buffer.len();
                self.thread_compose_buffer.clone_from(&self.input_buffer);
                self.thread_compose_cursor = self.input_cursor;
            }
            _ => {
                let _ = apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                );
                self.thread_compose_buffer = self.input_buffer.clone();
                self.thread_compose_cursor = self.input_cursor.min(self.input_buffer.len());

                // Update slash command suggestions
                self.update_slash_suggestions();
                // Reset history browsing when user types
                self.compose_history_index = None;
            }
        }
        Ok(())
    }

    fn update_slash_suggestions(&mut self) {
        let buf = &self.input_buffer;
        if !buf.starts_with('/') || buf.contains(' ') {
            self.slash_suggestions.clear();
            self.slash_suggestion_index = 0;
            return;
        }

        let query = &buf[1..]; // strip leading /

        // Load skills lazily if not loaded
        if self.installed_skills.is_empty() {
            let mut skills = crate::skills::list_skills(true, None).unwrap_or_default();
            if let Some(project) = self.selected_project() {
                skills.extend(
                    crate::skills::list_skills(false, Some(&project.repo_path)).unwrap_or_default(),
                );
            }
            self.installed_skills = skills;
        }

        // Filter skills matching the query
        let mut suggestions: Vec<String> = self
            .installed_skills
            .iter()
            .filter(|skill| {
                query.is_empty() || skill.name.to_lowercase().contains(&query.to_lowercase())
            })
            .map(|skill| skill.name.clone())
            .take(10)
            .collect();

        // Also add built-in workflow stage prompts
        let builtins = ["plan", "tdd", "verify", "review", "learn"];
        for cmd in builtins {
            if (query.is_empty() || cmd.contains(&query.to_lowercase()))
                && !suggestions.iter().any(|s| s == cmd)
            {
                suggestions.push(cmd.to_string());
            }
        }

        suggestions.truncate(10);
        self.slash_suggestion_index = self
            .slash_suggestion_index
            .min(suggestions.len().saturating_sub(1));
        self.slash_suggestions = suggestions;
    }

    /// Smart paste from clipboard: captures images as thread attachments,
    /// or pastes text into the compose buffer.
    fn smart_paste_into_compose(&mut self) -> Result<()> {
        let thread_id = self
            .thread_compose_thread_id
            .clone()
            .or_else(|| self.selected_thread().map(|t| t.id.clone()))
            .or_else(|| self.active_session_thread().map(|t| t.id));
        let Some(thread_id) = thread_id else {
            self.show_toast("No thread for paste", ToastStyle::Info);
            return Ok(());
        };

        match crate::threads::smart_paste_clipboard(&self.store, &thread_id, None) {
            Ok(crate::threads::SmartPasteResult::Image(attachment)) => {
                // Image pasted — insert a reference in the compose buffer
                let ref_text = format!("[image: {}]", attachment.file_name);
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), &ref_text);
                self.input_cursor =
                    (self.input_cursor + ref_text.len()).min(self.input_buffer.len());
                self.thread_compose_buffer = self.input_buffer.clone();
                self.thread_compose_cursor = self.input_cursor;
                self.show_toast(
                    format!("Image attached: {}", attachment.file_name),
                    ToastStyle::Success,
                );
            }
            Ok(crate::threads::SmartPasteResult::Text(text)) => {
                // Text paste — insert into compose buffer
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), &text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
                self.thread_compose_buffer = self.input_buffer.clone();
                self.thread_compose_cursor = self.input_cursor;
            }
            Err(err) => {
                self.show_toast(format!("Paste failed: {err:#}"), ToastStyle::Error);
            }
        }
        Ok(())
    }

    pub(super) fn handle_settings_edit_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Esc => {
                self.settings_edit_target = None;
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Enter => {
                let Some(target) = self.settings_edit_target else {
                    self.input_mode = InputMode::Normal;
                    return Ok(());
                };
                if target == SettingsEditTarget::WorkflowName {
                    // Special handling: rename workflow file
                    let new_name = self.input_buffer.trim().to_string();
                    if new_name.is_empty() {
                        self.show_toast("Workflow name cannot be empty", ToastStyle::Error);
                        return Ok(());
                    }
                    let defs = self.store.list_workflow_defs().unwrap_or_default();
                    if let Some(old_def) = defs.get(self.settings_workflow_index) {
                        let old_name = old_def.name.clone();
                        if old_name != new_name {
                            // Load, rename, save new, delete old
                            if let Ok(mut definition) =
                                serde_yaml::from_str::<crate::workflows::WorkflowDefinition>(
                                    &old_def.definition_yaml,
                                )
                            {
                                definition.name = new_name.clone();
                                let _ = crate::workflows::save_workflow_definition(&definition);
                                let _ = crate::workflows::delete_workflow_definition(&old_name);
                                let _ = crate::workflows::sync_workflow_definitions(
                                    &self.store,
                                    None,
                                );
                                self.refresh_available_workflow_names();
                                self.show_toast(
                                    format!("Renamed: {old_name} → {new_name}"),
                                    ToastStyle::Success,
                                );
                            }
                        }
                    }
                    self.settings_edit_target = None;
                    self.input_mode = InputMode::Normal;
                } else {
                    target.apply(&mut self.config, &self.input_buffer)?;
                    if matches!(
                        target,
                        SettingsEditTarget::ClientId
                            | SettingsEditTarget::AppSlug
                            | SettingsEditTarget::InstallUrl
                            | SettingsEditTarget::RelayUrl
                            | SettingsEditTarget::InstallationId
                    ) {
                        crate::config::save_github_app_config(&self.config.github_app)?;
                    } else {
                        crate::config::save_settings_config(&self.config)?;
                    }
                    self.github_status =
                        crate::github_app::local_status(&self.config.github_app)
                            .unwrap_or_default();
                    self.settings_edit_target = None;
                    self.input_mode = InputMode::Normal;
                    self.show_toast(format!("Saved {}", target.label()), ToastStyle::Success);
                }
            }
            _ => {
                let _ = apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                );
            }
        }
        Ok(())
    }

    pub(super) fn handle_settings_picker_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.settings_picker_target = None;
                self.settings_picker_options.clear();
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.settings_picker_index + 1 < self.settings_picker_options.len() {
                    self.settings_picker_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.settings_picker_index = self.settings_picker_index.saturating_sub(1);
            }
            KeyCode::Enter => {
                let Some(target) = self.settings_picker_target else {
                    self.input_mode = InputMode::Normal;
                    return Ok(());
                };
                let Some(value) = self.settings_picker_options.get(self.settings_picker_index)
                else {
                    self.input_mode = InputMode::Normal;
                    return Ok(());
                };
                target.apply(&mut self.config, value)?;
                crate::config::save_settings_config(&self.config)?;
                let label = target.label();
                self.settings_picker_target = None;
                self.settings_picker_options.clear();
                self.input_mode = InputMode::Normal;
                self.show_toast(format!("Saved {label}"), ToastStyle::Success);
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn handle_github_installation_picker_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.github_installations.is_empty() {
                    self.github_installation_index = (self.github_installation_index + 1)
                        .min(self.github_installations.len().saturating_sub(1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.github_installation_index = self.github_installation_index.saturating_sub(1);
            }
            KeyCode::Char('f' | 'r') => {
                self.spawn_github_installations_fetch();
                self.show_toast("Refreshing GitHub installations...", ToastStyle::Info);
            }
            KeyCode::Enter => {
                if let Some(installation) = self
                    .github_installations
                    .get(self.github_installation_index)
                {
                    self.config.github_app.default_installation_id =
                        Some(installation.id.to_string());
                    crate::config::save_github_app_config(&self.config.github_app)?;
                    self.github_status = crate::github_app::local_status(&self.config.github_app)
                        .unwrap_or_default();
                    self.input_mode = InputMode::Normal;
                    self.show_toast(
                        format!(
                            "Selected GitHub installation {} ({})",
                            installation.account.login, installation.id
                        ),
                        ToastStyle::Success,
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn handle_github_project_picker_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        let visible_indices = self.visible_github_project_indices();
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.input_buffer.clear();
                self.input_cursor = 0;
                self.input_mode = if self.workbench_view == WorkbenchView::SprintBoard {
                    InputMode::BoardView
                } else {
                    InputMode::Normal
                };
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(current_position) = visible_indices
                    .iter()
                    .position(|index| *index == self.github_project_index)
                    .or((!visible_indices.is_empty()).then_some(0))
                {
                    let next_position =
                        (current_position + 1).min(visible_indices.len().saturating_sub(1));
                    self.github_project_index = visible_indices[next_position];
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(current_position) = visible_indices
                    .iter()
                    .position(|index| *index == self.github_project_index)
                    .or((!visible_indices.is_empty()).then_some(0))
                {
                    self.github_project_index = visible_indices[current_position.saturating_sub(1)];
                }
            }
            KeyCode::Char('f' | 'R') => {
                self.refresh_board_issues();
                if let Some(first_visible) = self.visible_github_project_indices().first().copied()
                {
                    self.github_project_index = first_visible;
                }
                self.show_toast("Refreshing GitHub projects...", ToastStyle::Info);
            }
            KeyCode::Char('x') => {
                self.config.github_app.default_project_id = None;
                crate::config::save_github_app_config(&self.config.github_app)?;
                self.github_status =
                    crate::github_app::local_status(&self.config.github_app).unwrap_or_default();
                self.input_mode = if self.workbench_view == WorkbenchView::SprintBoard {
                    self.load_board_issues_from_cache();
                    InputMode::BoardView
                } else {
                    InputMode::Normal
                };
                self.show_toast(
                    "Cleared default GitHub project; board will use the first synced project",
                    ToastStyle::Success,
                );
            }
            KeyCode::Enter => {
                if let Some(project) = self.github_projects_v2.get(self.github_project_index) {
                    let project_number = project.project_number;
                    let project_label = project.display_label();
                    self.config.github_app.default_project_id = Some(project_number.to_string());
                    crate::config::save_github_app_config(&self.config.github_app)?;
                    self.github_status = crate::github_app::local_status(&self.config.github_app)
                        .unwrap_or_default();
                    self.input_mode = if self.workbench_view == WorkbenchView::SprintBoard {
                        self.load_board_issues_from_cache();
                        InputMode::BoardView
                    } else {
                        InputMode::Normal
                    };
                    self.show_toast(
                        format!("Selected default GitHub project {project_label}"),
                        ToastStyle::Success,
                    );
                }
            }
            _ => {
                if apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                ) && let Some(first_visible) =
                    self.visible_github_project_indices().first().copied()
                {
                    self.github_project_index = first_visible;
                }
            }
        }
        Ok(())
    }

    pub(super) fn handle_project_picker_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.project_picker_index + 1 < self.projects.len() {
                    self.project_picker_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.project_picker_index = self.project_picker_index.saturating_sub(1);
            }
            KeyCode::Enter => {
                if self.project_picker_index < self.projects.len() {
                    self.project_index = self.project_picker_index;
                    self.sidebar_cursor =
                        App::sidebar_nav_count().saturating_add(self.project_picker_index);
                    self.task_index = 0;
                    self.thread_index = 0;
                    self.review_index = 0;
                    self.inspector_scroll = 0;
                    self.refresh_data()?;
                    self.input_mode = if self.workbench_view == WorkbenchView::SprintBoard {
                        self.load_board_issues_from_cache();
                        InputMode::BoardView
                    } else {
                        InputMode::Normal
                    };
                    if let Some(project) = self.selected_project() {
                        self.show_toast(
                            format!("Switched to project {}", project.name),
                            ToastStyle::Success,
                        );
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn handle_thread_provider_picker_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char('j' | 'l') | KeyCode::Down | KeyCode::Right => {
                self.thread_provider_picker_index =
                    (self.thread_provider_picker_index + 1) % LAUNCH_THREAD_PROVIDERS.len();
            }
            KeyCode::Char('k' | 'h') | KeyCode::Up | KeyCode::Left => {
                self.thread_provider_picker_index = self
                    .thread_provider_picker_index
                    .checked_sub(1)
                    .unwrap_or(LAUNCH_THREAD_PROVIDERS.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                let Some(thread) = self.thread_provider_picker_target() else {
                    self.input_mode = InputMode::Normal;
                    self.show_toast("Select a thread first", ToastStyle::Info);
                    return Ok(());
                };
                let provider_kind = LAUNCH_THREAD_PROVIDERS[self.thread_provider_picker_index];
                self.switch_thread_provider_interactive(&thread, provider_kind)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Execute a normal-mode action. Context-dependent actions (e.g. `k` = kill
    /// or move-up, `l` = focus or launch) are resolved here based on current state.
    fn execute_action(&mut self, action: super::super::keymap::Action) -> Result<()> {
        use super::super::keymap::Action;
        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::OpenCommandPalette => {
                self.input_mode = InputMode::CommandPalette;
                self.input_buffer.clear();
                self.palette_index = 0;
                self.filter_palette();
            }
            Action::PrevTab => self.prev_tab(),
            Action::NextTab => self.next_tab(),
            Action::FocusProjects => {
                if self.workbench_view == WorkbenchView::SprintBoard
                    && self.focus == Focus::Tasks
                    && self.board_column_index > 0
                {
                    self.board_column_index -= 1;
                    self.board_issue_index = 0;
                } else if self.focus == Focus::Inspector {
                    self.focus = Focus::Tasks;
                } else {
                    self.focus = Focus::Projects;
                }
            }
            Action::FocusTasks => {
                if self.workbench_view == WorkbenchView::SprintBoard
                    && self.focus == Focus::Tasks
                    && self.board_column_index + 1 < self.board_columns.len()
                {
                    self.board_column_index += 1;
                    self.board_issue_index = 0;
                } else {
                    self.focus = Focus::Tasks;
                }
            }
            Action::ShowHelp => {
                self.input_mode = InputMode::HelpOverlay;
            }
            Action::FilterTasks => {
                self.task_filter.clear();
                self.recompute_visible_tasks();
                self.input_mode = InputMode::TaskFilter;
                self.focus = Focus::Tasks;
            }
            Action::ResizePaneNarrow => self.resize_workbench_pane(-2),
            Action::ResizePaneWide => self.resize_workbench_pane(2),
            Action::MoveDown => self.move_down(),
            Action::MoveUp => self.move_up(),
            Action::ReorderTaskDown => {
                if self.focus == Focus::Tasks {
                    let visible = self.visible_tasks();
                    if let (Some(current), Some(next)) = (
                        visible.get(self.task_index),
                        visible.get(self.task_index + 1),
                    ) {
                        let current_id = current.id.clone();
                        let next_id = next.id.clone();
                        if self.store.swap_task_order(&current_id, &next_id).is_ok() {
                            self.task_index += 1;
                            let _ = self.refresh_data();
                        }
                    }
                }
            }
            Action::ReorderTaskUp => {
                if self.focus == Focus::Tasks && self.task_index > 0 {
                    let visible = self.visible_tasks();
                    if let (Some(current), Some(prev)) = (
                        visible.get(self.task_index),
                        visible.get(self.task_index - 1),
                    ) {
                        let current_id = current.id.clone();
                        let prev_id = prev.id.clone();
                        if self.store.swap_task_order(&current_id, &prev_id).is_ok() {
                            self.task_index -= 1;
                            let _ = self.refresh_data();
                        }
                    }
                }
            }
            Action::Select => match self.focus {
                Focus::Projects => match self.selected_sidebar_item() {
                    SidebarItem::Navigation(view) => {
                        self.switch_workbench_view(view);
                    }
                    SidebarItem::Repository(index) => {
                        if index < self.projects.len() {
                            self.project_index = index;
                            self.refresh_data()?;
                            self.task_index = 0;
                            self.review_index = 0;
                            self.thread_index = 0;
                            if self.workbench_view == WorkbenchView::SprintBoard {
                                self.load_board_issues_from_cache();
                                self.input_mode = InputMode::BoardView;
                            }
                        }
                    }
                },
                Focus::Tasks => {
                    if self.workbench_view == WorkbenchView::MyTasks && self.uses_github_my_tasks()
                    {
                        self.task_details_scroll = 0;
                        self.refresh_my_task_selected_comments();
                        self.input_mode = InputMode::MyTaskDrawer;
                    } else if self.workbench_view == WorkbenchView::Reviews {
                        if let Some(thread) = self.selected_thread().cloned() {
                            self.focus_thread_workspace(&thread.id);
                            self.show_toast("Focused review thread", ToastStyle::Info);
                        } else if self.selected_review_item().is_some() {
                            // Open the drawer instead of immediately launching
                            self.review_drawer_tab = super::ReviewDrawerTab::Description;
                            self.review_drawer_scroll = 0;
                            self.refresh_review_selected_context();
                            self.input_mode = InputMode::ReviewDrawer;
                        }
                    } else if self.workbench_view == WorkbenchView::Threads {
                        if let Some(thread) = self.selected_thread().cloned() {
                            self.continue_thread(thread)?;
                        } else {
                            self.open_adhoc_launch_modal()?;
                        }
                    } else if self.workbench_view == WorkbenchView::Settings {
                        if SettingsSection::ALL[self.settings_section_index]
                            == SettingsSection::Permissions
                        {
                            self.cached_config_status = Some(
                                crate::configure::load_config_status().map_err(|e| e.to_string()),
                            );
                            self.input_mode = InputMode::ConfigureWizard;
                        }
                    } else if self.workbench_view == WorkbenchView::SprintBoard {
                        if self.selected_board_issue().is_some() {
                            self.task_details_scroll = 0;
                            self.input_mode = InputMode::TaskDetails;
                        }
                    } else if let Some(task) = self.selected_task() {
                        if let Some(session_id) = &task.session_id {
                            let session = self.store.get_session(session_id)?;
                            if session.closed_at.is_none() {
                                let _ = crate::threads::ensure_thread_bridge_for_session(
                                    &self.store,
                                    &session,
                                    Some(task),
                                )?;
                                if !self.goto_session_tab(&session.id) {
                                    self.restore_session_tab(&session)?;
                                } else if let Some(Tab::Session { view_mode, .. }) = self
                                    .tabs
                                    .iter_mut()
                                    .find(|tab| matches!(tab, Tab::Session { session_id: sid, .. } if sid == &session.id))
                                {
                                    *view_mode = SessionTabView::Conversation;
                                }
                            } else {
                                self.show_toast("Session is closed", ToastStyle::Info);
                            }
                        } else if let Some(thread) = self
                            .threads
                            .iter()
                            .find(|thread| thread.task_id.as_deref() == Some(task.id.as_str()))
                            .cloned()
                        {
                            self.focus_thread_workspace(&thread.id);
                            self.show_toast("Focused task thread", ToastStyle::Info);
                        } else if matches!(
                            task.status,
                            crate::store::TaskStatus::Pending | crate::store::TaskStatus::Draft
                        ) {
                            self.open_task_launch_modal()?;
                        }
                    }
                }
                Focus::Inspector => {}
            },
            Action::ViewTaskDetails => {
                let can_show_details = match self.workbench_view {
                    WorkbenchView::SprintBoard => self.selected_board_issue().is_some(),
                    WorkbenchView::MyTasks => {
                        if self.uses_github_my_tasks() {
                            self.selected_my_task_item().is_some()
                        } else {
                            self.selected_task().is_some()
                        }
                    }
                    WorkbenchView::Reviews => self.selected_review_item().is_some(),
                    _ => self.selected_task().is_some(),
                };
                if self.focus == Focus::Tasks && can_show_details {
                    self.task_details_scroll = 0;
                    if self.workbench_view == WorkbenchView::Reviews {
                        self.review_drawer_tab = super::ReviewDrawerTab::Description;
                        self.review_drawer_scroll = 0;
                        self.refresh_review_selected_context();
                        self.input_mode = InputMode::ReviewDrawer;
                    } else if self.workbench_view == WorkbenchView::MyTasks
                        && self.uses_github_my_tasks()
                    {
                        self.refresh_my_task_selected_comments();
                        self.input_mode = InputMode::MyTaskDrawer;
                    } else {
                        self.input_mode = InputMode::TaskDetails;
                    }
                }
            }
            Action::OpenSubtasks => {
                if self.focus == Focus::Tasks && self.selected_task().is_some() {
                    if let Some(task) = self.selected_task() {
                        self.subtasks = self
                            .store
                            .list_subtasks_for_task(&task.id)
                            .unwrap_or_default();
                    }
                    self.subtask_index = 0;
                    self.input_buffer.clear();
                    self.input_mode = InputMode::SubtaskPanel;
                }
            }
            Action::NewTask => {
                if self.workbench_view == WorkbenchView::Threads && self.focus == Focus::Tasks {
                    self.open_adhoc_launch_modal()?;
                } else if self.selected_project().is_some() {
                    self.reset_task_form();
                    self.input_mode = InputMode::NewTask;
                }
            }
            Action::EditTask => {
                if self.focus == Focus::Tasks {
                    let task_data = self.selected_task().map(|t| {
                        (
                            t.id.clone(),
                            t.title.clone(),
                            t.description.clone(),
                            t.mode,
                            t.status,
                            t.base.clone(),
                            t.branch.clone(),
                            t.push_mode,
                            t.review_loop,
                        )
                    });
                    if let Some((
                        id,
                        _title,
                        desc,
                        mode,
                        status,
                        base,
                        branch,
                        push_mode,
                        review_loop,
                    )) = task_data
                        && matches!(
                            status,
                            crate::store::TaskStatus::Pending | crate::store::TaskStatus::Draft
                        )
                    {
                        self.editing_task_id = Some(id);
                        self.new_task_description.clone_from(&desc);
                        self.new_task_mode = mode;
                        self.new_task_base = base.unwrap_or_default();
                        self.new_task_branch = branch.unwrap_or_default();
                        self.new_task_push_mode = push_mode;
                        self.new_task_review_loop = review_loop;
                        self.new_task_field = 0;
                        self.input_buffer.clone_from(&desc);
                        self.input_cursor = self.input_buffer.len();
                        self.input_mode = InputMode::EditTask;
                    }
                }
            }
            Action::MarkDone => {
                if self.focus == Focus::Tasks
                    && let Some(task) = self.selected_task()
                    && matches!(
                        task.status,
                        crate::store::TaskStatus::InReview
                            | crate::store::TaskStatus::Working
                            | crate::store::TaskStatus::Interrupted
                            | crate::store::TaskStatus::CiFailed
                    )
                {
                    self.store
                        .update_task_status(&task.id, crate::store::TaskStatus::Done)?;
                    if let Some(ref sid) = task.session_id {
                        self.spawn_teardown_session(sid.clone());
                    }
                    self.refresh_data()?;
                    self.show_toast("Task marked as done", ToastStyle::Success);
                    crate::sync::try_auto_push();
                }
            }
            // `k` = kill session when a running task is focused, otherwise vim-style move up
            Action::KillSession => {
                let mut killed = false;
                if self.focus == Focus::Tasks {
                    if self.session_op_in_progress {
                        self.show_toast("Session operation in progress...", ToastStyle::Info);
                        killed = true;
                    } else if let Some(task) = self.selected_task()
                        && let Some(ref sid) = task.session_id
                        && matches!(
                            task.status,
                            crate::store::TaskStatus::Working
                                | crate::store::TaskStatus::InReview
                                | crate::store::TaskStatus::CiFailed
                                | crate::store::TaskStatus::Error
                        )
                    {
                        let sid = sid.clone();
                        self.store
                            .update_task_status(&task.id, crate::store::TaskStatus::Pending)?;
                        self.store.unassign_task_from_session(&task.id)?;
                        self.spawn_teardown_session(sid);
                        self.refresh_data()?;
                        self.show_toast("Session killed — press Enter to resume", ToastStyle::Info);
                        killed = true;
                    }
                }
                if !killed {
                    self.move_up();
                }
            }
            Action::OpenPR => {
                if self.focus == Focus::Tasks
                    && self.workbench_view == WorkbenchView::Threads
                    && let Some(thread) = self.selected_thread().cloned()
                {
                    self.open_thread_live_session(&thread)?;
                } else if self.focus == Focus::Tasks
                    && self.workbench_view == WorkbenchView::MyTasks
                    && self.uses_github_my_tasks()
                    && let Some(item) = self.selected_my_task_item()
                {
                    let opener = if cfg!(target_os = "macos") {
                        "open"
                    } else {
                        "xdg-open"
                    };
                    let _ = std::process::Command::new(opener)
                        .arg(&item.github_item.url)
                        .spawn();
                    self.show_toast("Opening GitHub item in browser", ToastStyle::Success);
                } else if self.focus == Focus::Tasks
                    && self.workbench_view == WorkbenchView::Reviews
                    && let Some(review_item) = self.selected_review_item()
                {
                    let opener = if cfg!(target_os = "macos") {
                        "open"
                    } else {
                        "xdg-open"
                    };
                    let _ = std::process::Command::new(opener)
                        .arg(&review_item.github_item.url)
                        .spawn();
                    self.show_toast("Opening PR in browser", ToastStyle::Success);
                } else if self.focus == Focus::Tasks
                    && let Some(task) = self.selected_task()
                    && let Some(ref url) = task.pr_url
                {
                    let opener = if cfg!(target_os = "macos") {
                        "open"
                    } else {
                        "xdg-open"
                    };
                    let _ = std::process::Command::new(opener).arg(url).spawn();
                    self.show_toast("Opening PR in browser", ToastStyle::Success);
                }
            }
            // `l` = focus tasks when on projects, launch task when on tasks.
            // If the task already has a session (stuck/working), tear it down first
            // and relaunch in a fresh session.
            Action::LaunchTask => {
                if self.focus == Focus::Projects {
                    self.focus = Focus::Tasks;
                } else if self.workbench_view == WorkbenchView::MyTasks
                    && self.uses_github_my_tasks()
                {
                    self.open_my_task_launch_modal()?;
                } else if self.workbench_view == WorkbenchView::SprintBoard {
                    if let Some(issue) = self.selected_board_issue() {
                        if let Some(thread) = self
                            .store
                            .find_thread_for_github_item(&issue.github_item_id)?
                        {
                            self.focus_thread_workspace(&thread.id);
                            self.show_toast("Focused existing thread", ToastStyle::Info);
                        } else {
                            self.open_board_issue_launch_modal()?;
                        }
                    }
                } else if self.workbench_view == WorkbenchView::Reviews {
                    if let Some(thread) = self.selected_thread().cloned() {
                        self.focus_thread_workspace(&thread.id);
                        self.show_toast("Focused existing thread", ToastStyle::Info);
                    } else {
                        self.open_review_launch_modal()?;
                    }
                } else if self.workbench_view == WorkbenchView::Threads {
                    if let Some(thread) = self.selected_thread().cloned() {
                        self.continue_thread(thread)?;
                    } else {
                        self.open_adhoc_launch_modal()?;
                    }
                } else if self.session_op_in_progress {
                    self.show_toast("Session operation in progress...", ToastStyle::Info);
                } else if let Some(task) = self.selected_task() {
                    if matches!(
                        task.status,
                        crate::store::TaskStatus::Pending | crate::store::TaskStatus::Draft
                    ) {
                        self.open_task_launch_modal()?;
                    } else if let Some(thread) = self
                        .threads
                        .iter()
                        .find(|thread| thread.task_id.as_deref() == Some(task.id.as_str()))
                        .cloned()
                    {
                        self.focus_thread_workspace(&thread.id);
                        self.show_toast("Focused existing thread", ToastStyle::Info);
                    } else if let Some(project_id) = self.selected_project().map(|p| p.id.clone()) {
                        // Legacy relaunch path for pre-thread sessions.
                        if let Some(ref sid) = task.session_id
                            && matches!(
                                task.status,
                                crate::store::TaskStatus::Working
                                    | crate::store::TaskStatus::InReview
                                    | crate::store::TaskStatus::CiFailed
                                    | crate::store::TaskStatus::Error
                            )
                        {
                            let sid = sid.clone();
                            self.store
                                .update_task_status(&task.id, crate::store::TaskStatus::Pending)?;
                            self.store.unassign_task_from_session(&task.id)?;
                            self.pending_relaunch = Some((task.id.clone(), project_id));
                            self.spawn_teardown_session(sid);
                            self.refresh_data()?;
                            self.show_toast("Relaunching task in new session...", ToastStyle::Info);
                        }
                    }
                }
            }
            Action::DeleteItem => match self.focus {
                Focus::Projects => {
                    if let SidebarItem::Repository(index) = self.selected_sidebar_item()
                        && let Some((name, id)) = self
                            .projects
                            .get(index)
                            .map(|project| (project.name.clone(), project.id.clone()))
                    {
                        self.confirm_target = name;
                        self.confirm_entity_id = id;
                        self.confirm_delete_kind = DeleteTarget::Project;
                        self.input_mode = InputMode::ConfirmDelete;
                    }
                }
                Focus::Tasks => {
                    let task_data = self
                        .selected_task()
                        .map(|t| (t.id.clone(), t.title.clone()));
                    if let Some((id, title)) = task_data {
                        self.confirm_target = title;
                        self.confirm_entity_id = id;
                        self.confirm_delete_kind = DeleteTarget::Task;
                        self.input_mode = InputMode::ConfirmDelete;
                    }
                }
                Focus::Inspector => {}
            },
            Action::OpenSkills => {
                self.refresh_skills();
                self.skill_index = 0;
                self.input_mode = InputMode::SkillPanel;
            }
            Action::AddProject => {
                self.input_mode = InputMode::NewProject;
                self.input_buffer.clear();
                self.new_project_name.clear();
                self.new_project_path = String::from(".");
                self.new_project_git_linked = true;
                self.new_project_field = 0;
                self.clear_path_autocomplete();
            }
            Action::Configure => {
                self.cached_config_status =
                    Some(crate::configure::load_config_status().map_err(|e| e.to_string()));
                self.input_mode = InputMode::ConfigureWizard;
            }
            Action::OpenBoard => {
                if let Some(project) = self.selected_project() {
                    if project.is_git_linked {
                        self.switch_workbench_view(WorkbenchView::SprintBoard);
                    } else {
                        self.show_toast("Project not linked to git", ToastStyle::Info);
                    }
                }
            }
            // Session-only actions are no-ops in normal mode
            Action::ReturnToDashboard
            | Action::FocusPrevPane
            | Action::FocusNextPane
            | Action::ScrollToBottom
            | Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::SplitRight
            | Action::SplitDown
            | Action::ClosePane
            | Action::CommentOnIssue
            | Action::CloseIssue
            | Action::CreateIssue
            | Action::EditIssue => {}
        }
        Ok(())
    }

    /// Handle keys shared between new-task and edit-task forms (tab, back-tab, mode toggle, typing).
    /// Returns `true` if the key was consumed.
    fn handle_task_form_shared_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let field_count: u8 = 7;
        match code {
            // On subtask field with subtasks: Tab cycles through them
            KeyCode::Tab if self.new_task_field == 6 && !self.new_task_subtasks.is_empty() => {
                // If editing, save the current edit first
                if let Some(idx) = self.editing_subtask_index {
                    let trimmed = self.input_buffer.trim().to_string();
                    if !trimmed.is_empty() {
                        self.new_task_subtasks[idx] = trimmed;
                    }
                    self.editing_subtask_index = None;
                    self.input_buffer.clear();
                }
                self.new_task_subtask_index =
                    (self.new_task_subtask_index + 1) % self.new_task_subtasks.len();
                true
            }
            KeyCode::Tab => {
                self.editing_subtask_index = None;
                self.save_current_task_field();
                self.new_task_field = (self.new_task_field + 1) % field_count;
                self.load_current_task_field();
                true
            }
            KeyCode::BackTab => {
                // Cancel any editing state when leaving field 4
                self.editing_subtask_index = None;
                self.save_current_task_field();
                self.new_task_field = if self.new_task_field == 0 {
                    field_count - 1
                } else {
                    self.new_task_field - 1
                };
                self.load_current_task_field();
                true
            }
            KeyCode::Left | KeyCode::Right if self.new_task_field == 1 && modifiers.is_empty() => {
                use crate::store::TaskMode::{Autonomous, Exploration, Supervised};
                // Right: Autonomous -> Supervised -> Exploration -> ...
                // Left:  Autonomous -> Exploration -> Supervised -> ...
                self.new_task_mode = match (code, self.new_task_mode) {
                    (KeyCode::Right, Autonomous) | (KeyCode::Left, Exploration) => Supervised,
                    (KeyCode::Right, Supervised) | (KeyCode::Left, Autonomous) => Exploration,
                    (KeyCode::Right, Exploration) | (KeyCode::Left, Supervised) => Autonomous,
                    _ => unreachable!(),
                };
                true
            }
            KeyCode::Left | KeyCode::Right if self.new_task_field == 4 && modifiers.is_empty() => {
                self.new_task_push_mode = match self.new_task_push_mode {
                    crate::store::PushMode::Pr => crate::store::PushMode::Push,
                    crate::store::PushMode::Push => crate::store::PushMode::Pr,
                };
                true
            }
            KeyCode::Left | KeyCode::Right if self.new_task_field == 5 && modifiers.is_empty() => {
                self.new_task_review_loop = !self.new_task_review_loop;
                true
            }
            // Subtask input field: typing, add, delete, navigate
            _ if self.new_task_field == 6 => self.handle_subtask_input_key(code, modifiers),
            // Base field: text input
            _ if self.new_task_field == 2 => apply_text_edit(
                &mut self.input_buffer,
                &mut self.input_cursor,
                code,
                modifiers,
            ),
            // Branch field: text input
            _ if self.new_task_field == 3 => apply_text_edit(
                &mut self.input_buffer,
                &mut self.input_cursor,
                code,
                modifiers,
            ),
            _ if self.new_task_field == 0 => apply_text_edit(
                &mut self.input_buffer,
                &mut self.input_cursor,
                code,
                modifiers,
            ),
            _ => false,
        }
    }

    /// Handle keys when the subtask input field (field 2) is focused in the task form.
    fn handle_subtask_input_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        match code {
            // Esc while editing a subtask: cancel edit
            KeyCode::Esc if self.editing_subtask_index.is_some() => {
                self.editing_subtask_index = None;
                self.input_buffer.clear();
                true
            }
            // Enter while editing: save edited subtask (trim, reject empty)
            KeyCode::Enter if self.editing_subtask_index.is_some() => {
                let trimmed = self.input_buffer.trim().to_string();
                if let Some(idx) = self.editing_subtask_index
                    && !trimmed.is_empty()
                {
                    self.new_task_subtasks[idx] = trimmed;
                }
                self.editing_subtask_index = None;
                self.input_buffer.clear();
                true
            }
            // Enter with text, not editing: add new subtask (trim, reject empty)
            KeyCode::Enter if !self.input_buffer.is_empty() => {
                let trimmed = self.input_buffer.trim().to_string();
                self.input_buffer.clear();
                if !trimmed.is_empty() {
                    self.new_task_subtasks.push(trimmed);
                }
                true
            }
            // Enter with empty input: start editing selected subtask
            KeyCode::Enter
                if self.input_buffer.is_empty()
                    && !self.new_task_subtasks.is_empty()
                    && self.editing_subtask_index.is_none() =>
            {
                let idx = self.new_task_subtask_index;
                self.editing_subtask_index = Some(idx);
                self.input_buffer.clone_from(&self.new_task_subtasks[idx]);
                self.input_cursor = self.input_buffer.len();
                true
            }
            // 'd' with empty input and not editing: delete selected subtask
            KeyCode::Char('d')
                if self.input_buffer.is_empty() && self.editing_subtask_index.is_none() =>
            {
                if !self.new_task_subtasks.is_empty() {
                    self.new_task_subtasks.remove(self.new_task_subtask_index);
                    if self.new_task_subtasks.is_empty() {
                        self.new_task_subtask_index = 0;
                    } else if self.new_task_subtask_index >= self.new_task_subtasks.len() {
                        self.new_task_subtask_index = self.new_task_subtasks.len() - 1;
                    }
                }
                true
            }
            // j/k navigation only when not editing
            KeyCode::Char('j') | KeyCode::Down
                if self.input_buffer.is_empty() && self.editing_subtask_index.is_none() =>
            {
                if !self.new_task_subtasks.is_empty() {
                    self.new_task_subtask_index = (self.new_task_subtask_index + 1)
                        .min(self.new_task_subtasks.len().saturating_sub(1));
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up
                if self.input_buffer.is_empty() && self.editing_subtask_index.is_none() =>
            {
                self.new_task_subtask_index = self.new_task_subtask_index.saturating_sub(1);
                true
            }
            _ => apply_text_edit(
                &mut self.input_buffer,
                &mut self.input_cursor,
                code,
                modifiers,
            ),
        }
    }

    pub(super) fn handle_input_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        if self.handle_task_form_shared_key(code, modifiers) {
            return Ok(());
        }
        match code {
            KeyCode::Enter => {
                self.save_current_task_field();
                let is_exploration = self.new_task_mode == crate::store::TaskMode::Exploration;
                if !self.new_task_description.is_empty() || is_exploration {
                    if let Some(project_id) = self.selected_project().map(|p| p.id.clone()) {
                        let fallback = if is_exploration && self.new_task_description.is_empty() {
                            "Exploration session".to_string()
                        } else {
                            fallback_title(&self.new_task_description)
                        };
                        let branch = if self.new_task_branch.is_empty() {
                            None
                        } else {
                            Some(self.new_task_branch.as_str())
                        };
                        let base = if self.new_task_base.is_empty() {
                            None
                        } else {
                            Some(self.new_task_base.as_str())
                        };
                        let task = self.store.create_task(
                            &project_id,
                            &fallback,
                            &self.new_task_description,
                            self.new_task_mode,
                            branch,
                            base,
                            self.new_task_push_mode,
                            self.new_task_review_loop,
                        )?;

                        // Create inline subtasks
                        for subtask_desc in &self.new_task_subtasks {
                            let st_title = fallback_title(subtask_desc);
                            self.store
                                .create_subtask(&task.id, &st_title, subtask_desc)?;
                        }

                        // Launch autonomous and exploration tasks immediately,
                        // or just generate the title for supervised tasks.
                        if matches!(
                            self.new_task_mode,
                            crate::store::TaskMode::Autonomous
                                | crate::store::TaskMode::Exploration
                        ) {
                            self.launch_task(task.id, project_id)?;
                        } else {
                            let desc = self.new_task_description.clone();
                            self.spawn_title_generation(task.id, desc);
                        }
                    }
                    self.reset_task_form();
                    self.input_mode = InputMode::Normal;
                    self.refresh_data()?;
                    crate::sync::try_auto_push();
                }
            }
            KeyCode::Esc => {
                self.save_current_task_field();
                let is_exploration = self.new_task_mode == crate::store::TaskMode::Exploration;
                if (!self.new_task_description.is_empty() || is_exploration)
                    && let Some(project_id) = self.selected_project().map(|p| p.id.clone())
                {
                    let fallback = if is_exploration && self.new_task_description.is_empty() {
                        "Exploration session".to_string()
                    } else {
                        fallback_title(&self.new_task_description)
                    };
                    let branch = if self.new_task_branch.is_empty() {
                        None
                    } else {
                        Some(self.new_task_branch.as_str())
                    };
                    let base = if self.new_task_base.is_empty() {
                        None
                    } else {
                        Some(self.new_task_base.as_str())
                    };
                    let task = self.store.create_task(
                        &project_id,
                        &fallback,
                        &self.new_task_description,
                        self.new_task_mode,
                        branch,
                        base,
                        self.new_task_push_mode,
                        self.new_task_review_loop,
                    )?;
                    self.store
                        .update_task_status(&task.id, crate::store::TaskStatus::Draft)?;

                    // Create inline subtasks
                    for subtask_desc in &self.new_task_subtasks {
                        let st_title = fallback_title(subtask_desc);
                        self.store
                            .create_subtask(&task.id, &st_title, subtask_desc)?;
                    }

                    self.show_toast("Task saved as draft", ToastStyle::Info);
                    self.refresh_data()?;
                    crate::sync::try_auto_push();
                }
                self.reset_task_form();
                self.input_mode = InputMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    fn save_current_task_field(&mut self) {
        match self.new_task_field {
            0 => self.new_task_description.clone_from(&self.input_buffer),
            2 => self.new_task_base.clone_from(&self.input_buffer),
            3 => self.new_task_branch.clone_from(&self.input_buffer),
            _ => {}
        }
    }

    fn load_current_task_field(&mut self) {
        match self.new_task_field {
            0 => {
                self.input_buffer.clone_from(&self.new_task_description);
                self.input_cursor = self.input_buffer.len();
            }
            2 => {
                self.input_buffer.clone_from(&self.new_task_base);
                self.input_cursor = self.input_buffer.len();
            }
            3 => {
                self.input_buffer.clone_from(&self.new_task_branch);
                self.input_cursor = self.input_buffer.len();
            }
            _ => {
                self.input_buffer.clear();
                self.input_cursor = 0;
            }
        }
    }

    pub(super) fn handle_new_project_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        // Path field (field 1) with autocomplete support
        if self.new_project_field == 1 {
            match code {
                KeyCode::Enter => {
                    self.save_current_project_field();
                    self.clear_path_autocomplete();
                    self.submit_new_project()?;
                }
                KeyCode::Tab if self.show_path_suggestions => {
                    self.accept_path_suggestion();
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    self.save_current_project_field();
                    self.clear_path_autocomplete();
                    if code == KeyCode::Tab {
                        self.new_project_field = (self.new_project_field + 1) % 3;
                    } else {
                        self.new_project_field = (self.new_project_field + 2) % 3;
                    }
                    self.load_current_project_field();
                }
                KeyCode::Down if self.show_path_suggestions => {
                    if !self.path_suggestions.is_empty() {
                        self.path_suggestion_index = (self.path_suggestion_index + 1)
                            .min(self.path_suggestions.len().saturating_sub(1));
                    }
                }
                KeyCode::Up if self.show_path_suggestions => {
                    self.path_suggestion_index = self.path_suggestion_index.saturating_sub(1);
                }
                KeyCode::Esc if self.show_path_suggestions => {
                    self.clear_path_autocomplete();
                }
                KeyCode::Esc => {
                    self.input_buffer.clear();
                    self.new_project_name.clear();
                    self.new_project_path.clear();
                    self.new_project_git_linked = true;
                    self.new_project_field = 0;
                    self.clear_path_autocomplete();
                    self.input_mode = InputMode::Normal;
                }
                // Path field uses shared text editing with autocomplete refresh
                _ => {
                    let old_len = self.input_buffer.len();
                    let is_char = matches!(code, KeyCode::Char(_));
                    apply_text_edit(
                        &mut self.input_buffer,
                        &mut self.input_cursor,
                        code,
                        modifiers,
                    );
                    let new_len = self.input_buffer.len();

                    if new_len < old_len {
                        // Something was deleted — refresh autocomplete
                        self.refresh_path_autocomplete_after_delete();
                    } else if is_char && new_len > old_len {
                        // A character was inserted — check if it triggers autocomplete
                        let last_inserted =
                            self.input_buffer[..self.input_cursor].chars().next_back();
                        if last_inserted == Some('/')
                            || (last_inserted == Some('~') && self.input_buffer == "~")
                            || self.show_path_suggestions
                        {
                            self.update_path_suggestions();
                        }
                    }
                }
            }
        } else if self.new_project_field == 2 {
            // Git linked toggle field
            match code {
                KeyCode::Char(' ') | KeyCode::Enter => {
                    if code == KeyCode::Char(' ') {
                        self.new_project_git_linked = !self.new_project_git_linked;
                    } else {
                        self.submit_new_project()?;
                    }
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    if code == KeyCode::Tab {
                        self.new_project_field = (self.new_project_field + 1) % 3;
                    } else {
                        self.new_project_field = (self.new_project_field + 2) % 3;
                    }
                    self.load_current_project_field();
                }
                KeyCode::Esc => {
                    self.input_buffer.clear();
                    self.new_project_name.clear();
                    self.new_project_path.clear();
                    self.new_project_git_linked = true;
                    self.new_project_field = 0;
                    self.clear_path_autocomplete();
                    self.input_mode = InputMode::Normal;
                }
                _ => {}
            }
        } else {
            // Name field (field 0) — use shared text editing
            match code {
                KeyCode::Enter => {
                    self.save_current_project_field();
                    self.submit_new_project()?;
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    self.save_current_project_field();
                    if code == KeyCode::Tab {
                        self.new_project_field = (self.new_project_field + 1) % 3;
                    } else {
                        self.new_project_field = (self.new_project_field + 2) % 3;
                    }
                    self.load_current_project_field();
                }
                KeyCode::Esc => {
                    self.input_buffer.clear();
                    self.new_project_name.clear();
                    self.new_project_path.clear();
                    self.new_project_git_linked = true;
                    self.new_project_field = 0;
                    self.clear_path_autocomplete();
                    self.input_mode = InputMode::Normal;
                }
                _ => {
                    apply_text_edit(
                        &mut self.input_buffer,
                        &mut self.input_cursor,
                        code,
                        modifiers,
                    );
                }
            }
        }
        Ok(())
    }

    fn save_current_project_field(&mut self) {
        match self.new_project_field {
            0 => self.new_project_name.clone_from(&self.input_buffer),
            1 => self.new_project_path.clone_from(&self.input_buffer),
            _ => {} // field 2 is a toggle, no text to save
        }
    }

    fn submit_new_project(&mut self) -> Result<()> {
        if !self.new_project_name.is_empty() && !self.new_project_path.is_empty() {
            let name = self.new_project_name.clone();
            let path_to_resolve =
                Self::expand_tilde(&self.new_project_path).unwrap_or(self.new_project_path.clone());
            let Ok(abs_path) = std::fs::canonicalize(&path_to_resolve) else {
                self.show_toast(
                    format!("Invalid path: {path_to_resolve}"),
                    ToastStyle::Error,
                );
                return Ok(());
            };
            let Some(abs_str) = abs_path.to_str() else {
                self.show_toast("Path contains invalid UTF-8".to_string(), ToastStyle::Error);
                return Ok(());
            };
            let default_branch = crate::config::detect_default_branch(abs_str);
            self.store.create_project(
                &self.new_project_name,
                abs_str,
                &default_branch,
                self.new_project_git_linked,
            )?;
            self.new_project_name.clear();
            self.new_project_path.clear();
            self.new_project_git_linked = true;
            self.new_project_field = 0;
            self.input_buffer.clear();
            self.clear_path_autocomplete();
            self.input_mode = InputMode::Normal;
            self.refresh_data()?;
            self.show_toast(format!("Project '{name}' created"), ToastStyle::Success);
        }
        Ok(())
    }

    fn load_current_project_field(&mut self) {
        match self.new_project_field {
            0 => self.input_buffer.clone_from(&self.new_project_name),
            1 => self.input_buffer.clone_from(&self.new_project_path),
            _ => {
                self.input_buffer.clear();
            } // field 2 is a toggle, no text to load
        }
        self.input_cursor = self.input_buffer.len();
    }

    /// Expand `~` prefix to home directory in the given path string.
    fn expand_tilde(raw: &str) -> Option<String> {
        if let Some(rest) = raw.strip_prefix('~') {
            let home = dirs::home_dir()?;
            Some(home.to_string_lossy().to_string() + rest)
        } else {
            Some(raw.to_string())
        }
    }

    fn update_path_suggestions(&mut self) {
        let Some(expanded) = Self::expand_tilde(&self.input_buffer) else {
            self.show_path_suggestions = false;
            self.path_suggestions.clear();
            return;
        };

        // Split into base directory and partial name
        let (base_dir, partial) = if expanded.ends_with('/') {
            (expanded.as_str(), "")
        } else if let Some(pos) = expanded.rfind('/') {
            (&expanded[..=pos], &expanded[pos + 1..])
        } else {
            self.show_path_suggestions = false;
            self.path_suggestions.clear();
            return;
        };

        let partial_lower = partial.to_lowercase();

        let Ok(entries) = std::fs::read_dir(base_dir) else {
            self.show_path_suggestions = false;
            self.path_suggestions.clear();
            return;
        };

        let mut suggestions: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_type().is_ok_and(|ft| ft.is_dir())
                    && !e.file_name().to_string_lossy().starts_with('.')
            })
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| partial.is_empty() || name.to_lowercase().starts_with(&partial_lower))
            .collect();

        suggestions.sort_unstable();

        self.show_path_suggestions = !suggestions.is_empty();
        self.path_suggestions = suggestions;
        self.path_suggestion_index = 0;
    }

    fn accept_path_suggestion(&mut self) {
        let Some(suggestion) = self
            .path_suggestions
            .get(self.path_suggestion_index)
            .cloned()
        else {
            return;
        };

        let Some(expanded) = Self::expand_tilde(&self.input_buffer) else {
            return;
        };

        let base = if expanded.ends_with('/') {
            expanded
        } else if let Some(pos) = expanded.rfind('/') {
            expanded[..=pos].to_string()
        } else {
            return;
        };

        // Reconstruct with ~ if original started with ~
        let new_path = if self.input_buffer.starts_with('~') {
            if let Some(home) = dirs::home_dir() {
                let home_str = home.to_string_lossy().to_string();
                if let Some(rest) = base.strip_prefix(&home_str) {
                    format!("~{rest}{suggestion}/")
                } else {
                    format!("{base}{suggestion}/")
                }
            } else {
                format!("{base}{suggestion}/")
            }
        } else {
            format!("{base}{suggestion}/")
        };

        self.input_buffer = new_path;
        self.input_cursor = self.input_buffer.len();
        self.update_path_suggestions();
    }

    fn clear_path_autocomplete(&mut self) {
        self.path_suggestions.clear();
        self.path_suggestion_index = 0;
        self.show_path_suggestions = false;
    }

    /// Update or clear path autocomplete after a deletion in the path field.
    fn refresh_path_autocomplete_after_delete(&mut self) {
        if self.show_path_suggestions {
            if self.input_buffer.contains('/') || self.input_buffer == "~" {
                self.update_path_suggestions();
            } else {
                self.clear_path_autocomplete();
            }
        }
    }

    fn reset_task_form(&mut self) {
        self.input_buffer.clear();
        self.input_cursor = 0;
        self.new_task_description.clear();
        self.new_task_mode = crate::store::TaskMode::Autonomous;
        self.new_task_base.clear();
        self.new_task_branch.clear();
        self.new_task_push_mode = crate::store::PushMode::Pr;
        self.new_task_review_loop = false;
        self.new_task_field = 0;
        self.new_task_subtasks.clear();
        self.new_task_subtask_index = 0;
        self.editing_subtask_index = None;
    }

    pub(super) fn handle_confirm_delete_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Char('y') => {
                if !self.confirm_entity_id.is_empty() {
                    let name = self.confirm_target.clone();
                    match self.confirm_delete_kind {
                        DeleteTarget::Project => {
                            self.store.delete_project(&self.confirm_entity_id)?;
                            self.project_index = 0;
                            self.show_toast(
                                format!("Project '{name}' deleted"),
                                ToastStyle::Success,
                            );
                        }
                        DeleteTarget::Task => {
                            // Spawn teardown in background if task has a linked session
                            if let Ok(task) = self.store.get_task(&self.confirm_entity_id)
                                && let Some(ref sid) = task.session_id
                            {
                                self.spawn_teardown_session(sid.clone());
                            }
                            self.store.delete_task(&self.confirm_entity_id)?;
                            self.show_toast(format!("Task '{name}' deleted"), ToastStyle::Success);
                            crate::sync::try_auto_push();
                        }
                        DeleteTarget::Session => {
                            let session_id = self.confirm_entity_id.clone();
                            // Mark any linked thread as done
                            if let Ok(Some(thread)) =
                                self.store.find_thread_for_session(&session_id)
                            {
                                let _ = self.store.update_thread_status(
                                    &thread.id,
                                    crate::store::ThreadStatus::Done,
                                );
                            }
                            self.spawn_teardown_session(session_id);
                            self.show_toast(
                                format!("Session '{name}' closed permanently"),
                                ToastStyle::Success,
                            );
                        }
                    }
                    self.confirm_entity_id.clear();
                    self.confirm_target.clear();
                    self.input_mode = InputMode::Normal;
                    self.refresh_data()?;
                }
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                self.confirm_entity_id.clear();
                self.confirm_target.clear();
                self.input_mode = InputMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    fn move_down(&mut self) {
        match self.focus {
            Focus::Projects => {
                let total = Self::sidebar_nav_count() + self.projects.len();
                if total > 0 {
                    self.sidebar_cursor = (self.sidebar_cursor + 1).min(total.saturating_sub(1));
                }
            }
            Focus::Tasks => match self.workbench_view {
                WorkbenchView::MyTasks => {
                    let visible_count = if self.uses_github_my_tasks() {
                        self.visible_my_task_count()
                    } else {
                        self.visible_task_count()
                    };
                    if visible_count > 0 {
                        self.task_index =
                            (self.task_index + 1).min(visible_count.saturating_sub(1));
                    }
                }
                WorkbenchView::Reviews => {
                    let review_count = self.review_items().len();
                    if review_count > 0 {
                        self.review_index =
                            (self.review_index + 1).min(review_count.saturating_sub(1));
                        self.refresh_review_selected_context();
                    }
                }
                WorkbenchView::Threads => {
                    if !self.threads.is_empty() {
                        self.thread_index =
                            (self.thread_index + 1).min(self.threads.len().saturating_sub(1));
                    }
                }
                WorkbenchView::Settings => {
                    self.settings_section_index = (self.settings_section_index + 1)
                        .min(SettingsSection::ALL.len().saturating_sub(1));
                    self.maybe_prefetch_github_installations();
                }
                _ => {
                    let visible_count = self.visible_tasks().len();
                    if visible_count > 0 {
                        self.task_index =
                            (self.task_index + 1).min(visible_count.saturating_sub(1));
                    }
                }
            },
            Focus::Inspector => {
                self.inspector_scroll = self.inspector_scroll.saturating_add(1);
            }
        }
    }

    fn move_up(&mut self) {
        match self.focus {
            Focus::Projects => {
                self.sidebar_cursor = self.sidebar_cursor.saturating_sub(1);
            }
            Focus::Tasks => match self.workbench_view {
                WorkbenchView::Reviews => {
                    self.review_index = self.review_index.saturating_sub(1);
                    self.refresh_review_selected_context();
                }
                WorkbenchView::Threads => {
                    self.thread_index = self.thread_index.saturating_sub(1);
                }
                WorkbenchView::Settings => {
                    self.settings_section_index = self.settings_section_index.saturating_sub(1);
                    self.maybe_prefetch_github_installations();
                }
                _ => {
                    self.task_index = self.task_index.saturating_sub(1);
                }
            },
            Focus::Inspector => {
                self.inspector_scroll = self.inspector_scroll.saturating_sub(1);
            }
        }
    }

    pub(super) fn handle_palette_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Enter => {
                if let Some(&idx) = self.palette_filtered.get(self.palette_index) {
                    let Some(item) = self.palette_items.get(idx) else {
                        return Ok(());
                    };
                    let action = item.action;
                    self.input_buffer.clear();
                    self.input_mode = InputMode::Normal;
                    self.execute_palette_action(action)?;
                }
            }
            KeyCode::Up | KeyCode::Char('k') if self.input_buffer.is_empty() => {
                self.palette_index = self.palette_index.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if self.input_buffer.is_empty() => {
                if self.palette_filtered.len() > 1 {
                    self.palette_index =
                        (self.palette_index + 1).min(self.palette_filtered.len().saturating_sub(1));
                }
            }
            _ => {
                if apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                ) {
                    self.filter_palette();
                    self.palette_index = self
                        .palette_index
                        .min(self.palette_filtered.len().saturating_sub(1));
                }
            }
        }
        Ok(())
    }

    pub(super) fn filter_palette(&mut self) {
        let query = self.input_buffer.to_lowercase();
        if query.is_empty() {
            self.palette_filtered = (0..self.palette_items.len()).collect();
        } else {
            self.palette_filtered = self
                .palette_items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.label.to_lowercase().contains(&query))
                .map(|(i, _)| i)
                .collect();
        }
    }

    fn execute_palette_action(&mut self, action: PaletteAction) -> Result<()> {
        match action {
            PaletteAction::NewTask => {
                if self.selected_project().is_some() {
                    self.reset_task_form();
                    self.input_mode = InputMode::NewTask;
                }
            }
            PaletteAction::AddProject => {
                self.input_mode = InputMode::NewProject;
                self.input_buffer.clear();
                self.new_project_name.clear();
                self.new_project_path = String::from(".");
                self.new_project_git_linked = true;
                self.new_project_field = 0;
                self.clear_path_autocomplete();
            }
            PaletteAction::RemoveProject => {
                if let Some((name, id)) = self
                    .selected_project()
                    .map(|p| (p.name.clone(), p.id.clone()))
                {
                    self.confirm_target = name;
                    self.confirm_entity_id = id;
                    self.confirm_delete_kind = DeleteTarget::Project;
                    self.input_mode = InputMode::ConfirmDelete;
                }
            }
            PaletteAction::FocusProjects => self.focus = Focus::Projects,
            PaletteAction::FocusTasks => self.focus = Focus::Tasks,
            PaletteAction::FindSkills => {
                self.refresh_skills();
                self.input_mode = InputMode::SkillSearch;
                self.input_buffer.clear();
                self.search_results.clear();
            }
            PaletteAction::UpdateSkills => {
                self.skill_status_message = "Updating skills...".to_string();
                match crate::skills::update_skills() {
                    Ok(msg) => {
                        self.skill_status_message = msg;
                        self.refresh_skills();
                    }
                    Err(e) => {
                        self.skill_status_message = format!("Update failed: {e}");
                    }
                }
            }
            PaletteAction::Quit => self.should_quit = true,
            PaletteAction::Configure => {
                self.cached_config_status =
                    Some(crate::configure::load_config_status().map_err(|e| e.to_string()));
                self.input_mode = InputMode::ConfigureWizard;
            }
            PaletteAction::SprintBoard => {
                if let Some(project) = self.selected_project() {
                    if project.is_git_linked {
                        self.board_column_index = 0;
                        self.board_issue_index = 0;
                        self.board_first_load = true;
                        self.board_filter.clear();
                        self.board_filter_cursor = 0;
                        self.load_board_issues_from_cache();
                        self.input_mode = InputMode::BoardView;
                    } else {
                        self.show_toast("Project not linked to git", ToastStyle::Info);
                    }
                }
            }
        }
        Ok(())
    }

    /// Handle keys in the `ConfigureWizard` overlay.
    pub(super) fn handle_configure_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.cached_config_status = None;
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char('a') => {
                // Apply all recommendations
                match crate::configure::load_config_status() {
                    Ok(mut status) => {
                        match crate::configure::apply_all_recommendations(&mut status) {
                            Ok(0) => {
                                self.show_toast("Permissions already aligned", ToastStyle::Info);
                            }
                            Ok(n) => {
                                self.config_warning = None;
                                self.show_toast(
                                    format!("Applied {n} permission(s) to ~/.claude/settings.json"),
                                    ToastStyle::Success,
                                );
                            }
                            Err(e) => {
                                self.show_toast(format!("Failed to apply: {e}"), ToastStyle::Error);
                            }
                        }
                    }
                    Err(e) => {
                        self.show_toast(format!("Failed to load settings: {e}"), ToastStyle::Error);
                    }
                }
                self.cached_config_status = None;
                self.input_mode = InputMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn handle_board_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        // Clear close confirmation on any key except x
        if !matches!(code, KeyCode::Char('x')) {
            self.board_confirm_close = false;
        }
        if modifiers.is_empty() && self.handle_workbench_prefix(code)? {
            return Ok(());
        }
        if let Some(action) = self.keymap.lookup_normal(code, modifiers)
            && matches!(
                action,
                super::super::keymap::Action::ResizePaneNarrow
                    | super::super::keymap::Action::ResizePaneWide
            )
        {
            self.execute_action(action)?;
            return Ok(());
        }

        if self.focus != Focus::Tasks {
            match (code, modifiers) {
                (KeyCode::Char('j') | KeyCode::Down, KeyModifiers::NONE) => {
                    self.move_down();
                    return Ok(());
                }
                (KeyCode::Char('k') | KeyCode::Up, KeyModifiers::NONE) => {
                    self.move_up();
                    return Ok(());
                }
                (KeyCode::Enter, KeyModifiers::NONE) => {
                    self.execute_action(super::super::keymap::Action::Select)?;
                    return Ok(());
                }
                _ => {}
            }
        }

        match (code, modifiers) {
            (KeyCode::Char('1'), KeyModifiers::NONE) => {
                self.focus = Focus::Projects;
                return Ok(());
            }
            (KeyCode::Char('2'), KeyModifiers::NONE) => {
                self.focus = Focus::Tasks;
                return Ok(());
            }
            (KeyCode::Char('?'), KeyModifiers::NONE) => {
                self.input_mode = InputMode::HelpOverlay;
                return Ok(());
            }
            (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                self.next_tab();
                return Ok(());
            }
            (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                self.prev_tab();
                return Ok(());
            }
            _ => {}
        }

        match code {
            KeyCode::Esc | KeyCode::Char('b') => {
                self.workbench_view = WorkbenchView::MyTasks;
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char('v') | KeyCode::Enter => {
                if self.selected_board_issue().is_some() {
                    self.task_details_scroll = 0;
                    self.input_mode = InputMode::BoardIssueDrawer;
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                // Skip empty columns when navigating left
                let mut target = self.board_column_index;
                while target > 0 {
                    target -= 1;
                    if self
                        .board_issues
                        .get(target)
                        .is_some_and(|issues| !issues.is_empty())
                    {
                        break;
                    }
                }
                if target != self.board_column_index {
                    self.board_column_index = target;
                    self.board_issue_index = 0;
                    self.sync_board_scroll_offset();
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                // Skip empty columns when navigating right
                let col_count = self.board_columns.len();
                let mut target = self.board_column_index;
                while target + 1 < col_count {
                    target += 1;
                    if self
                        .board_issues
                        .get(target)
                        .is_some_and(|issues| !issues.is_empty())
                    {
                        break;
                    }
                }
                if target != self.board_column_index
                    && self
                        .board_issues
                        .get(target)
                        .is_some_and(|issues| !issues.is_empty())
                {
                    self.board_column_index = target;
                    self.board_issue_index = 0;
                    self.sync_board_scroll_offset();
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(col_issues) = self.board_issues.get(self.board_column_index)
                    && self.board_issue_index + 1 < col_issues.len()
                {
                    self.board_issue_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.board_issue_index = self.board_issue_index.saturating_sub(1);
            }
            // Page down in current column
            KeyCode::Char('d') if modifiers == KeyModifiers::CONTROL => {
                if let Some(col_issues) = self.board_issues.get(self.board_column_index) {
                    self.board_issue_index =
                        (self.board_issue_index + 10).min(col_issues.len().saturating_sub(1));
                }
            }
            // Page up in current column
            KeyCode::Char('u') if modifiers == KeyModifiers::CONTROL => {
                self.board_issue_index = self.board_issue_index.saturating_sub(10);
            }
            // Jump to bottom of column
            KeyCode::Char('G') => {
                if let Some(col_issues) = self.board_issues.get(self.board_column_index) {
                    self.board_issue_index = col_issues.len().saturating_sub(1);
                }
            }
            KeyCode::Char('o') => {
                self.open_selected_issue();
            }
            KeyCode::Char('L') => {
                if let Some(issue) = self.selected_board_issue() {
                    if let Some(thread) = self
                        .store
                        .find_thread_for_github_item(&issue.github_item_id)?
                    {
                        self.focus_thread_workspace(&thread.id);
                        self.show_toast("Focused existing thread", ToastStyle::Info);
                    } else {
                        self.open_board_issue_launch_modal()?;
                    }
                }
            }
            KeyCode::Char('/') => {
                self.board_filter.clear();
                self.board_filter_cursor = 0;
                self.input_mode = InputMode::BoardFilter;
            }
            KeyCode::Char('m') => {
                self.board_sprint_index = 0;
                self.input_mode = InputMode::MilestoneFilter;
            }
            KeyCode::Char('p') if modifiers == KeyModifiers::CONTROL => {
                self.input_mode = InputMode::CommandPalette;
                self.input_buffer.clear();
                self.palette_index = 0;
                self.filter_palette();
            }
            KeyCode::Char('P' | 'p') => {
                self.open_project_picker()?;
            }
            KeyCode::Char('g') => {
                self.open_github_project_picker()?;
            }
            KeyCode::Char('t') => {
                self.board_scope = self.board_scope.next();
                self.board_issue_index = 0;
                self.apply_board_filter();
            }
            KeyCode::Char('R') => {
                self.refresh_board_issues();
            }
            KeyCode::Char('c') => {
                self.open_comment_compose();
            }
            KeyCode::Char('x') => {
                self.toggle_board_issue_state();
            }
            KeyCode::Char('n') => {
                self.open_github_issue_create();
            }
            KeyCode::Char('e') => {
                self.open_github_issue_edit();
            }
            KeyCode::Char('q') => {
                self.should_quit = true;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn handle_board_sprint_picker_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Esc => {
                self.input_mode = InputMode::BoardView;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.board_sprint_index < self.board_sprints.len() {
                    self.board_sprint_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.board_sprint_index = self.board_sprint_index.saturating_sub(1);
            }
            KeyCode::Enter => {
                self.board_sprint_filter = if self.board_sprint_index == 0 {
                    None
                } else {
                    self.board_sprints.get(self.board_sprint_index - 1).cloned()
                };
                self.input_mode = InputMode::BoardView;
                self.apply_board_filter();
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn handle_board_filter_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Enter | KeyCode::Esc => {
                self.input_mode = InputMode::BoardView;
            }
            _ => {
                if crate::tui::form::apply_text_edit(
                    &mut self.board_filter,
                    &mut self.board_filter_cursor,
                    code,
                    modifiers,
                ) {
                    self.apply_board_filter();
                }
            }
        }
        Ok(())
    }

    pub(crate) fn selected_board_issue(&self) -> Option<crate::github::GitHubBoardItem> {
        self.board_issues
            .get(self.board_column_index)
            .and_then(|column| column.get(self.board_issue_index))
            .cloned()
    }

    fn open_selected_issue(&mut self) {
        if let Some(issue) = self.selected_board_issue() {
            let url = issue.url.clone();
            let _ = std::process::Command::new("open")
                .arg(&url)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            self.show_toast(
                format!("Opening #{} in browser", issue.number),
                ToastStyle::Info,
            );
        }
    }

    pub(super) fn load_board_issues_from_cache(&mut self) {
        let Some(project) = self.selected_project().cloned() else {
            return;
        };

        if !project.is_git_linked {
            self.board_error = Some("Project not linked to git".to_string());
            return;
        }

        let selected_project_hint = self.config.github_app.default_project_id.clone();

        match crate::github::load_project_board_from_cache(
            &self.store,
            &project,
            selected_project_hint.as_deref(),
        ) {
            Ok(snapshot) => {
                self.apply_board_snapshot(snapshot, selected_project_hint.as_deref());
            }
            Err(error) => {
                self.clear_board_snapshot(Some(error.to_string()));
            }
        }
    }

    pub(super) fn refresh_board_issues(&mut self) {
        let Some(project) = self.selected_project() else {
            return;
        };
        if !project.is_git_linked {
            self.board_error = Some("Project not linked to git".to_string());
            return;
        }
        self.spawn_github_sync();
        self.show_toast("Syncing GitHub data...", ToastStyle::Info);
    }

    pub(super) fn apply_board_snapshot(
        &mut self,
        snapshot: crate::github::GitHubProjectBoardSnapshot,
        selected_project_hint: Option<&str>,
    ) {
        self.github_projects_v2 = snapshot.projects;
        if let Some(selected_project) = snapshot.selected_project {
            let selected_project_is_default = selected_project_hint.is_some_and(|hint| {
                selected_project.id == hint
                    || selected_project.project_number.to_string() == hint
                    || selected_project.title.eq_ignore_ascii_case(hint)
            });
            let selected_project_label = selected_project.display_label();
            self.board_project_title = Some(if selected_project_is_default {
                selected_project_label
            } else {
                format!("{selected_project_label} [auto]")
            });
            if let Some(index) = self
                .github_projects_v2
                .iter()
                .position(|project| project.project_number == selected_project.project_number)
            {
                self.github_project_index = index;
            }
            self.board_source_items = snapshot.items;
            self.auto_discover_board_columns();
            self.board_sprints = self.collect_board_sprints();
            if self.board_first_load {
                self.board_first_load = false;
                self.board_scope = if self.current_github_login().is_some() {
                    BoardScope::AssignedToMe
                } else {
                    BoardScope::AllSprint
                };
                self.board_sprint_filter = self.default_board_sprint();
            }
            if let Some(selected_sprint) = self.board_sprint_filter.as_deref()
                && !self
                    .board_sprints
                    .iter()
                    .any(|sprint| sprint == selected_sprint)
            {
                self.board_sprint_filter = None;
            }
            self.board_sprint_index = self
                .board_sprint_filter
                .as_deref()
                .and_then(|selected_sprint| {
                    self.board_sprints
                        .iter()
                        .position(|sprint| sprint == selected_sprint)
                        .map(|index| index + 1)
                })
                .unwrap_or(0);
            self.board_error = None;
            self.apply_board_filter();
        } else {
            self.clear_board_snapshot(Some(
                "No cached GitHub Projects v2 found. Press R to sync the board first.".to_string(),
            ));
        }
    }

    fn clear_board_snapshot(&mut self, error: Option<String>) {
        self.board_project_title = None;
        self.board_source_items.clear();
        self.board_sprints.clear();
        self.board_sprint_filter = None;
        self.board_sprint_index = 0;
        self.board_error = error;
        self.apply_board_filter();
    }

    fn apply_board_filter(&mut self) {
        let col_count = self.board_columns.len();
        let mut grouped: Vec<Vec<crate::github::GitHubBoardItem>> =
            (0..col_count).map(|_| Vec::new()).collect();
        let current_login = self.current_github_login();

        for item in self.board_source_items.iter().filter(|item| {
            let sprint_matches = self
                .board_sprint_filter
                .as_deref()
                .is_none_or(|sprint| Self::board_item_sprint(item).as_deref() == Some(sprint));
            let scope_matches = match (self.board_scope, current_login.as_deref()) {
                (BoardScope::AllSprint, _) => true,
                (BoardScope::AssignedToMe, Some(login)) => item
                    .assignees
                    .iter()
                    .any(|assignee| assignee.login.eq_ignore_ascii_case(login)),
                (BoardScope::AssignedToMe, None) => false,
            };
            sprint_matches && scope_matches
        }) {
            let column = self.board_item_column(item);
            if column < col_count {
                grouped[column].push(item.clone());
            }
        }

        self.board_all_issues = grouped;

        if self.board_filter.is_empty() {
            self.board_issues = self.board_all_issues.clone();
        } else {
            let query = self.board_filter.to_lowercase();
            self.board_issues =
                self.board_all_issues
                    .iter()
                    .map(|column| {
                        column
                            .iter()
                            .filter(|issue| {
                                issue.title.to_lowercase().contains(&query)
                                    || issue.number.to_string().contains(&query)
                                    || issue
                                        .labels
                                        .iter()
                                        .any(|label| label.name.to_lowercase().contains(&query))
                                    || issue.assignees.iter().any(|assignee| {
                                        assignee.login.to_lowercase().contains(&query)
                                    })
                                    || issue.kind.as_str().contains(&query)
                            })
                            .cloned()
                            .collect()
                    })
                    .collect();
        }

        // Clamp selection
        let visible_col_count = self.board_issues.len();
        self.board_column_index = self
            .board_column_index
            .min(visible_col_count.saturating_sub(1));
        if let Some(col_issues) = self.board_issues.get(self.board_column_index) {
            self.board_issue_index = self
                .board_issue_index
                .min(col_issues.len().saturating_sub(1));
        } else {
            self.board_issue_index = 0;
        }
    }

    /// Auto-discover board columns from the Status field values found in items.
    ///
    /// If items have a "Status" project field, the unique values are extracted
    /// and used as columns (preserving insertion order, with "Done" pushed to
    /// the end).  Falls back to config columns when no Status values are found.
    fn auto_discover_board_columns(&mut self) {
        use std::collections::BTreeSet;

        let mut seen = BTreeSet::new();
        let mut order = Vec::new();

        for item in &self.board_source_items {
            if let Some(status) = item.project_field_values.as_object().and_then(|fields| {
                fields.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("status")
                        .then(|| value.as_str().map(str::to_string))
                        .flatten()
                })
            }) && !status.is_empty()
                && seen.insert(status.clone())
            {
                order.push(status);
            }
        }

        if order.is_empty() {
            // No Status field data — keep the config-based columns.
            return;
        }

        // Move "Done" / closed-like statuses to the end.
        let done_keywords = ["done", "closed", "merged", "complete", "completed"];
        let (done_cols, other_cols): (Vec<_>, Vec<_>) = order.into_iter().partition(|status| {
            done_keywords
                .iter()
                .any(|kw| status.eq_ignore_ascii_case(kw))
        });

        let mut final_columns = other_cols;
        final_columns.extend(done_cols);

        self.board_columns = final_columns;
    }

    fn board_item_column(&self, item: &crate::github::GitHubBoardItem) -> usize {
        // First, try matching on the item's Status project field value.
        let status_value = item
            .project_field_values
            .as_object()
            .and_then(|fields| {
                fields.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("status")
                        .then(|| value.as_str().map(str::to_string))
                        .flatten()
                })
            })
            .unwrap_or_default();

        if let Some(pos) = self
            .board_columns
            .iter()
            .position(|column| column.eq_ignore_ascii_case(&status_value))
        {
            return pos;
        }

        // Closed/merged items without a matching status go to the first
        // "done"-like column (not blindly the last column).
        if item.state.eq_ignore_ascii_case("closed") || item.state.eq_ignore_ascii_case("merged") {
            let done_keywords = ["done", "closed", "merged", "complete", "completed"];
            if let Some(pos) = self
                .board_columns
                .iter()
                .position(|col| done_keywords.iter().any(|kw| col.eq_ignore_ascii_case(kw)))
            {
                return pos;
            }
            // Absolute fallback: last column
            return self.board_columns.len().saturating_sub(1);
        }

        // Open items without a matching status go to the first column (backlog).
        0
    }

    /// Keep `board_scroll_offset` in sync so the selected column is visible.
    fn sync_board_scroll_offset(&mut self) {
        // Build the same non-empty column list used by the renderer.
        let col_count = self.board_columns.len();
        let non_empty: Vec<usize> = (0..col_count)
            .filter(|&idx| {
                self.board_issues
                    .get(idx)
                    .is_some_and(|issues| !issues.is_empty())
                    || idx == self.board_column_index
            })
            .collect();

        // Find the position of the selected column within the non-empty list.
        let Some(pos) = non_empty
            .iter()
            .position(|&idx| idx == self.board_column_index)
        else {
            return;
        };

        // Determine how many columns fit on screen (approximate — use 26 min width).
        let term_width = self.last_terminal_area.width;
        let max_visible = ((term_width / 26) as usize).max(1).min(non_empty.len());

        if pos < self.board_scroll_offset {
            self.board_scroll_offset = pos;
        } else if pos >= self.board_scroll_offset + max_visible {
            self.board_scroll_offset = pos.saturating_sub(max_visible - 1);
        }
    }

    fn board_item_sprint(item: &crate::github::GitHubBoardItem) -> Option<String> {
        item.project_field_values.as_object().and_then(|fields| {
            fields.iter().find_map(|(name, value)| {
                (name.eq_ignore_ascii_case("iteration")
                    || name.eq_ignore_ascii_case("sprint")
                    || name.eq_ignore_ascii_case("milestone"))
                .then(|| value.as_str().map(str::to_string))
                .flatten()
            })
        })
    }

    fn collect_board_sprints(&self) -> Vec<String> {
        let mut values = self
            .board_source_items
            .iter()
            .filter_map(Self::board_item_sprint)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        values.sort();
        values
    }

    fn default_board_sprint(&self) -> Option<String> {
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        for sprint in self
            .board_source_items
            .iter()
            .filter_map(Self::board_item_sprint)
        {
            *counts.entry(sprint).or_default() += 1;
        }
        counts
            .into_iter()
            .max_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)))
            .map(|(sprint, _)| sprint)
    }

    pub fn refresh_skills(&mut self) {
        let mut all_skills = crate::skills::list_skills(true, None).unwrap_or_default();

        if let Some(project) = self.selected_project() {
            let project_skills =
                crate::skills::list_skills(false, Some(&project.repo_path)).unwrap_or_default();
            all_skills.extend(project_skills);
        }

        self.installed_skills = all_skills;

        if self.skill_index >= self.installed_skills.len() && !self.installed_skills.is_empty() {
            self.skill_index = self.installed_skills.len() - 1;
        }

        self.refresh_skill_detail();
    }

    fn refresh_skill_detail(&mut self) {
        if let Some(skill) = self.installed_skills.get(self.skill_index) {
            self.skill_detail_content = crate::skills::read_skill_md(&skill.path)
                .unwrap_or_else(|_| "Could not read SKILL.md".to_string());
        } else {
            self.skill_detail_content.clear();
        }
    }

    pub(super) fn handle_skill_panel_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.installed_skills.is_empty() {
                    self.skill_index =
                        (self.skill_index + 1).min(self.installed_skills.len().saturating_sub(1));
                    self.refresh_skill_detail();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.skill_index = self.skill_index.saturating_sub(1);
                self.refresh_skill_detail();
            }
            KeyCode::Char('f') => {
                self.input_mode = InputMode::SkillSearch;
                self.input_buffer.clear();
                self.search_results.clear();
                self.selected_search_indices.clear();
                self.skill_index = 0;
            }
            KeyCode::Char('a') => {
                self.input_mode = InputMode::SkillAdd;
                self.input_buffer.clear();
            }
            KeyCode::Char('x') => {
                if let Some(skill) = self.installed_skills.get(self.skill_index) {
                    let name = skill.name.clone();
                    let global = skill.scope == crate::skills::SkillScope::Global;
                    let project_path =
                        if let crate::skills::SkillScope::Project(ref p) = skill.scope {
                            Some(p.clone())
                        } else {
                            None
                        };
                    match crate::skills::remove_skill(&name, global, project_path.as_deref()) {
                        Ok(_) => {
                            self.show_toast(format!("Removed {name}"), ToastStyle::Success);
                            self.refresh_skills();
                        }
                        Err(e) => {
                            self.show_toast(format!("Remove failed: {e}"), ToastStyle::Error);
                        }
                    }
                }
            }
            KeyCode::Char('u') => {
                self.show_toast("Updating skills...", ToastStyle::Info);
                match crate::skills::update_skills() {
                    Ok(msg) => {
                        self.show_toast(msg, ToastStyle::Success);
                        self.refresh_skills();
                    }
                    Err(e) => {
                        self.show_toast(format!("Update failed: {e}"), ToastStyle::Error);
                    }
                }
            }
            KeyCode::Char('g') => {
                self.skill_scope_global = !self.skill_scope_global;
                self.refresh_skills();
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn handle_skill_search_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Enter => {
                if !self.input_buffer.is_empty() {
                    if self.search_results.is_empty() {
                        let query = self.input_buffer.clone();
                        self.skill_status_message = format!("Searching for '{query}'...");
                        match crate::skills::find_skills(&query) {
                            Ok(results) => {
                                self.skill_status_message =
                                    format!("Found {} results", results.len());
                                self.search_results = results;
                                self.skill_index = 0;
                                self.selected_search_indices.clear();
                            }
                            Err(e) => {
                                self.skill_status_message = format!("Search failed: {e}");
                            }
                        }
                    } else {
                        // Install selected skills (or just the cursor item if none toggled)
                        let indices_to_install: Vec<usize> =
                            if self.selected_search_indices.is_empty() {
                                vec![self.skill_index]
                            } else {
                                let mut v: Vec<usize> =
                                    self.selected_search_indices.iter().copied().collect();
                                v.sort_unstable();
                                v
                            };

                        let packages: Vec<String> = indices_to_install
                            .iter()
                            .filter_map(|&i| self.search_results.get(i).map(|r| r.package.clone()))
                            .collect();

                        if packages.is_empty() {
                            return Ok(());
                        }

                        let global = self.skill_scope_global;
                        let project_path = if global {
                            None
                        } else {
                            self.selected_project().map(|p| p.repo_path.clone())
                        };

                        let total = packages.len();
                        let mut installed = Vec::new();
                        let mut failed = Vec::new();

                        for (i, package) in packages.iter().enumerate() {
                            self.skill_status_message =
                                format!("Installing {package} ({}/{})", i + 1, total);
                            match crate::skills::add_skill(package, global, project_path.as_deref())
                            {
                                Ok(_) => installed.push(package.clone()),
                                Err(e) => failed.push(format!("{package}: {e}")),
                            }
                        }

                        if failed.is_empty() {
                            self.skill_status_message = if installed.len() == 1 {
                                format!("Installed {}", installed[0])
                            } else {
                                format!("Installed {} skills", installed.len())
                            };
                        } else if installed.is_empty() {
                            self.skill_status_message =
                                format!("Install failed: {}", failed.join("; "));
                        } else {
                            self.skill_status_message = format!(
                                "Installed {}; failed: {}",
                                installed.len(),
                                failed.join("; ")
                            );
                        }

                        self.input_mode = InputMode::SkillPanel;
                        self.input_buffer.clear();
                        self.search_results.clear();
                        self.selected_search_indices.clear();
                        self.refresh_skills();
                    }
                }
            }
            KeyCode::Char(' ') if !self.search_results.is_empty() => {
                // Toggle selection on the current item
                if self.selected_search_indices.contains(&self.skill_index) {
                    self.selected_search_indices.remove(&self.skill_index);
                } else {
                    self.selected_search_indices.insert(self.skill_index);
                }
                // Advance cursor to the next item for convenient multi-select
                if self.skill_index + 1 < self.search_results.len() {
                    self.skill_index += 1;
                }
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.search_results.clear();
                self.selected_search_indices.clear();
                self.input_mode = InputMode::SkillPanel;
                self.skill_status_message.clear();
            }
            KeyCode::Char('j') | KeyCode::Down if !self.search_results.is_empty() => {
                self.skill_index =
                    (self.skill_index + 1).min(self.search_results.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up if !self.search_results.is_empty() => {
                self.skill_index = self.skill_index.saturating_sub(1);
            }
            _ => {
                if apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                ) {
                    self.search_results.clear();
                    self.skill_index = 0;
                    self.selected_search_indices.clear();
                    self.skill_status_message.clear();
                }
            }
        }
        Ok(())
    }

    pub(super) fn handle_edit_task_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        if self.handle_task_form_shared_key(code, modifiers) {
            return Ok(());
        }
        match code {
            KeyCode::Enter => {
                self.save_current_task_field();
                let is_exploration = self.new_task_mode == crate::store::TaskMode::Exploration;
                if !self.new_task_description.is_empty() || is_exploration {
                    if let Some(ref task_id) = self.editing_task_id.clone() {
                        let fallback = if is_exploration && self.new_task_description.is_empty() {
                            "Exploration session".to_string()
                        } else {
                            fallback_title(&self.new_task_description)
                        };
                        let branch = if self.new_task_branch.is_empty() {
                            None
                        } else {
                            Some(self.new_task_branch.as_str())
                        };
                        let base = if self.new_task_base.is_empty() {
                            None
                        } else {
                            Some(self.new_task_base.as_str())
                        };
                        self.store.update_task(
                            task_id,
                            &fallback,
                            &self.new_task_description,
                            self.new_task_mode,
                            branch,
                            base,
                            self.new_task_push_mode,
                            self.new_task_review_loop,
                        )?;

                        // Promote draft -> pending on submit
                        if let Ok(task) = self.store.get_task(task_id)
                            && task.status == crate::store::TaskStatus::Draft
                        {
                            self.store
                                .update_task_status(task_id, crate::store::TaskStatus::Pending)?;
                        }

                        // Create inline subtasks added during edit
                        for subtask_desc in &self.new_task_subtasks {
                            let st_title = fallback_title(subtask_desc);
                            self.store
                                .create_subtask(task_id, &st_title, subtask_desc)?;
                        }

                        // Launch autonomous and exploration tasks immediately,
                        // or just generate the title for supervised tasks.
                        if matches!(
                            self.new_task_mode,
                            crate::store::TaskMode::Autonomous
                                | crate::store::TaskMode::Exploration
                        ) && let Some(project_id) = self.selected_project().map(|p| p.id.clone())
                        {
                            self.launch_task(task_id.clone(), project_id)?;
                        } else {
                            self.spawn_title_generation(
                                task_id.clone(),
                                self.new_task_description.clone(),
                            );
                        }
                        self.show_toast("Task updated", ToastStyle::Success);
                    }
                    self.editing_task_id = None;
                    self.reset_task_form();
                    self.input_mode = InputMode::Normal;
                    self.refresh_data()?;
                    crate::sync::try_auto_push();
                }
            }
            KeyCode::Esc => {
                self.save_current_task_field();
                let is_exploration = self.new_task_mode == crate::store::TaskMode::Exploration;
                if (!self.new_task_description.is_empty() || is_exploration)
                    && let Some(ref task_id) = self.editing_task_id.clone()
                {
                    let fallback = if is_exploration && self.new_task_description.is_empty() {
                        "Exploration session".to_string()
                    } else {
                        fallback_title(&self.new_task_description)
                    };
                    let branch = if self.new_task_branch.is_empty() {
                        None
                    } else {
                        Some(self.new_task_branch.as_str())
                    };
                    let base = if self.new_task_base.is_empty() {
                        None
                    } else {
                        Some(self.new_task_base.as_str())
                    };
                    self.store.update_task(
                        task_id,
                        &fallback,
                        &self.new_task_description,
                        self.new_task_mode,
                        branch,
                        base,
                        self.new_task_push_mode,
                        self.new_task_review_loop,
                    )?;

                    // Create inline subtasks added during edit
                    for subtask_desc in &self.new_task_subtasks {
                        let st_title = fallback_title(subtask_desc);
                        self.store
                            .create_subtask(task_id, &st_title, subtask_desc)?;
                    }

                    self.spawn_title_generation(task_id.clone(), self.new_task_description.clone());
                    self.show_toast("Task draft saved", ToastStyle::Info);
                    crate::sync::try_auto_push();
                }
                self.editing_task_id = None;
                self.reset_task_form();
                self.input_mode = InputMode::Normal;
                self.refresh_data()?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn handle_task_filter_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Enter => {
                self.input_mode = InputMode::Normal;
                self.task_index = 0;
            }
            KeyCode::Esc => {
                self.task_filter.clear();
                self.recompute_visible_tasks();
                self.recompute_my_task_items()?;
                self.input_mode = InputMode::Normal;
                self.task_index = 0;
            }
            _ => {
                if apply_text_edit(
                    &mut self.task_filter,
                    &mut self.task_filter_cursor,
                    code,
                    modifiers,
                ) {
                    self.recompute_visible_tasks();
                    self.recompute_my_task_items()?;
                    self.task_index = 0;
                }
            }
        }
        Ok(())
    }

    pub(super) fn handle_subtask_panel_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Enter => {
                if !self.input_buffer.is_empty()
                    && let Some(task) = self.visible_tasks().get(self.task_index)
                {
                    let task_id = task.id.clone();
                    let desc = std::mem::take(&mut self.input_buffer);
                    let title = fallback_title(&desc);
                    self.store.create_subtask(&task_id, &title, &desc)?;
                    self.subtasks = self
                        .store
                        .list_subtasks_for_task(&task_id)
                        .unwrap_or_default();
                    self.show_toast("Subtask added", ToastStyle::Success);
                }
            }
            KeyCode::Char('d') if self.input_buffer.is_empty() => {
                if let Some(st) = self.subtasks.get(self.subtask_index) {
                    let st_id = st.id.clone();
                    let task_id = st.task_id.clone();
                    self.store.delete_subtask(&st_id)?;
                    self.subtasks = self
                        .store
                        .list_subtasks_for_task(&task_id)
                        .unwrap_or_default();
                    if self.subtask_index >= self.subtasks.len() {
                        self.subtask_index = self.subtasks.len().saturating_sub(1);
                    }
                    self.show_toast("Subtask deleted", ToastStyle::Success);
                }
            }
            KeyCode::Char('j') | KeyCode::Down if self.input_buffer.is_empty() => {
                if !self.subtasks.is_empty() {
                    self.subtask_index =
                        (self.subtask_index + 1).min(self.subtasks.len().saturating_sub(1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up if self.input_buffer.is_empty() => {
                self.subtask_index = self.subtask_index.saturating_sub(1);
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.input_cursor = 0;
                self.input_mode = InputMode::Normal;
                self.refresh_data()?;
            }
            _ => {
                apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                );
            }
        }
        Ok(())
    }

    pub(super) fn handle_skill_add_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Enter => {
                if !self.input_buffer.is_empty() {
                    let package = self.input_buffer.clone();
                    let global = self.skill_scope_global;
                    let project_path = if global {
                        None
                    } else {
                        self.selected_project().map(|p| p.repo_path.clone())
                    };

                    self.skill_status_message = format!("Installing {package}...");
                    match crate::skills::add_skill(&package, global, project_path.as_deref()) {
                        Ok(_) => {
                            self.skill_status_message = format!("Installed {package}");
                            self.input_mode = InputMode::SkillPanel;
                            self.input_buffer.clear();
                            self.refresh_skills();
                        }
                        Err(e) => {
                            self.skill_status_message = format!("Install failed: {e}");
                        }
                    }
                }
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.input_cursor = 0;
                self.input_mode = InputMode::SkillPanel;
            }
            _ => {
                apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                );
            }
        }
        Ok(())
    }

    // ── Comment compose ─────────────────────────────────────────────

    pub(super) fn handle_board_drawer_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        // Clear close confirmation on any key except x
        if !matches!(code, KeyCode::Char('x')) {
            self.board_confirm_close = false;
        }
        match code {
            KeyCode::Esc | KeyCode::Char('v' | 'q') => {
                self.input_mode = InputMode::BoardView;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.task_details_scroll = self.task_details_scroll.saturating_add(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.task_details_scroll = self.task_details_scroll.saturating_sub(1);
            }
            KeyCode::Char('c') if modifiers.is_empty() => {
                self.open_comment_compose();
            }
            KeyCode::Char('x') if modifiers.is_empty() => {
                self.toggle_board_issue_state();
            }
            KeyCode::Char('o') if modifiers.is_empty() => {
                self.open_selected_issue();
            }
            KeyCode::Char('s') if modifiers.is_empty() => {
                self.open_field_picker("status");
            }
            KeyCode::Char('L') => {
                // Launch thread from board drawer — close drawer, open launch modal
                self.input_mode = InputMode::BoardView;
                self.open_board_issue_launch_modal()?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn handle_review_drawer_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.review_drawer_scroll = self.review_drawer_scroll.saturating_add(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.review_drawer_scroll = self.review_drawer_scroll.saturating_sub(1);
            }
            KeyCode::Char('1') if modifiers.is_empty() => {
                self.review_drawer_tab = super::ReviewDrawerTab::Description;
                self.review_drawer_scroll = 0;
            }
            KeyCode::Char('2') if modifiers.is_empty() => {
                self.review_drawer_tab = super::ReviewDrawerTab::Diff;
                self.review_drawer_scroll = 0;
            }
            KeyCode::Char('3') if modifiers.is_empty() => {
                self.review_drawer_tab = super::ReviewDrawerTab::Comments;
                self.review_drawer_scroll = 0;
            }
            KeyCode::Char('l') if modifiers.is_empty() => {
                // Launch thread from drawer
                self.input_mode = InputMode::Normal;
                self.open_review_launch_modal()?;
            }
            KeyCode::Char('o') if modifiers.is_empty() => {
                if let Some(item) = self.selected_review_item() {
                    let _ = std::process::Command::new("open")
                        .arg(&item.github_item.url)
                        .spawn();
                }
            }
            KeyCode::Char('c') if modifiers.is_empty() => {
                // Open comment compose from review drawer
                self.open_review_comment_compose();
            }
            _ => {}
        }
        Ok(())
    }

    fn open_review_comment_compose(&mut self) {
        let Some(item) = self.selected_review_item() else {
            return;
        };
        let repo = self.store.get_github_repo(&item.github_item.repo_id).ok();
        let Some(repo) = repo else {
            self.show_toast("No repo found for this PR", super::ToastStyle::Error);
            return;
        };
        let repo_path = repo.full_name;
        let number = item.github_item.number;
        let github_item_id = item.github_item.id.clone();
        self.comment_compose_buffer.clear();
        self.comment_compose_cursor = 0;
        self.comment_compose_target = Some((repo_path, number, github_item_id));
        self.comment_compose_return_mode = InputMode::ReviewDrawer;
        self.input_mode = InputMode::CommentCompose;
    }

    pub(super) fn handle_my_task_drawer_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Esc | KeyCode::Char('v' | 'q') => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.task_details_scroll = self.task_details_scroll.saturating_add(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.task_details_scroll = self.task_details_scroll.saturating_sub(1);
            }
            KeyCode::Char('c') if modifiers.is_empty() => {
                self.open_my_task_comment_compose();
            }
            KeyCode::Char('o') if modifiers.is_empty() => {
                if let Some(item) = self.selected_my_task_item() {
                    let url = item.github_item.url.clone();
                    let number = item.github_item.number;
                    let _ = std::process::Command::new("open")
                        .arg(&url)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                    self.show_toast(format!("Opening #{number} in browser"), ToastStyle::Info);
                }
            }
            KeyCode::Char('t') if modifiers.is_empty() => {
                // Focus or launch thread for this item
                if let Some(thread) = self.selected_thread().cloned() {
                    self.input_mode = InputMode::Normal;
                    self.focus_thread_workspace(&thread.id);
                    self.show_toast("Focused thread", ToastStyle::Info);
                }
            }
            KeyCode::Char('l' | 'L') if modifiers.is_empty() => {
                // Launch or resume: if an existing thread has a live session tab, switch to it;
                // otherwise open the launch modal.
                if let Some(item) = self.selected_my_task_item().cloned() {
                    let switched = item
                        .linked_thread
                        .as_ref()
                        .and_then(|t| t.session_id.as_deref())
                        .is_some_and(|sid| self.goto_session_tab(sid));
                    self.input_mode = InputMode::Normal;
                    if switched {
                        self.show_toast("Switched to live session", ToastStyle::Info);
                    } else {
                        self.open_my_task_launch_modal()?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn open_my_task_comment_compose(&mut self) {
        let Some(item) = self.selected_my_task_item() else {
            return;
        };
        let repo_path = item.repo.full_name.clone();
        let number = item.github_item.number;
        let github_item_id = item.github_item.id.clone();
        self.comment_compose_buffer.clear();
        self.comment_compose_cursor = 0;
        self.comment_compose_target = Some((repo_path, number, github_item_id));
        self.comment_compose_return_mode = InputMode::MyTaskDrawer;
        self.input_mode = InputMode::CommentCompose;
    }

    fn open_field_picker(&mut self, field_name: &str) {
        let Some(issue) = self.selected_board_issue() else {
            return;
        };
        // Find the project's node_id
        let project_node_id = self
            .github_projects_v2
            .get(self.github_project_index)
            .and_then(|p| p.node_id.clone());
        let Some(project_node_id) = project_node_id else {
            self.show_toast(
                "No project node ID cached. Press R on the board to sync first.".to_string(),
                ToastStyle::Error,
            );
            return;
        };
        let item_node_id = self
            .store
            .get_github_item(&issue.github_item_id)
            .ok()
            .and_then(|item| item.node_id);
        let Some(item_node_id) = item_node_id else {
            self.show_toast("No item node ID cached".to_string(), ToastStyle::Error);
            return;
        };

        // Fetch field definitions (blocking for now — fast API call)
        match crate::github::fetch_project_v2_fields(&project_node_id) {
            Ok(fields) => {
                let target_field = fields
                    .iter()
                    .find(|f| f.name.eq_ignore_ascii_case(field_name));
                let Some(field) = target_field else {
                    self.show_toast(
                        format!("No '{field_name}' field found on this project"),
                        ToastStyle::Error,
                    );
                    return;
                };
                if field.options.is_empty() {
                    self.show_toast(
                        format!("Field '{field_name}' has no selectable options"),
                        ToastStyle::Error,
                    );
                    return;
                }
                self.field_picker_options.clone_from(&field.options);
                self.field_picker_index = 0;
                self.field_picker_field_name = Some(field.name.clone());
                self.field_picker_field_id = Some(field.id.clone());
                self.field_picker_project_node_id = Some(project_node_id);
                self.field_picker_item_node_id = Some(item_node_id);
                self.input_mode = InputMode::FieldPicker;
            }
            Err(err) => {
                self.show_toast(format!("Failed to fetch fields: {err}"), ToastStyle::Error);
            }
        }
    }

    pub(super) fn handle_field_picker_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Esc => {
                self.input_mode = InputMode::BoardIssueDrawer;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.field_picker_index + 1 < self.field_picker_options.len() {
                    self.field_picker_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.field_picker_index = self.field_picker_index.saturating_sub(1);
            }
            KeyCode::Enter => {
                self.submit_field_picker_selection();
            }
            _ => {}
        }
        Ok(())
    }

    fn submit_field_picker_selection(&mut self) {
        let Some(option) = self
            .field_picker_options
            .get(self.field_picker_index)
            .cloned()
        else {
            return;
        };
        let Some(project_node_id) = self.field_picker_project_node_id.clone() else {
            return;
        };
        let Some(item_node_id) = self.field_picker_item_node_id.clone() else {
            return;
        };
        let Some(field_id) = self.field_picker_field_id.clone() else {
            return;
        };
        let field_name = self.field_picker_field_name.clone().unwrap_or_default();
        let option_name = option.name.clone();
        let value = serde_json::json!({ "singleSelectOptionId": option.id });
        let tx = self.github_mutation_tx.clone();

        self.input_mode = InputMode::BoardIssueDrawer;
        self.show_toast(
            format!("Setting {field_name} to {option_name}..."),
            ToastStyle::Info,
        );

        std::thread::spawn(move || {
            let result = crate::github::update_project_v2_item_field(
                &project_node_id,
                &item_node_id,
                &field_id,
                &value,
            );
            let msg = match result {
                Ok(()) => super::GitHubMutationResult::FieldUpdated {
                    message: format!("{field_name} \u{2192} {option_name}"),
                },
                Err(err) => super::GitHubMutationResult::Error {
                    message: format!("Failed to update field: {err}"),
                },
            };
            let _ = tx.send(msg);
        });
    }

    fn open_comment_compose(&mut self) {
        let Some(issue) = self.selected_board_issue() else {
            return;
        };
        let repo_path = self.selected_project().map(|p| p.repo_path.clone());
        let Some(repo_path) = repo_path else {
            return;
        };
        self.comment_compose_buffer.clear();
        self.comment_compose_cursor = 0;
        self.comment_compose_target = Some((repo_path, issue.number, issue.github_item_id));
        self.comment_compose_return_mode = InputMode::BoardIssueDrawer;
        self.input_mode = InputMode::CommentCompose;
    }

    pub(super) fn handle_comment_compose_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Esc => {
                self.input_mode = self.comment_compose_return_mode;
                self.comment_compose_target = None;
            }
            KeyCode::Enter if modifiers.is_empty() => {
                self.submit_comment();
            }
            _ => {
                apply_text_edit(
                    &mut self.comment_compose_buffer,
                    &mut self.comment_compose_cursor,
                    code,
                    modifiers,
                );
            }
        }
        Ok(())
    }

    fn submit_comment(&mut self) {
        let body = self.comment_compose_buffer.trim().to_string();
        if body.is_empty() {
            return;
        }
        let Some((repo_path, number, _item_id)) = self.comment_compose_target.clone() else {
            return;
        };
        let tx = self.github_mutation_tx.clone();
        std::thread::spawn(move || {
            let result = crate::github::create_comment(&repo_path, number, &body);
            let msg = match result {
                Ok(_) => super::GitHubMutationResult::CommentCreated {
                    message: format!("Comment posted on #{number}"),
                },
                Err(err) => super::GitHubMutationResult::Error {
                    message: format!("Failed to post comment: {err}"),
                },
            };
            let _ = tx.send(msg);
        });
        self.input_mode = self.comment_compose_return_mode;
        self.comment_compose_target = None;
        self.comment_compose_buffer.clear();
        self.comment_compose_cursor = 0;
        self.show_toast("Posting comment...".to_string(), ToastStyle::Info);
    }

    // ── Close/reopen issue ──────────────────────────────────────────

    fn toggle_board_issue_state(&mut self) {
        let Some(issue) = self.selected_board_issue() else {
            return;
        };
        let number = issue.number;
        let new_state = if issue.state.eq_ignore_ascii_case("open") {
            "closed"
        } else {
            "open"
        };

        // First press: ask for confirmation. Second press: execute.
        if !self.board_confirm_close {
            self.board_confirm_close = true;
            self.show_toast(
                format!("Press x again to {new_state} #{number}"),
                ToastStyle::Info,
            );
            return;
        }
        self.board_confirm_close = false;

        let repo_path = self.selected_project().map(|p| p.repo_path.clone());
        let Some(repo_path) = repo_path else {
            return;
        };
        let state_owned = new_state.to_string();
        let tx = self.github_mutation_tx.clone();
        std::thread::spawn(move || {
            let result = crate::github::set_issue_state(&repo_path, number, &state_owned);
            let msg = match result {
                Ok(()) => super::GitHubMutationResult::StateChanged {
                    message: format!("Issue #{number} {state_owned}"),
                },
                Err(err) => super::GitHubMutationResult::Error {
                    message: format!("Failed to change state: {err}"),
                },
            };
            let _ = tx.send(msg);
        });
        // Optimistic UI update
        if let Some(col) = self.board_issues.get_mut(self.board_column_index)
            && let Some(item) = col.get_mut(self.board_issue_index)
        {
            item.state = new_state.to_uppercase();
        }
        self.show_toast(
            format!("Setting #{number} to {new_state}..."),
            ToastStyle::Info,
        );
    }

    // ── GitHub issue create/edit forms ───────────────────────────────

    fn open_github_issue_create(&mut self) {
        self.github_issue_form_title.clear();
        self.github_issue_form_body.clear();
        self.github_issue_form_labels.clear();
        self.github_issue_form_assignees.clear();
        self.github_issue_form_field = 0;
        self.github_issue_editing_number = None;
        self.input_mode = InputMode::CreateGitHubIssue;
    }

    fn open_github_issue_edit(&mut self) {
        let Some(issue) = self.selected_board_issue() else {
            return;
        };
        self.github_issue_form_title.clone_from(&issue.title);
        self.github_issue_form_body = issue.body.clone().unwrap_or_default();
        self.github_issue_form_labels = issue
            .labels
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        self.github_issue_form_assignees = issue
            .assignees
            .iter()
            .map(|a| a.login.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        self.github_issue_form_field = 0;
        self.github_issue_editing_number = Some(issue.number);
        self.input_mode = InputMode::EditGitHubIssue;
    }

    pub(super) fn handle_github_issue_form_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Esc => {
                self.input_mode = InputMode::BoardView;
            }
            KeyCode::Tab => {
                self.github_issue_form_field = (self.github_issue_form_field + 1) % 4;
            }
            KeyCode::BackTab => {
                self.github_issue_form_field = (self.github_issue_form_field + 3) % 4;
            }
            KeyCode::Enter if modifiers.contains(KeyModifiers::ALT) => {
                self.submit_github_issue_form();
            }
            _ => {
                let (buf, cursor) = match self.github_issue_form_field {
                    0 => (
                        &mut self.github_issue_form_title,
                        &mut self.comment_compose_cursor,
                    ),
                    1 => (
                        &mut self.github_issue_form_body,
                        &mut self.comment_compose_cursor,
                    ),
                    2 => (
                        &mut self.github_issue_form_labels,
                        &mut self.comment_compose_cursor,
                    ),
                    _ => (
                        &mut self.github_issue_form_assignees,
                        &mut self.comment_compose_cursor,
                    ),
                };
                if *cursor > buf.len() {
                    *cursor = buf.len();
                }
                apply_text_edit(buf, cursor, code, modifiers);
            }
        }
        Ok(())
    }

    fn submit_github_issue_form(&mut self) {
        let title = self.github_issue_form_title.trim().to_string();
        if title.is_empty() {
            self.show_toast("Title is required".to_string(), ToastStyle::Error);
            return;
        }
        let Some(project) = self.selected_project() else {
            return;
        };
        let repo_path = project.repo_path.clone();
        let body = self.github_issue_form_body.trim().to_string();
        let labels: Vec<String> = self
            .github_issue_form_labels
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let assignees: Vec<String> = self
            .github_issue_form_assignees
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let editing_number = self.github_issue_editing_number;
        let tx = self.github_mutation_tx.clone();

        std::thread::spawn(move || {
            let result = if let Some(number) = editing_number {
                crate::github::edit_issue(
                    &repo_path,
                    number,
                    Some(&title),
                    Some(&body),
                    Some(&labels),
                    Some(&assignees),
                )
                .map(|_| super::GitHubMutationResult::IssueEdited {
                    message: format!("Issue #{number} updated"),
                })
            } else {
                let body_opt = if body.is_empty() {
                    None
                } else {
                    Some(body.as_str())
                };
                crate::github::create_issue(&repo_path, &title, body_opt, &labels, &assignees).map(
                    |issue| super::GitHubMutationResult::IssueCreated {
                        message: format!("Issue #{} created", issue.number),
                    },
                )
            };
            let msg = match result {
                Ok(msg) => msg,
                Err(err) => super::GitHubMutationResult::Error {
                    message: format!("GitHub API error: {err}"),
                },
            };
            let _ = tx.send(msg);
        });

        self.input_mode = InputMode::BoardView;
        if editing_number.is_some() {
            self.show_toast("Updating issue...".to_string(), ToastStyle::Info);
        } else {
            self.show_toast("Creating issue...".to_string(), ToastStyle::Info);
        }
    }
}

fn combine_prompt(base: &str, extra_context: &str) -> String {
    let trimmed_base = base.trim();
    let trimmed_extra = extra_context.trim();
    match (trimmed_base.is_empty(), trimmed_extra.is_empty()) {
        (true, true) => String::new(),
        (false, true) => trimmed_base.to_string(),
        (true, false) => trimmed_extra.to_string(),
        (false, false) => format!("{trimmed_base}\n\nAdditional context:\n{trimmed_extra}"),
    }
}

fn build_launch_prompt_for_task(
    store: &crate::store::Store,
    task_id: &str,
    extra_context: &str,
) -> Result<String> {
    let task = store.get_task(task_id)?;
    Ok(combine_prompt(&task.description, extra_context))
}

fn build_launch_prompt_for_review_pr(
    review_pr: &PendingReviewPrLaunch,
    extra_context: &str,
) -> String {
    let mut base = String::new();
    match review_pr.mode {
        PendingReviewLaunchMode::ContinueWork => {
            let _ = writeln!(
                base,
                "Continue working on GitHub pull request #{}: {}",
                review_pr.number, review_pr.title
            );
            let _ = writeln!(base, "PR URL: {}", review_pr.url);
            if let (Some(base_ref), Some(head_ref)) =
                (review_pr.base_ref.as_deref(), review_pr.head_ref.as_deref())
            {
                let _ = writeln!(base, "Branch: {head_ref} -> {base_ref}");
            }
            if !review_pr.body.trim().is_empty() {
                base.push_str("\nPR description:\n");
                base.push_str(review_pr.body.trim());
            }
        }
        PendingReviewLaunchMode::Review => {
            let _ = writeln!(
                base,
                "Review GitHub pull request #{}: {}",
                review_pr.number, review_pr.title
            );
            let _ = writeln!(base, "PR URL: {}", review_pr.url);
            if let (Some(base_ref), Some(head_ref)) =
                (review_pr.base_ref.as_deref(), review_pr.head_ref.as_deref())
            {
                let _ = writeln!(base, "Branch: {head_ref} -> {base_ref}");
            }
            base.push_str(
                "\nReview goals:\n\
                 - inspect the code changes carefully before suggesting fixes\n\
                 - prioritize correctness, regressions, missing tests, security, and performance risks\n\
                 - produce concrete findings with file-level references\n\
                 - do not make code changes unless the user explicitly asks; start with the review\n",
            );
            if !review_pr.body.trim().is_empty() {
                base.push_str("\nPR description:\n");
                base.push_str(review_pr.body.trim());
            }
        }
    }

    combine_prompt(&base, extra_context)
}

// ── Free functions: key/mouse encoding ──

/// Convert a crossterm key event into the raw bytes a terminal would send.
/// Stack-allocated key byte buffer (avoids heap allocation per keystroke).
/// Maximum escape sequence is 4 bytes (e.g. `\x1b[3~`), and max UTF-8 char is 4 bytes.
pub(crate) struct KeyBytes {
    buf: [u8; 8],
    pub len: usize,
}

impl KeyBytes {
    const fn empty() -> Self {
        Self {
            buf: [0; 8],
            len: 0,
        }
    }

    fn from_slice(s: &[u8]) -> Self {
        let mut buf = [0u8; 8];
        let len = s.len().min(8);
        buf[..len].copy_from_slice(&s[..len]);
        Self { buf, len }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

pub(crate) fn keycode_to_bytes(code: KeyCode, modifiers: KeyModifiers) -> KeyBytes {
    // Ctrl+letter -> control character (ASCII)
    if modifiers.contains(KeyModifiers::CONTROL)
        && let KeyCode::Char(c) = code
    {
        let ctrl = (c.to_ascii_lowercase() as u8)
            .wrapping_sub(b'a')
            .wrapping_add(1);
        return KeyBytes {
            buf: [ctrl, 0, 0, 0, 0, 0, 0, 0],
            len: 1,
        };
    }

    // Modified special keys (arrows, Home, End, Insert, Delete, Page, F-keys).
    // Uses xterm modifier encoding: \x1b[1;{mod}X for arrow/Home/End,
    // \x1b[N;{mod}~ for tilde-style keys, \x1b[1;{mod}X for F1-F4.
    // mod = 1 + (shift?1:0) + (alt?2:0) + (ctrl?4:0)
    if modifiers != KeyModifiers::NONE
        && !matches!(code, KeyCode::Char(_))
        && let Some(bytes) = modified_special_key(code, modifiers)
    {
        return bytes;
    }

    // Alt+key -> ESC prefix (standard terminal convention)
    if modifiers.contains(KeyModifiers::ALT) {
        let base = keycode_to_bytes_base(code);
        if base.len > 0 {
            let mut buf = [0u8; 8];
            buf[0] = 0x1b;
            let copy_len = base.len.min(7);
            buf[1..=copy_len].copy_from_slice(&base.buf[..copy_len]);
            return KeyBytes {
                buf,
                len: 1 + copy_len,
            };
        }
        return KeyBytes::empty();
    }

    keycode_to_bytes_base(code)
}

/// Encode a special key (arrow, Home, End, etc.) with modifier bits in
/// xterm's `\x1b[1;{mod}X` / `\x1b[N;{mod}~` format.
///
/// Returns `None` for keys that don't have a modified encoding.
fn modified_special_key(code: KeyCode, modifiers: KeyModifiers) -> Option<KeyBytes> {
    let modifier_val = 1u8
        + u8::from(modifiers.contains(KeyModifiers::SHIFT))
        + u8::from(modifiers.contains(KeyModifiers::ALT)) * 2
        + u8::from(modifiers.contains(KeyModifiers::CONTROL)) * 4;

    // Keys using \x1b[1;{mod}{letter} format
    let letter_key = match code {
        KeyCode::Up => Some(b'A'),
        KeyCode::Down => Some(b'B'),
        KeyCode::Right => Some(b'C'),
        KeyCode::Left => Some(b'D'),
        KeyCode::Home => Some(b'H'),
        KeyCode::End => Some(b'F'),
        _ => None,
    };

    if let Some(k) = letter_key {
        let seq = [0x1b, b'[', b'1', b';', b'0' + modifier_val, k, 0, 0];
        return Some(KeyBytes { buf: seq, len: 6 });
    }

    // Keys using \x1b[{N};{mod}~ format (tilde-style)
    let tilde_param = match code {
        KeyCode::Insert => Some(b'2'),
        KeyCode::Delete => Some(b'3'),
        KeyCode::PageUp => Some(b'5'),
        KeyCode::PageDown => Some(b'6'),
        _ => None,
    };

    if let Some(n) = tilde_param {
        let seq = [0x1b, b'[', n, b';', b'0' + modifier_val, b'~', 0, 0];
        return Some(KeyBytes { buf: seq, len: 6 });
    }

    // F1-F4: \x1b[1;{mod}{P-S}
    if let KeyCode::F(n) = code
        && (1..=4).contains(&n)
    {
        let k = b'O' + n; // P=1, Q=2, R=3, S=4
        let seq = [0x1b, b'[', b'1', b';', b'0' + modifier_val, k, 0, 0];
        return Some(KeyBytes { buf: seq, len: 6 });
    }

    // F5-F12: \x1b[{N};{mod}~ (two-digit N requires dynamic formatting)
    if let KeyCode::F(n) = code
        && (5..=12).contains(&n)
    {
        let param = match n {
            5 => "15",
            6 => "17",
            7 => "18",
            8 => "19",
            9 => "20",
            10 => "21",
            11 => "23",
            12 => "24",
            _ => return None,
        };
        let s = format!("\x1b[{param};{modifier_val}~");
        return Some(KeyBytes::from_slice(s.as_bytes()));
    }

    None
}

/// Map a keycode (without modifiers) to its raw terminal bytes.
///
/// Philosophy: forward ALL byte-producing keys to the PTY by default.
/// Only keys intercepted earlier in `handle_session_tab_key` are excluded.
/// Non-byte-producing keys (modifier-only, media, etc.) return empty.
fn keycode_to_bytes_base(code: KeyCode) -> KeyBytes {
    match code {
        KeyCode::Char(c) => {
            let mut buf = [0u8; 8];
            let s = c.encode_utf8(&mut buf[..4]);
            let len = s.len();
            KeyBytes { buf, len }
        }
        KeyCode::Esc => KeyBytes::from_slice(b"\x1b"),
        KeyCode::Enter => KeyBytes::from_slice(b"\r"),
        KeyCode::Backspace => KeyBytes::from_slice(&[0x7f]),
        KeyCode::Tab => KeyBytes::from_slice(b"\t"),
        KeyCode::BackTab => KeyBytes::from_slice(b"\x1b[Z"),
        KeyCode::Up => KeyBytes::from_slice(b"\x1b[A"),
        KeyCode::Down => KeyBytes::from_slice(b"\x1b[B"),
        KeyCode::Right => KeyBytes::from_slice(b"\x1b[C"),
        KeyCode::Left => KeyBytes::from_slice(b"\x1b[D"),
        KeyCode::Home => KeyBytes::from_slice(b"\x1b[H"),
        KeyCode::End => KeyBytes::from_slice(b"\x1b[F"),
        KeyCode::Insert => KeyBytes::from_slice(b"\x1b[2~"),
        KeyCode::Delete => KeyBytes::from_slice(b"\x1b[3~"),
        KeyCode::PageUp => KeyBytes::from_slice(b"\x1b[5~"),
        KeyCode::PageDown => KeyBytes::from_slice(b"\x1b[6~"),
        KeyCode::Null => KeyBytes::from_slice(&[0x00]),
        KeyCode::F(n) => match n {
            1 => KeyBytes::from_slice(b"\x1bOP"),
            2 => KeyBytes::from_slice(b"\x1bOQ"),
            3 => KeyBytes::from_slice(b"\x1bOR"),
            4 => KeyBytes::from_slice(b"\x1bOS"),
            5 => KeyBytes::from_slice(b"\x1b[15~"),
            6 => KeyBytes::from_slice(b"\x1b[17~"),
            7 => KeyBytes::from_slice(b"\x1b[18~"),
            8 => KeyBytes::from_slice(b"\x1b[19~"),
            9 => KeyBytes::from_slice(b"\x1b[20~"),
            10 => KeyBytes::from_slice(b"\x1b[21~"),
            11 => KeyBytes::from_slice(b"\x1b[23~"),
            12 => KeyBytes::from_slice(b"\x1b[24~"),
            _ => KeyBytes::empty(),
        },
        // Modifier-only keys, media keys, etc. don't produce terminal bytes
        _ => KeyBytes::empty(),
    }
}

/// Encode a crossterm mouse event as terminal escape sequences for forwarding
/// to a PTY application that has enabled mouse tracking.
///
/// Returns `None` for event types that the protocol doesn't cover.
///
/// Supports both SGR encoding (`\x1b[<btn;col;rowM/m`) and the legacy
/// default encoding (`\x1b[Mbxy`).  SGR is preferred by modern TUI apps
/// (crossterm/ratatui) because it handles coordinates > 222 and
/// distinguishes press from release.
pub(crate) fn encode_mouse_event(
    kind: &MouseEventKind,
    vt_col: u16,
    vt_row: u16,
    encoding: vt100::MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    // SGR uses 1-based coordinates
    let x = u32::from(vt_col) + 1;
    let y = u32::from(vt_row) + 1;

    let (button, is_release) = match kind {
        MouseEventKind::Down(MouseButton::Left) => (0u8, false),
        MouseEventKind::Down(MouseButton::Right) => (2, false),
        MouseEventKind::Down(MouseButton::Middle) => (1, false),
        MouseEventKind::Up(MouseButton::Left) => (0, true),
        MouseEventKind::Up(MouseButton::Right) => (2, true),
        MouseEventKind::Up(MouseButton::Middle) => (1, true),
        MouseEventKind::Drag(MouseButton::Left) => (32, false), // motion + left
        MouseEventKind::Drag(MouseButton::Right) => (34, false), // motion + right
        MouseEventKind::Drag(MouseButton::Middle) => (33, false),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::Moved => (35, false), // motion, no button
        _ => return None,
    };

    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            let suffix = if is_release { 'm' } else { 'M' };
            Some(format!("\x1b[<{button};{x};{y}{suffix}").into_bytes())
        }
        // Default and UTF-8 both use the `\x1b[M` prefix format.
        // Default caps at 223; UTF-8 extends to higher values but uses the
        // same structure.  For simplicity we use the same path for both,
        // clamping to what the encoding can represent.
        _ => {
            if is_release {
                // Default encoding: release is button 3
                let cb = 3u8 + 32;
                let cx = u8::try_from(x.min(255))
                    .expect("clamped to 255")
                    .wrapping_add(32);
                let cy = u8::try_from(y.min(255))
                    .expect("clamped to 255")
                    .wrapping_add(32);
                Some(vec![0x1b, b'[', b'M', cb, cx, cy])
            } else {
                let cb = button.wrapping_add(32);
                let cx = u8::try_from(x.min(255))
                    .expect("clamped to 255")
                    .wrapping_add(32);
                let cy = u8::try_from(y.min(255))
                    .expect("clamped to 255")
                    .wrapping_add(32);
                Some(vec![0x1b, b'[', b'M', cb, cx, cy])
            }
        }
    }
}
