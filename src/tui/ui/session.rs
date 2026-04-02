use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line as TextLine, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::pty::{LayoutNode, PaneId, SplitDirection, TerminalWidget};

use super::super::app::{App, SessionTabView, Tab};
use super::super::form::render_hints;
use super::tab_bar::draw_tab_bar;
use super::workbench::draw_thread_session_view;

/// Draw the session tab: conversation view (chat + inspector) by default,
/// terminal PTY view via Ctrl+O toggle.
pub(super) fn draw_session_tab(frame: &mut Frame, app: &mut App) {
    let size = frame.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Min(0),    // body area
            Constraint::Length(1), // hint bar
        ])
        .split(size);

    draw_tab_bar(frame, app, outer[0]);

    let session_label = match app.tabs.get(app.active_tab) {
        Some(Tab::Session { label, .. }) => label.clone(),
        _ => String::new(),
    };
    let view_mode = match app.tabs.get(app.active_tab) {
        Some(Tab::Session { view_mode, .. }) => *view_mode,
        _ => SessionTabView::Terminal,
    };

    if matches!(view_mode, SessionTabView::Terminal | SessionTabView::Editor)
        && let Some(Tab::Session { terminals, .. }) = app.tabs.get_mut(app.active_tab)
    {
        let sizes = super::super::app::compute_pane_sizes_for_resize(
            &terminals.layout,
            size.width,
            size.height,
        );
        let _ = terminals.resize_panes_with_clear(&sizes);
    }

    // Prefer the cached thread context, but fall back to an on-demand load so
    // freshly restored sessions still render the native chat workspace.
    let thread_ctx = app.current_session_thread_context();

    match view_mode {
        SessionTabView::Conversation => {
            if let Some(ref ctx) = thread_ctx {
                draw_thread_session_view(frame, app, outer[1], &session_label, ctx);
            } else if let Some(Tab::Session {
                terminals, label, ..
            }) = app.tabs.get(app.active_tab)
            {
                render_layout_node(
                    &terminals.layout,
                    terminals,
                    label,
                    &app.theme,
                    frame,
                    outer[1],
                );
            }
        }
        SessionTabView::Terminal => {
            if let Some(Tab::Session {
                terminals, label, ..
            }) = app.tabs.get(app.active_tab)
            {
                render_layout_node(
                    &terminals.layout,
                    terminals,
                    label,
                    &app.theme,
                    frame,
                    outer[1],
                );
            }
        }
        SessionTabView::Editor => {
            // Render the editor pane full-screen (no border, no layout tree)
            if let Some(Tab::Session { terminals, .. }) = app.tabs.get(app.active_tab)
                && let Some(editor_id) = terminals.editor_pane_id
                && let Some(term) = terminals.terminal(editor_id)
            {
                let view = term.screen_view();
                frame.render_widget(TerminalWidget::new(&view, true), outer[1]);
            }
        }
    }

    // Hint bar
    if view_mode == SessionTabView::Conversation && thread_ctx.is_some() {
        let expand_hint = if app.inspector_expanded {
            ("  f", ": collapse  ")
        } else {
            ("  f", ": expand  ")
        };
        let mut hints: Vec<(&str, &str)> = vec![
            ("  i", ": compose  "),
            ("  q", ": close/done  "),
            expand_hint,
            ("  k/j", ": older/newer  "),
        ];
        hints.extend_from_slice(&[
            ("Ctrl+O", ": terminal  "),
            ("Ctrl+E", ": editor  "),
            ("Ctrl+G", ": bottom  "),
            ("  1-6", ": inspector  "),
            ("Ctrl+D", ": dashboard"),
        ]);
        render_hints(
            frame,
            outer[2],
            &hints,
            Style::default().fg(app.theme.accent_secondary),
            Style::default(),
        );
    } else if view_mode == SessionTabView::Editor {
        render_hints(
            frame,
            outer[2],
            &[
                ("Ctrl+O", ": chat  "),
                ("Ctrl+E", ": editor (exit)  "),
                ("Ctrl+J/K", ": switch tab  "),
                ("Ctrl+Q", ": close session  "),
            ],
            Style::default().fg(app.theme.accent_secondary),
            Style::default(),
        );
    } else {
        render_hints(
            frame,
            outer[2],
            &[
                ("  Ctrl+O", ": chat  "),
                ("Ctrl+E", ": editor  "),
                ("Ctrl+H/L", ": switch pane  "),
                ("Ctrl+J/K", ": switch tab  "),
                ("Ctrl+G", ": scroll bottom  "),
                ("Ctrl+B", ": split  "),
                ("Ctrl+W", ": close pane  "),
                ("Ctrl+Q", ": close session  "),
            ],
            Style::default().fg(app.theme.accent_secondary),
            Style::default(),
        );
    }

    // Confirmation overlay (rendered on top of everything)
    if app.input_mode == super::super::app::InputMode::ConfirmDelete
        && matches!(
            app.confirm_delete_kind,
            super::super::app::DeleteTarget::Session
        )
    {
        let session_name = &app.confirm_target;
        let dialog_width = (size.width * 2 / 3).max(50).min(size.width - 4);
        let dialog_height = 9;
        let dialog_area = Rect {
            x: (size.width.saturating_sub(dialog_width)) / 2,
            y: size.height / 2 - dialog_height / 2,
            width: dialog_width,
            height: dialog_height,
        };

        // Gradient title bar (like Crush)
        let title_bar = " Close Session ".to_string();
        let bar_fill: String =
            "\u{2571}".repeat((dialog_width.saturating_sub(title_bar.len() as u16 + 2)) as usize);
        let block = Block::default()
            .title(TextLine::from(vec![
                Span::styled(
                    title_bar,
                    Style::default()
                        .fg(app.theme.text_primary)
                        .bg(app.theme.accent_secondary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {bar_fill}"),
                    Style::default()
                        .fg(app.theme.accent_secondary)
                        .add_modifier(Modifier::DIM),
                ),
            ]))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.accent_secondary))
            .style(app.theme.main_surface());
        let inner = block.inner(dialog_area);
        frame.render_widget(ratatui::widgets::Clear, dialog_area);
        frame.render_widget(block, dialog_area);

        let lines = vec![
            TextLine::from(""),
            TextLine::from(vec![
                Span::styled("  Session  ", Style::default().fg(app.theme.text_secondary)),
                Span::styled(
                    session_name.to_string(),
                    Style::default()
                        .fg(app.theme.text_primary)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            TextLine::from(Span::styled(
                "  Worktree and all local changes will be permanently deleted.",
                Style::default().fg(app.theme.text_secondary),
            )),
            TextLine::from(""),
            TextLine::from(vec![
                Span::raw("  "),
                Span::styled(
                    " y: Confirm ",
                    Style::default()
                        .fg(app.theme.text_primary)
                        .bg(app.theme.status_error)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("   "),
                Span::styled(
                    " Esc: Cancel ",
                    Style::default()
                        .fg(app.theme.text_primary)
                        .bg(app.theme.border_unfocused),
                ),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }

    // Permission approval dialog — shown when the agent is waiting for tool permission
    if let Some(sid) = app.active_session_id()
        && app.paused_sessions.contains(sid)
        && app.input_mode != super::super::app::InputMode::ConfirmDelete
    {
        // Extract the permission details from the PTY screen
        let permission_text = extract_permission_text(app);
        let dialog_width = (size.width * 2 / 3).max(50).min(size.width - 4);
        let dialog_height = 7;
        let dialog_area = Rect {
            x: (size.width.saturating_sub(dialog_width)) / 2,
            y: size.height / 2 - dialog_height / 2,
            width: dialog_width,
            height: dialog_height,
        };
        let block = Block::default()
            .title(TextLine::from(Span::styled(
                " Permission Required ",
                Style::default()
                    .fg(app.theme.accent_secondary)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.accent_secondary))
            .style(app.theme.main_surface());
        let inner = block.inner(dialog_area);
        frame.render_widget(ratatui::widgets::Clear, dialog_area);
        frame.render_widget(block, dialog_area);

        let lines = vec![
            TextLine::from(""),
            TextLine::from(Span::styled(
                format!("  {permission_text}"),
                Style::default().fg(app.theme.text_primary),
            )),
            TextLine::from(""),
            TextLine::from(vec![
                Span::raw("  "),
                Span::styled(
                    " Enter: Allow ",
                    Style::default()
                        .fg(app.theme.text_primary)
                        .bg(app.theme.accent_tertiary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    " Esc: Deny ",
                    Style::default()
                        .fg(app.theme.text_primary)
                        .bg(app.theme.border_unfocused),
                ),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

/// Extract permission prompt text from the active session's Claude PTY screen.
fn extract_permission_text(app: &App) -> String {
    let Some(sid) = app.active_session_id() else {
        return "Tool permission requested".to_string();
    };
    // Try to get the "Allow ..." line from the PTY preview
    if let Some(lines) = app.pty_activity_preview.get(sid) {
        for line in lines {
            if let Some(pos) = line.find("Allow ") {
                return line[pos..].trim_end().to_string();
            }
        }
    }
    "Tool permission requested — check terminal for details".to_string()
}

/// Recursively render a layout node tree into the given area.
fn render_layout_node(
    node: &LayoutNode,
    terminals: &crate::pty::SessionTerminals,
    session_label: &str,
    theme: &super::super::theme::Theme,
    frame: &mut Frame,
    area: Rect,
) {
    match node {
        LayoutNode::Pane(id) => {
            render_single_pane(*id, terminals, session_label, theme, frame, area);
        }
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let dir = match direction {
                SplitDirection::Horizontal => Direction::Horizontal,
                SplitDirection::Vertical => Direction::Vertical,
            };
            let chunks = Layout::default()
                .direction(dir)
                .constraints([
                    Constraint::Percentage(*ratio),
                    Constraint::Percentage(100 - *ratio),
                ])
                .split(area);

            render_layout_node(first, terminals, session_label, theme, frame, chunks[0]);
            render_layout_node(second, terminals, session_label, theme, frame, chunks[1]);
        }
    }
}

/// Render a single terminal pane with border and title.
fn render_single_pane(
    id: PaneId,
    terminals: &crate::pty::SessionTerminals,
    session_label: &str,
    theme: &super::super::theme::Theme,
    frame: &mut Frame,
    area: Rect,
) {
    let Some(term) = terminals.terminal(id) else {
        return;
    };

    let is_focused = terminals.focused == id;
    let is_claude = id == terminals.claude_pane_id;

    let base_label = if is_claude {
        session_label.to_string()
    } else {
        terminals.label(id).to_string()
    };

    let scrollback = term.scrollback();
    let title = if scrollback > 0 {
        format!(" {base_label} [+{scrollback} lines] ")
    } else {
        format!(" {base_label} ")
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(if is_focused {
            theme.focused_border()
        } else {
            theme.unfocused_border()
        });

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sel = terminals.selection.as_ref().filter(|s| s.pane == id);

    let view = term.screen_view();
    frame.render_widget(
        TerminalWidget::new(&view, is_focused)
            .with_selection(sel)
            .with_scrollback_offset(term.scrollback()),
        inner,
    );
}
