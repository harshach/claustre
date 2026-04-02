use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Widget, Wrap},
};
use tui_markdown::from_str as markdown_from_str;

use super::super::app::{App, LaunchThreadField, MyTaskItem, ReviewDrawerTab, WorkbenchView};
use super::super::form::{format_with_cursor, measure_wrapped_height, render_hints, render_modal};
use super::super::theme::Theme;
use super::spinner_char;
use super::usage::format_tokens;
use crate::github::GitHubBoardItem;

/// A widget that dims the background by overwriting cells with a dark, low-contrast style.
/// Simulates the backdrop overlay effect common in GUI apps when a modal/drawer opens.
struct DimOverlay;

impl Widget for DimOverlay {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let dim_style = Style::default()
            .fg(Color::Rgb(60, 65, 80))
            .bg(Color::Rgb(15, 17, 25));
        for y in area.y..area.y.saturating_add(area.height) {
            for x in area.x..area.x.saturating_add(area.width) {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(dim_style);
                }
            }
        }
    }
}

pub(super) fn draw_command_palette(frame: &mut Frame, app: &App) {
    let height = (app.palette_filtered.len() as u16 + 3).clamp(6, 16);
    let inner = render_modal(
        frame,
        " Command Palette ",
        Style::default().fg(app.theme.accent_primary),
        56,
        height,
        app.theme.overlay_surface(),
    );

    if inner.height < 2 {
        return;
    }

    // Search input
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let cursor_pos = app.input_cursor.min(app.input_buffer.len());
    let (before, after) = app.input_buffer.split_at(cursor_pos);
    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(app.theme.accent_primary)),
        Span::raw(before.to_string()),
        Span::styled("\u{2588}", Style::default().fg(app.theme.accent_primary)),
        Span::raw(after.to_string()),
    ]);
    frame.render_widget(Paragraph::new(input_line), input_area);

    // Items
    let items_area = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(1),
    );
    let items: Vec<ListItem> = app
        .palette_filtered
        .iter()
        .enumerate()
        .map(|(i, &idx)| {
            let item = &app.palette_items[idx];
            let style = if i == app.palette_index {
                app.theme.selected_fill()
            } else {
                Style::default().fg(app.theme.text_primary)
            };
            let prefix = if i == app.palette_index { "▸ " } else { "  " };
            ListItem::new(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(&item.label, style),
            ]))
        })
        .collect();

    frame.render_widget(
        List::new(items).style(app.theme.overlay_surface()),
        items_area,
    );
}

pub(super) fn draw_project_picker_overlay(frame: &mut Frame, app: &App) {
    let height = (app.projects.len() as u16 * 3 + 5).clamp(10, 24);
    let inner = render_modal(
        frame,
        " Switch Repository ",
        Style::default().fg(app.theme.accent_primary),
        64,
        height,
        app.theme.overlay_surface(),
    );

    if inner.height < 4 {
        return;
    }

    frame.render_widget(
        Paragraph::new(
            " Select the active repository workspace for tasks, threads, reviews, and boards.",
        )
        .style(Style::default().fg(app.theme.text_secondary)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let items: Vec<ListItem> = app
        .projects
        .iter()
        .enumerate()
        .map(|(index, project)| {
            let selected = index == app.project_picker_index;
            let active = index == app.project_index;
            let prefix = if selected { "▸ " } else { "  " };
            let suffix = if active { "  current" } else { "" };
            let summary = app
                .project_summaries
                .get(&project.id)
                .cloned()
                .unwrap_or_default();
            let style = if selected {
                app.theme.selected_fill()
            } else {
                Style::default().fg(app.theme.text_primary)
            };

            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(prefix, style),
                    Span::styled(format!("{}{}", project.name, suffix), style),
                ]),
                Line::from(vec![Span::styled(
                    format!("    {}", project.repo_path),
                    Style::default().fg(app.theme.text_secondary),
                )]),
                Line::from(vec![Span::styled(
                    format!(
                        "    {} active session(s)  •  {} open task(s)",
                        summary.active_sessions.len(),
                        summary.task_counts.active_total()
                    ),
                    Style::default().fg(app.theme.text_secondary),
                )]),
            ])
        })
        .collect();

    frame.render_widget(
        List::new(items).style(app.theme.overlay_surface()),
        Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(2),
        ),
    );

    render_hints(
        frame,
        Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(1),
            inner.width,
            1,
        ),
        &[
            ("  j/k", ": navigate  "),
            ("Enter", ": switch repo  "),
            ("Esc", ": cancel"),
        ],
        Style::default().fg(app.theme.accent_primary),
        Style::default().fg(app.theme.text_secondary),
    );
}

pub(super) fn draw_thread_provider_picker_overlay(frame: &mut Frame, app: &App) {
    let inner = render_modal(
        frame,
        " Switch Provider ",
        Style::default().fg(app.theme.accent_primary),
        52,
        10,
        app.theme.overlay_surface(),
    );

    let provider_items: Vec<ListItem> = [
        crate::store::ProviderKind::Claude,
        crate::store::ProviderKind::Codex,
    ]
    .iter()
    .enumerate()
    .map(|(index, provider)| {
        let selected = index == app.thread_provider_picker_index;
        let style = if selected {
            app.theme.selected_fill()
        } else {
            Style::default().fg(app.theme.text_primary)
        };
        let prefix = if selected { "▸ " } else { "  " };
        ListItem::new(vec![
            Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(provider.to_string(), style),
            ]),
            Line::from(vec![Span::styled(
                match provider {
                    crate::store::ProviderKind::Claude => "    Anthropic CLI-style coding session",
                    crate::store::ProviderKind::Codex => {
                        "    OpenAI Codex CLI-style coding session"
                    }
                    crate::store::ProviderKind::Gemini => "    Gemini-compatible coding session",
                    crate::store::ProviderKind::Local => "    Local model session",
                    crate::store::ProviderKind::Unknown => {
                        "    Provider profile is not fully configured"
                    }
                },
                Style::default().fg(app.theme.text_secondary),
            )]),
        ])
    })
    .collect();

    frame.render_widget(
        List::new(provider_items).style(app.theme.overlay_surface()),
        Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(1),
        ),
    );

    render_hints(
        frame,
        Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(1),
            inner.width,
            1,
        ),
        &[
            ("  j/k", ": navigate  "),
            ("Enter", ": switch  "),
            ("Esc", ": cancel"),
        ],
        Style::default().fg(app.theme.accent_primary),
        Style::default().fg(app.theme.text_secondary),
    );
}

pub(super) fn draw_subtask_panel(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));
    let inner_width = width.saturating_sub(2);
    let list_height = app.subtasks.len().min(10) as u16;

    // Measure input text wrapping for auto-adjust
    let input_text = format!(
        "  > {}",
        format_with_cursor(&app.input_buffer, app.input_cursor)
    );
    let input_lines = measure_wrapped_height(&input_text, inner_width);

    // Base: list/placeholder(1) + separator(1) + input + hints(1) + padding(4 for borders+gaps)
    let content_height = list_height.max(1) + 1 + input_lines + 1;
    let height = content_height + 4;

    let inner = render_modal(
        frame,
        " Subtasks ",
        Style::default().fg(app.theme.form_border_task),
        width,
        height,
        app.theme.overlay_surface(),
    );

    if inner.height < 3 || inner.width < 20 {
        return;
    }

    let dim = Style::default().fg(app.theme.form_dim);
    let highlight = Style::default().fg(app.theme.form_highlight);

    // Render existing subtasks
    let mut y_offset = 0u16;
    for (i, st) in app.subtasks.iter().enumerate() {
        if y_offset >= inner.height.saturating_sub(3) {
            break;
        }
        let status_style = app.theme.task_status_style(st.status);
        let prefix = if i == app.subtask_index { "▸ " } else { "  " };
        let selector_style = if i == app.subtask_index {
            Style::default().fg(app.theme.selection_indicator)
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, selector_style),
                Span::styled(st.status.symbol(), status_style),
                Span::raw(" "),
                Span::styled(&st.title, Style::default().fg(app.theme.text_primary)),
            ])),
            Rect::new(inner.x, inner.y + y_offset, inner.width, 1),
        );
        y_offset += 1;
    }

    if app.subtasks.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("  No subtasks yet", dim)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        y_offset = 1;
    }

    // Separator
    y_offset += 1;

    // Input line (auto-adjusting height based on wrapped text)
    let input_val = format_with_cursor(&app.input_buffer, app.input_cursor);
    let available_for_input = inner.height.saturating_sub(y_offset + 2); // reserve hints + pad
    let input_h = input_lines.min(available_for_input).max(1);
    if inner.y + y_offset < inner.y + inner.height.saturating_sub(1) {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  > ", highlight),
                Span::styled(input_val, Style::default().fg(app.theme.text_primary)),
            ]))
            .wrap(Wrap { trim: false }),
            Rect::new(inner.x, inner.y + y_offset, inner.width, input_h),
        );
        y_offset += input_h;
    }

    // Hints at bottom
    let hints_y = inner.y + y_offset + 1;
    if hints_y < inner.y + inner.height {
        render_hints(
            frame,
            Rect::new(inner.x, hints_y, inner.width, 1),
            &[
                ("  Enter", ":add  "),
                ("d", ":del  "),
                ("j/k", ":nav  "),
                ("Esc", ":close"),
            ],
            highlight,
            dim,
        );
    }
}

pub(super) fn draw_settings_edit_overlay(frame: &mut Frame, app: &App) {
    let Some(target) = app.settings_edit_target else {
        return;
    };

    let title = format!(" Edit {} ", target.label());
    let inner = render_modal(
        frame,
        &title,
        Style::default().fg(app.theme.accent_primary),
        72,
        7,
        app.theme.overlay_surface(),
    );

    if inner.width < 8 || inner.height < 3 {
        return;
    }

    let value = format_with_cursor(&app.input_buffer, app.input_cursor);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(app.theme.accent_primary)),
            Span::styled(value, Style::default().fg(app.theme.text_primary)),
        ]))
        .wrap(Wrap { trim: false }),
        Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(1),
        ),
    );

    render_hints(
        frame,
        Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(1),
            inner.width,
            1,
        ),
        &[(" Enter", ":save  "), ("Esc", ":cancel")],
        Style::default().fg(app.theme.accent_primary),
        Style::default().fg(app.theme.text_secondary),
    );
}

pub(super) fn draw_settings_picker_overlay(frame: &mut Frame, app: &App) {
    let Some(target) = app.settings_picker_target else {
        return;
    };

    let title = format!("{} ", target.label());
    let current_value = target.current_value(&app.config);
    let option_count = app.settings_picker_options.len();
    // Height: 2 (padding + hints) + options + 1 (bottom padding)
    let height = (option_count as u16 + 5).clamp(8, 20);
    let inner = render_modal(
        frame,
        &title,
        Style::default().fg(app.theme.accent_primary),
        52,
        height,
        app.theme.overlay_surface(),
    );

    if inner.width < 8 || inner.height < 3 {
        return;
    }

    let mut y = inner.y;

    // Options list
    y += 1; // top padding
    for (i, option) in app.settings_picker_options.iter().enumerate() {
        if y >= inner.y + inner.height.saturating_sub(2) {
            break;
        }
        let selected = i == app.settings_picker_index;
        let is_current = option.eq_ignore_ascii_case(&current_value);

        let style = if selected {
            app.theme.selected_fill()
        } else {
            Style::default().fg(app.theme.text_primary)
        };

        let mut spans = vec![Span::styled(
            format!(" {option}"),
            if selected {
                Style::default()
                    .fg(app.theme.text_primary)
                    .bg(app.theme.accent_primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                style
            },
        )];

        // Pad to fill width for selection highlight
        let label_len = option.len() + 1; // +1 for leading space
        let current_tag = if is_current { "current" } else { "" };
        let pad = inner
            .width
            .saturating_sub(label_len as u16)
            .saturating_sub(current_tag.len() as u16) as usize;

        if selected {
            spans.push(Span::styled(
                " ".repeat(pad),
                Style::default().bg(app.theme.accent_primary),
            ));
            if is_current {
                spans.push(Span::styled(
                    current_tag,
                    Style::default()
                        .fg(app.theme.text_secondary)
                        .bg(app.theme.accent_primary),
                ));
            }
        } else if is_current {
            spans.push(Span::styled(" ".repeat(pad), style));
            spans.push(Span::styled(
                current_tag,
                Style::default().fg(app.theme.text_secondary),
            ));
        }

        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(inner.x, y, inner.width, 1),
        );
        y += 1;
    }

    // Hint bar at bottom
    render_hints(
        frame,
        Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(2),
            inner.width,
            1,
        ),
        &[
            (" \u{2191}/\u{2193}", ":choose  "),
            ("Enter", ":confirm  "),
            ("Esc", ":cancel"),
        ],
        Style::default().fg(app.theme.accent_primary),
        Style::default().fg(app.theme.text_secondary),
    );
}

pub(super) fn draw_launch_thread_overlay(frame: &mut Frame, app: &App) {
    let Some(draft) = app.launch_thread_draft.as_ref() else {
        return;
    };

    let field = LaunchThreadField::ALL
        .get(app.launch_thread_field_index)
        .copied()
        .unwrap_or(LaunchThreadField::Provider);
    let modal_width = 92u16.min(frame.area().width.saturating_sub(4));
    let modal_height = 30u16.min(frame.area().height.saturating_sub(4));
    let inner = render_modal(
        frame,
        draft.modal_title(),
        Style::default().fg(app.theme.accent_primary),
        modal_width,
        modal_height,
        app.theme.overlay_surface(),
    );

    if inner.width < 32 || inner.height < 10 {
        return;
    }

    // Workflow stage preview: show stages when a workflow is selected
    let has_workflow_preview = !draft.workflow_name.trim().is_empty();
    let preview_height: u16 = u16::from(has_workflow_preview);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),              // [0] summary
            Constraint::Length(1),              // [1] provider
            Constraint::Length(1),              // [2] runtime profile
            Constraint::Length(1),              // [3] workflow
            Constraint::Length(preview_height), // [4] workflow stage preview
            Constraint::Length(1),              // [5] title
            Constraint::Min(4),                 // [6] extra context
            Constraint::Length(1),              // [7] hints
        ])
        .split(inner);

    let summary_style = Style::default().fg(app.theme.text_primary);
    let dim_style = Style::default().fg(app.theme.text_secondary);
    let selected_style = Style::default()
        .fg(app.theme.accent_primary)
        .add_modifier(Modifier::BOLD);

    let summary_lines = vec![
        Line::from(vec![
            Span::styled(
                format!(" {} ", draft.source_kind_label()),
                app.theme.chip_style(app.theme.accent_secondary),
            ),
            Span::raw(" "),
            Span::styled(
                format!(" {} ", draft.provider_kind),
                app.theme.chip_style(app.theme.accent_primary),
            ),
            Span::raw(" "),
            Span::styled(
                if draft.workflow_name.trim().is_empty() {
                    " no workflow ".to_string()
                } else {
                    format!(" {} ", draft.workflow_name)
                },
                app.theme.chip_style(app.theme.tab_inactive),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Source: ", selected_style),
            Span::styled(draft.source_label(), summary_style),
        ]),
        Line::from(vec![
            Span::styled(" Project: ", selected_style),
            Span::styled(
                app.selected_project()
                    .map_or("none".to_string(), |project| project.name.clone()),
                summary_style,
            ),
        ]),
        Line::from(vec![
            Span::styled(" Result: ", selected_style),
            Span::styled(draft.launch_result_label(), summary_style),
        ]),
        Line::from(Span::styled(draft.launch_summary(), dim_style)),
    ];
    frame.render_widget(
        Paragraph::new(summary_lines).wrap(Wrap { trim: false }),
        vertical[0],
    );

    let render_field = |frame: &mut Frame, area: Rect, target: LaunchThreadField, value: String| {
        let selected = field == target;
        let label_style = if selected {
            selected_style
        } else {
            summary_style
        };
        let marker = if selected { ">" } else { " " };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{marker} {}: ", target.label()), label_style),
                Span::styled(value, if selected { label_style } else { summary_style }),
            ])),
            area,
        );
    };

    render_field(
        frame,
        vertical[1],
        LaunchThreadField::Provider,
        format!("< {} >", draft.provider_kind),
    );
    render_field(
        frame,
        vertical[2],
        LaunchThreadField::RuntimeProfile,
        if draft.runtime_profile.trim().is_empty() {
            "auto".to_string()
        } else {
            draft.runtime_profile.clone()
        },
    );
    render_field(
        frame,
        vertical[3],
        LaunchThreadField::Workflow,
        if draft.workflow_name.trim().is_empty() {
            "< none >".to_string()
        } else {
            format!("< {} >", draft.workflow_name)
        },
    );
    // Stage preview line
    if has_workflow_preview {
        let stage_names: Vec<String> = app
            .available_workflow_names
            .iter()
            .find(|n| **n == draft.workflow_name)
            .and_then(|_| {
                crate::workflows::builtin_workflows()
                    .into_iter()
                    .find(|d| d.name == draft.workflow_name)
                    .or_else(|| {
                        crate::workflows::load_workflow_definitions(None)
                            .ok()?
                            .into_iter()
                            .find(|d| d.name == draft.workflow_name)
                    })
            })
            .map(|def| def.stages.into_iter().map(|s| s.name).collect())
            .unwrap_or_default();
        if !stage_names.is_empty() {
            let preview = format!(
                "   {} stages: {}",
                stage_names.len(),
                stage_names.join(" \u{203A} ")
            );
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    preview,
                    Style::default().fg(app.theme.text_secondary),
                ))),
                vertical[4],
            );
        }
    }

    render_field(
        frame,
        vertical[5],
        LaunchThreadField::Title,
        if field == LaunchThreadField::Title {
            format_with_cursor(&app.input_buffer, app.input_cursor)
        } else {
            draft.title.clone()
        },
    );

    let context_block = Block::default()
        .title(" Additional Context ")
        .borders(Borders::ALL)
        .border_style(if field == LaunchThreadField::ExtraContext {
            Style::default().fg(app.theme.accent_primary)
        } else {
            Style::default().fg(app.theme.text_secondary)
        });
    let context_inner = context_block.inner(vertical[6]);
    frame.render_widget(context_block, vertical[6]);
    let context_value = if field == LaunchThreadField::ExtraContext {
        format_with_cursor(&app.input_buffer, app.input_cursor)
    } else if draft.extra_context.trim().is_empty() {
        "Optional instructions, constraints, notes, or pasted context.".to_string()
    } else {
        draft.extra_context.clone()
    };
    let context_style =
        if draft.extra_context.trim().is_empty() && field != LaunchThreadField::ExtraContext {
            dim_style
        } else {
            summary_style
        };
    // Auto-scroll the text area so the cursor line is always visible
    let scroll_offset = if field == LaunchThreadField::ExtraContext && context_inner.width > 0 {
        let wrap_width = context_inner.width as usize;
        // Count visual lines up to the cursor position
        let text_before_cursor = &app.input_buffer[..app.input_cursor.min(app.input_buffer.len())];
        let mut visual_lines: u16 = 0;
        for line in text_before_cursor.split('\n') {
            let line_visual = if wrap_width > 0 {
                ((line.len() as u16) / (wrap_width as u16)) + 1
            } else {
                1
            };
            visual_lines += line_visual;
        }
        visual_lines.saturating_sub(context_inner.height)
    } else {
        0
    };
    frame.render_widget(
        Paragraph::new(context_value)
            .style(context_style)
            .wrap(Wrap { trim: false })
            .scroll((scroll_offset, 0)),
        context_inner,
    );

    render_hints(
        frame,
        vertical[7],
        &[
            (" Tab", ":next field  "),
            ("Shift+Tab", ":prev  "),
            ("h/l", ":provider/workflow  "),
            (" Enter", ":launch  "),
            ("Esc", ":cancel"),
        ],
        Style::default().fg(app.theme.accent_primary),
        dim_style,
    );
}

pub(super) fn draw_github_installation_overlay(frame: &mut Frame, app: &App) {
    let count = app.github_installations.len().max(1);
    let height = (count as u16 + 4).min(18);
    let inner = render_modal(
        frame,
        " GitHub Installations ",
        Style::default().fg(app.theme.accent_primary),
        72,
        height,
        app.theme.overlay_surface(),
    );

    if inner.width < 20 || inner.height < 3 {
        return;
    }

    let list_height = inner.height.saturating_sub(1);
    let default_id = app.config.github_app.default_installation_id.as_deref();

    if app.github_installations.is_empty() {
        let message = if app.busy_indicator_label() == Some("loading GitHub installations") {
            format!("  {} Loading GitHub installations...", spinner_char())
        } else {
            "  No installations loaded. Press f to refresh.".to_string()
        };
        frame.render_widget(
            Paragraph::new(message).wrap(Wrap { trim: false }),
            Rect::new(inner.x, inner.y, inner.width, list_height.max(1)),
        );
    } else {
        let items: Vec<ListItem> = app
            .github_installations
            .iter()
            .enumerate()
            .map(|(idx, installation)| {
                let selected = idx == app.github_installation_index;
                let installation_id = installation.id.to_string();
                let is_default = default_id == Some(installation_id.as_str());
                let prefix = if selected { "▸ " } else { "  " };
                let marker = if is_default { " default" } else { "" };
                let style = if selected {
                    Style::default()
                        .fg(app.theme.accent_primary)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.text_primary)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(prefix, style),
                    Span::styled(
                        format!(
                            "{} ({}){}",
                            installation.account.login, installation_id, marker
                        ),
                        style,
                    ),
                ]))
            })
            .collect();
        frame.render_widget(
            List::new(items),
            Rect::new(inner.x, inner.y, inner.width, list_height.max(1)),
        );
    }

    render_hints(
        frame,
        Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(1),
            inner.width,
            1,
        ),
        &[
            (" j/k", ":nav  "),
            (" Enter", ":select  "),
            ("f", ":refresh  "),
            ("Esc", ":close"),
        ],
        Style::default().fg(app.theme.accent_primary),
        Style::default().fg(app.theme.form_dim),
    );
}

pub(super) fn draw_skill_panel(frame: &mut Frame, app: &App) {
    let scope_label = if app.skill_scope_global {
        "global"
    } else {
        "project"
    };
    let inner = render_modal(
        frame,
        &format!(" Skills [{scope_label}] "),
        Style::default().fg(app.theme.accent_primary),
        80,
        20,
        app.theme.overlay_surface(),
    );

    if inner.height < 3 || inner.width < 30 {
        return;
    }

    // Split inner into left (skill list 40%) and right (detail 60%)
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(inner);

    // Reserve 1 row for hints at the bottom
    let list_area = Rect::new(
        halves[0].x,
        halves[0].y,
        halves[0].width,
        halves[0].height.saturating_sub(1),
    );
    let detail_area = Rect::new(
        halves[1].x,
        halves[1].y,
        halves[1].width,
        halves[1].height.saturating_sub(1),
    );

    // LEFT: Skill list
    if app.installed_skills.is_empty() {
        let msg = Paragraph::new("  No skills installed.\n  Press 'f' to find\n  or 'a' to add.");
        frame.render_widget(
            msg.style(Style::default().fg(app.theme.text_secondary)),
            list_area,
        );
    } else {
        let items: Vec<ListItem> = app
            .installed_skills
            .iter()
            .enumerate()
            .map(|(i, skill)| {
                let is_selected = i == app.skill_index;
                let style = if is_selected {
                    Style::default()
                        .fg(app.theme.text_primary)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.text_primary)
                };
                let prefix = if is_selected { "\u{25b8} " } else { "  " };
                let prefix_style = if is_selected {
                    Style::default().fg(app.theme.selection_indicator)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(prefix, prefix_style),
                    Span::styled(&skill.name, style),
                ]))
            })
            .collect();
        let list = List::new(items);
        frame.render_widget(list, list_area);
    }

    // RIGHT: Skill detail
    if let Some(skill) = app.installed_skills.get(app.skill_index) {
        let max_lines = detail_area.height.saturating_sub(3) as usize;
        let mut lines = vec![
            Line::from(vec![
                Span::styled("  Name: ", Style::default().fg(app.theme.text_secondary)),
                Span::styled(&skill.name, Style::default().fg(app.theme.text_accent)),
            ]),
            Line::from(vec![
                Span::styled("  Agents: ", Style::default().fg(app.theme.text_secondary)),
                Span::styled(
                    skill.agents.join(", "),
                    Style::default().fg(app.theme.text_primary),
                ),
            ]),
            Line::from(""),
        ];

        for md_line in app.skill_detail_content.lines().take(max_lines) {
            lines.push(Line::from(Span::styled(
                format!("  {md_line}"),
                Style::default().fg(app.theme.text_primary),
            )));
        }
        if app.skill_detail_content.lines().count() > max_lines {
            lines.push(Line::from(Span::styled(
                "  ...",
                Style::default().fg(app.theme.text_secondary),
            )));
        }

        let detail = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(detail, detail_area);
    }

    // Hints at the bottom of the panel
    let hints_y = inner.y + inner.height.saturating_sub(1);
    render_hints(
        frame,
        Rect::new(inner.x, hints_y, inner.width, 1),
        &[
            (" f", ":find  "),
            ("a", ":add  "),
            ("x", ":remove  "),
            ("u", ":update  "),
            ("g", ":global/project  "),
            ("Esc", ":close"),
        ],
        Style::default().fg(app.theme.text_accent),
        Style::default().fg(app.theme.text_secondary),
    );
}

pub(super) fn draw_skill_search_overlay(frame: &mut Frame, app: &App) {
    let width = 54u16.min(frame.area().width.saturating_sub(4));
    let inner_width = width.saturating_sub(2);
    let result_rows = app.search_results.len().min(8) as u16;
    let has_status = !app.skill_status_message.is_empty();
    let status_row = u16::from(has_status);

    // Measure input wrapping for auto-adjust
    let input_text = format!(
        "> {}",
        format_with_cursor(&app.input_buffer, app.input_cursor)
    );
    let input_lines = measure_wrapped_height(&input_text, inner_width);

    // input lines + optional status + results + hints = rows inside borders
    let height = (3 + input_lines + status_row + result_rows).clamp(7, 18);
    let inner = render_modal(
        frame,
        " Find Skills ",
        Style::default().fg(app.theme.form_highlight),
        width,
        height,
        app.theme.overlay_surface(),
    );

    if inner.height < 2 {
        return;
    }

    // Search input (auto-adjusting)
    let input_h = input_lines.min(inner.height.saturating_sub(1));
    let ss_cursor = app.input_cursor.min(app.input_buffer.len());
    let (ss_before, ss_after) = app.input_buffer.split_at(ss_cursor);
    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(app.theme.form_highlight)),
        Span::raw(ss_before.to_string()),
        Span::styled("\u{2588}", Style::default().fg(app.theme.form_highlight)),
        Span::raw(ss_after.to_string()),
    ]);
    frame.render_widget(
        Paragraph::new(input_line).wrap(Wrap { trim: false }),
        Rect::new(inner.x, inner.y, inner.width, input_h),
    );

    // Status message (shown after search completes)
    let mut next_y = inner.y + input_h;
    if has_status {
        let color = if app.skill_status_message.starts_with("Search failed")
            || app.skill_status_message.starts_with("Install failed")
        {
            app.theme.toast_error
        } else {
            app.theme.text_secondary
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                &app.skill_status_message,
                Style::default().fg(color),
            )),
            Rect::new(inner.x, next_y, inner.width, 1),
        );
        next_y += 1;
    }

    // Search results
    if !app.search_results.is_empty() {
        let items_area = Rect::new(
            inner.x,
            next_y,
            inner.width,
            inner.height.saturating_sub(next_y - inner.y + 1),
        );
        let items: Vec<ListItem> = app
            .search_results
            .iter()
            .enumerate()
            .map(|(i, result)| {
                let is_cursor = i == app.skill_index;
                let is_selected = app.selected_search_indices.contains(&i);
                let style = if is_cursor {
                    app.theme.selected_fill()
                } else {
                    Style::default().fg(app.theme.text_primary)
                };
                let checkbox = if is_selected { "[x] " } else { "[ ] " };
                let checkbox_style = if is_selected {
                    Style::default().fg(app.theme.selection_indicator)
                } else {
                    Style::default().fg(app.theme.text_secondary)
                };
                let mut spans = vec![
                    Span::styled(checkbox, checkbox_style),
                    Span::styled(&result.package, style),
                ];
                if !result.installs.is_empty() {
                    spans.push(Span::styled(
                        format!("  {}", result.installs),
                        Style::default().fg(app.theme.text_secondary),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        frame.render_widget(
            List::new(items).style(app.theme.overlay_surface()),
            items_area,
        );
    }

    // Hints at bottom
    let hints_y = inner.y + inner.height.saturating_sub(1);
    let key_style = Style::default().fg(app.theme.form_highlight);
    let desc_style = Style::default().fg(app.theme.text_secondary);
    let selected_count = app.selected_search_indices.len();
    let install_label = if app.search_results.is_empty() {
        ":search  ".to_string()
    } else if selected_count > 0 {
        format!(":install ({selected_count})  ")
    } else {
        ":search/install  ".to_string()
    };
    let mut hint_spans = vec![
        Span::styled("Enter", key_style),
        Span::styled(install_label, desc_style),
    ];
    if !app.search_results.is_empty() {
        hint_spans.push(Span::styled("Space", key_style));
        hint_spans.push(Span::styled(":select  ", desc_style));
        hint_spans.push(Span::styled("j/k", key_style));
        hint_spans.push(Span::styled(":navigate  ", desc_style));
    }
    hint_spans.push(Span::styled("Esc", key_style));
    hint_spans.push(Span::styled(":back", desc_style));
    frame.render_widget(
        Paragraph::new(Line::from(hint_spans)),
        Rect::new(inner.x, hints_y, inner.width, 1),
    );
}

pub(super) fn draw_skill_add_overlay(frame: &mut Frame, app: &App) {
    let width = 56u16.min(frame.area().width.saturating_sub(4));
    let inner_width = width.saturating_sub(2);

    // Measure input wrapping for auto-adjust
    let input_text = format!(
        "> {}",
        format_with_cursor(&app.input_buffer, app.input_cursor)
    );
    let input_lines = measure_wrapped_height(&input_text, inner_width);

    let height = (4u16 + input_lines).clamp(6, 12);
    let inner = render_modal(
        frame,
        " Add Skill (owner/repo@skill) ",
        Style::default().fg(app.theme.form_highlight),
        width,
        height,
        app.theme.overlay_surface(),
    );

    if inner.height < 2 {
        return;
    }

    // Package input (auto-adjusting)
    let input_h = input_lines.min(inner.height.saturating_sub(1));
    let sa_cursor = app.input_cursor.min(app.input_buffer.len());
    let (sa_before, sa_after) = app.input_buffer.split_at(sa_cursor);
    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(app.theme.form_highlight)),
        Span::raw(sa_before.to_string()),
        Span::styled("\u{2588}", Style::default().fg(app.theme.form_highlight)),
        Span::raw(sa_after.to_string()),
    ]);
    frame.render_widget(
        Paragraph::new(input_line).wrap(Wrap { trim: false }),
        Rect::new(inner.x, inner.y, inner.width, input_h),
    );

    // Hints at bottom
    let hints_y = inner.y + inner.height.saturating_sub(1);
    render_hints(
        frame,
        Rect::new(inner.x, hints_y, inner.width, 1),
        &[("Enter", ":install  "), ("Esc", ":back")],
        Style::default().fg(app.theme.form_highlight),
        Style::default().fg(app.theme.text_secondary),
    );
}

pub(super) fn draw_task_details_panel(frame: &mut Frame, app: &App) {
    let theme = &app.theme;
    let selected_board_item = if app.workbench_view == WorkbenchView::SprintBoard {
        app.selected_board_issue()
    } else {
        None
    };
    let selected_my_task_item = if app.workbench_view == WorkbenchView::MyTasks {
        app.selected_my_task_item()
    } else {
        None
    };
    let selected_task = app.selected_task();
    if selected_board_item.is_none() && selected_my_task_item.is_none() && selected_task.is_none() {
        return;
    }

    let inner = render_modal(
        frame,
        " Details — press v or Esc to close ",
        Style::default().fg(theme.accent_primary),
        80,
        30,
        theme.overlay_surface(),
    );

    if let Some(item) = selected_board_item.as_ref() {
        frame.render_widget(
            Paragraph::new(markdown_from_str(&board_item_details_markdown(item)))
                .scroll((app.task_details_scroll, 0))
                .style(theme.overlay_surface())
                .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    if let Some(item) = selected_my_task_item {
        frame.render_widget(
            Paragraph::new(markdown_from_str(&my_task_item_details_markdown(item)))
                .scroll((app.task_details_scroll, 0))
                .style(theme.overlay_surface())
                .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    let Some(task) = selected_task else {
        return;
    };

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Title
    lines.push(Line::from(vec![
        Span::styled(
            "  Title: ",
            Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(task.title.clone(), Style::default().fg(theme.text_primary)),
    ]));

    // Status
    lines.push(Line::from(vec![
        Span::styled(
            "  Status: ",
            Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} {}", task.status.symbol(), task.status.as_str()),
            theme.task_status_style(task.status),
        ),
    ]));

    // Mode
    lines.push(Line::from(vec![
        Span::styled(
            "  Mode: ",
            Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            task.mode.as_str().to_string(),
            Style::default().fg(theme.text_primary),
        ),
    ]));

    // Push mode
    lines.push(Line::from(vec![
        Span::styled(
            "  Push: ",
            Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            task.push_mode.as_str().to_string(),
            Style::default().fg(theme.text_primary),
        ),
    ]));

    // Review loop
    lines.push(Line::from(vec![
        Span::styled(
            "  Review loop: ",
            Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if task.review_loop { "yes" } else { "no" },
            Style::default().fg(theme.text_primary),
        ),
    ]));

    // Base (PR target branch)
    if let Some(ref base) = task.base {
        lines.push(Line::from(vec![
            Span::styled(
                "  Base: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(base.clone(), Style::default().fg(theme.text_primary)),
        ]));
    }

    // Branch (existing branch to reuse)
    if let Some(ref branch) = task.branch {
        lines.push(Line::from(vec![
            Span::styled(
                "  Branch: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(branch.clone(), Style::default().fg(theme.text_primary)),
        ]));
    }

    // PR URL
    if let Some(ref url) = task.pr_url {
        lines.push(Line::from(vec![
            Span::styled(
                "  PR: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(url.clone(), Style::default().fg(theme.pr_link)),
        ]));
    }

    // Token usage
    let total_tokens = task.input_tokens + task.output_tokens;
    if total_tokens > 0 {
        lines.push(Line::from(vec![
            Span::styled(
                "  Tokens: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "{} in / {} out",
                    format_tokens(task.input_tokens),
                    format_tokens(task.output_tokens),
                ),
                Style::default().fg(theme.text_primary),
            ),
        ]));
    }

    // Timing
    if let Some(ref started) = task.started_at {
        lines.push(Line::from(vec![
            Span::styled(
                "  Started: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(started.clone(), Style::default().fg(theme.text_primary)),
        ]));
    }
    if let Some(ref completed) = task.completed_at {
        lines.push(Line::from(vec![
            Span::styled(
                "  Completed: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(completed.clone(), Style::default().fg(theme.text_primary)),
        ]));
    }

    if let Some(thread_ctx) = app.selected_thread_context() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Thread Workspace",
            Style::default()
                .fg(theme.accent_secondary)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "  ─".to_string() + &"─".repeat(inner.width.saturating_sub(4) as usize),
            Style::default().fg(theme.text_secondary),
        )));
        lines.push(Line::from(vec![
            Span::styled(
                "  Thread: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                thread_ctx.identity_summary(),
                Style::default().fg(theme.text_primary),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                "  Activity: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                thread_ctx.activity_summary(),
                Style::default().fg(theme.text_primary),
            ),
        ]));
        if let Some(run_summary) = thread_ctx.latest_run_summary() {
            lines.push(Line::from(vec![
                Span::styled(
                    "  Agent: ",
                    Style::default()
                        .fg(theme.text_secondary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(run_summary, Style::default().fg(theme.text_primary)),
            ]));
        }
        lines.push(Line::from(vec![
            Span::styled(
                "  Runtime: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                thread_ctx.runtime_summary(),
                Style::default().fg(theme.text_primary),
            ),
        ]));
        if let Some(workflow_summary) = thread_ctx.workflow_summary() {
            lines.push(Line::from(vec![
                Span::styled(
                    "  Workflow: ",
                    Style::default()
                        .fg(theme.text_secondary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(workflow_summary, Style::default().fg(theme.text_primary)),
            ]));
        }
        if let Some(github_summary) = thread_ctx.github_summary() {
            lines.push(Line::from(vec![
                Span::styled(
                    "  GitHub: ",
                    Style::default()
                        .fg(theme.text_secondary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(github_summary, Style::default().fg(theme.text_primary)),
            ]));
        }
        if let Some(preview) = thread_ctx.github_preview() {
            lines.push(Line::from(vec![Span::styled(
                "  Preview:",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            )]));
            for line in preview.lines().take(4) {
                lines.push(Line::from(Span::styled(
                    format!("    {line}"),
                    Style::default().fg(theme.text_primary),
                )));
            }
        }
        lines.push(Line::from(vec![
            Span::styled(
                "  Knowledge: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                thread_ctx.knowledge_summary(),
                Style::default().fg(theme.text_primary),
            ),
        ]));
    }

    // Prompt section
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Prompt",
        Style::default()
            .fg(theme.accent_secondary)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        "  ─".to_string() + &"─".repeat(inner.width.saturating_sub(4) as usize),
        Style::default().fg(theme.text_secondary),
    )));

    if task.description.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no description)",
            Style::default().fg(theme.text_secondary),
        )));
    } else {
        for line in task.description.lines() {
            lines.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::default().fg(theme.text_primary),
            )));
        }
    }

    // Subtask section
    let subtasks = app
        .store
        .list_subtasks_for_task(&task.id)
        .unwrap_or_default();
    if !subtasks.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  Subtasks ({})", subtasks.len()),
            Style::default()
                .fg(theme.accent_secondary)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "  ─".to_string() + &"─".repeat(inner.width.saturating_sub(4) as usize),
            Style::default().fg(theme.text_secondary),
        )));
        for (i, st) in subtasks.iter().enumerate() {
            lines.push(Line::from(Span::styled(
                format!("  {}. {}", i + 1, st.title),
                Style::default().fg(theme.text_primary),
            )));
        }
    }

    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.task_details_scroll, 0))
            .style(theme.overlay_surface())
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn board_item_details_markdown(item: &GitHubBoardItem) -> String {
    let mut text = String::new();

    let _ = std::fmt::Write::write_fmt(
        &mut text,
        format_args!("# {} #{} [{}]\n", item.kind, item.number, item.state),
    );
    let _ = std::fmt::Write::write_fmt(&mut text, format_args!("## {}\n\n", item.title));
    let _ = std::fmt::Write::write_fmt(&mut text, format_args!("- URL: {}\n", item.url));

    if !item.assignees.is_empty() {
        let assignees = item
            .assignees
            .iter()
            .map(|assignee| assignee.login.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = std::fmt::Write::write_fmt(&mut text, format_args!("- Assignees: {assignees}\n"));
    }
    if !item.labels.is_empty() {
        let labels = item
            .labels
            .iter()
            .map(|label| label.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = std::fmt::Write::write_fmt(&mut text, format_args!("- Labels: {labels}\n"));
    }

    if let Some(pr_number) = item.linked_pr_number {
        let _ =
            std::fmt::Write::write_fmt(&mut text, format_args!("- Linked PR: **#{pr_number}**\n"));
    }

    if let Some(fields) = item.project_field_values.as_object()
        && !fields.is_empty()
    {
        text.push_str("\n## Project Fields\n");
        let mut field_entries: Vec<_> = fields
            .iter()
            .filter_map(|(name, value)| value.as_str().map(|value| (name, value)))
            .collect();
        field_entries.sort_by(|left, right| left.0.cmp(right.0));
        for (name, value) in field_entries {
            let _ = std::fmt::Write::write_fmt(&mut text, format_args!("- **{name}**: {value}\n"));
        }
    }

    if let Some(body) = item.body.as_deref()
        && !body.trim().is_empty()
    {
        text.push('\n');
        text.push_str(body);
    } else {
        text.push_str("\n_No issue body cached for this board item._");
    }

    text
}

fn my_task_item_details_markdown(item: &MyTaskItem) -> String {
    let mut text = String::new();
    let kind_label = match item.github_item.kind {
        crate::store::GitHubItemKind::Issue => "Issue",
        crate::store::GitHubItemKind::PullRequest => "Pull request",
        crate::store::GitHubItemKind::ProjectItem => "Project item",
    };
    let _ = std::fmt::Write::write_fmt(
        &mut text,
        format_args!(
            "# {kind_label} #{} [{}]\n",
            item.github_item.number, item.github_item.state
        ),
    );
    let _ =
        std::fmt::Write::write_fmt(&mut text, format_args!("## {}\n\n", item.github_item.title));
    let _ = std::fmt::Write::write_fmt(
        &mut text,
        format_args!("- Repository: {}\n", item.repo.full_name),
    );
    let _ =
        std::fmt::Write::write_fmt(&mut text, format_args!("- URL: {}\n", item.github_item.url));

    if !item.github_item.assignee_logins.is_empty() {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!(
                "- Assignees: {}\n",
                item.github_item.assignee_logins.join(", ")
            ),
        );
    }
    if !item.github_item.label_names.is_empty() {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!("- Labels: {}\n", item.github_item.label_names.join(", ")),
        );
    }
    if let Some(thread) = item.linked_thread.as_ref() {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!("- Thread: {} ({})\n", thread.title, thread.status),
        );
    }

    if let Some(pr) = item.pr_cache.as_ref() {
        if let Some(base) = pr.base_ref.as_deref() {
            let head = pr.head_ref.as_deref().unwrap_or("?");
            let _ =
                std::fmt::Write::write_fmt(&mut text, format_args!("- Branch: {head} -> {base}\n"));
        }
        if let Some(review_decision) = pr.review_decision.as_deref() {
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!("- Review state: {review_decision}\n"),
            );
        }
        if let Some(merge_state) = pr.merge_state_status.as_deref() {
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!("- Merge state: {merge_state}\n"),
            );
        }
    }

    if let Some(fields) = item.github_item.project_field_values.as_object()
        && !fields.is_empty()
    {
        text.push_str("\n## Project Fields\n");
        let mut field_entries: Vec<_> = fields
            .iter()
            .filter_map(|(name, value)| value.as_str().map(|value| (name, value)))
            .collect();
        field_entries.sort_by(|left, right| left.0.cmp(right.0));
        for (name, value) in field_entries {
            let _ = std::fmt::Write::write_fmt(&mut text, format_args!("- **{name}**: {value}\n"));
        }
    }

    let body = item
        .pr_cache
        .as_ref()
        .and_then(|cache| cache.body.as_deref().or(cache.body_text.as_deref()))
        .or_else(|| {
            item.issue_cache
                .as_ref()
                .and_then(|cache| cache.body.as_deref().or(cache.body_text.as_deref()))
        })
        .or(item.github_item.body_text.as_deref());

    if let Some(body) = body
        && !body.trim().is_empty()
    {
        text.push('\n');
        text.push_str(body);
    } else {
        text.push_str("\n_No GitHub body cached for this item._");
    }

    text
}

pub(super) fn draw_help_overlay(frame: &mut Frame, app: &App) {
    let theme = &app.theme;
    let inner = render_modal(
        frame,
        " Help \u{2014} press ? or Esc to close ",
        Style::default().fg(theme.accent_primary),
        60,
        35,
        theme.overlay_surface(),
    );

    let mut lines: Vec<Line<'_>> = Vec::new();
    let groups = app.keymap.help_entries();

    for (i, (section_title, entries)) in groups.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(help_section(section_title, theme));
        for entry in entries {
            lines.push(help_line(entry.label, entry.description, theme));
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn help_section<'a>(title: &'a str, theme: &Theme) -> Line<'a> {
    Line::from(Span::styled(
        format!("  {title}"),
        Style::default()
            .fg(theme.accent_secondary)
            .add_modifier(Modifier::BOLD),
    ))
}

fn help_line<'a>(key: &'a str, desc: &'a str, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("  {key:<14}"),
            Style::default().fg(theme.text_accent),
        ),
        Span::styled(desc, Style::default().fg(theme.text_primary)),
    ])
}

pub(super) fn draw_configure_wizard(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Build content lines from cached status (loaded when overlay was opened)
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(Span::styled(
        " Configure Claude Code Permissions",
        Style::default()
            .fg(app.theme.text_accent)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    let cached = app.cached_config_status.as_ref();

    match cached {
        Some(Ok(status)) => {
            let total_missing: usize = status.diffs.iter().map(|d| d.missing.len()).sum();

            if total_missing == 0 {
                lines.push(Line::from(Span::styled(
                    " ✓ All permissions match recommendations!",
                    Style::default().fg(app.theme.toast_success),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    format!(" {total_missing} missing permission(s) detected"),
                    Style::default()
                        .fg(app.theme.accent_secondary)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));

                // Show recommended (from config.toml)
                let rec = &status.recommended;
                for (label, recommended, color) in [
                    ("allow", &rec.allow, app.theme.toast_success),
                    ("deny", &rec.deny, app.theme.status_error),
                    ("ask", &rec.ask, app.theme.accent_secondary),
                ] {
                    lines.push(Line::from(Span::styled(
                        format!(" {label}:"),
                        Style::default()
                            .fg(app.theme.text_primary)
                            .add_modifier(Modifier::BOLD),
                    )));
                    for perm in recommended {
                        lines.push(Line::from(Span::styled(
                            format!("   {perm}"),
                            Style::default().fg(color),
                        )));
                    }
                }

                lines.push(Line::from(""));

                // Show what's missing per category
                lines.push(Line::from(Span::styled(
                    " Missing:",
                    Style::default()
                        .fg(app.theme.text_primary)
                        .add_modifier(Modifier::BOLD),
                )));

                for diff in &status.diffs {
                    for m in &diff.missing {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("   + {m}"),
                                Style::default().fg(app.theme.toast_success),
                            ),
                            Span::styled(
                                format!("  ({})", diff.category),
                                Style::default().fg(app.theme.text_secondary),
                            ),
                        ]));
                    }
                }
            }
        }
        Some(Err(e)) => {
            lines.push(Line::from(Span::styled(
                format!(" Error loading settings: {e}"),
                Style::default().fg(app.theme.status_error),
            )));
        }
        None => {
            lines.push(Line::from(Span::styled(
                " Loading...",
                Style::default().fg(app.theme.text_secondary),
            )));
        }
    }

    // Hints at bottom
    lines.push(Line::from(""));
    let has_missing = cached.is_some_and(|r| {
        r.as_ref()
            .is_ok_and(|s| s.diffs.iter().any(|d| !d.missing.is_empty()))
    });
    if has_missing {
        lines.push(Line::from(vec![
            Span::styled(
                " a",
                Style::default()
                    .fg(app.theme.text_accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                ":apply all  ",
                Style::default().fg(app.theme.text_secondary),
            ),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(app.theme.text_accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(":close", Style::default().fg(app.theme.text_secondary)),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled(
                " Esc",
                Style::default()
                    .fg(app.theme.text_accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(":close", Style::default().fg(app.theme.text_secondary)),
        ]));
    }

    // Size and position the overlay
    let content_height = u16::try_from(lines.len()).unwrap_or(20) + 2; // +2 for borders
    let width = 60u16.min(area.width.saturating_sub(4));
    let height = content_height.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let overlay_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, overlay_area);

    let block = Block::default()
        .title(" Configure ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.accent_primary));

    let inner = block.inner(overlay_area);
    frame.render_widget(block, overlay_area);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// Draw the GitHub issue create/edit form overlay.
pub(super) fn draw_github_issue_form(frame: &mut Frame, app: &App) {
    let is_edit = app.input_mode == super::super::app::InputMode::EditGitHubIssue;
    let modal_title = if is_edit {
        " Edit Issue "
    } else {
        " New Issue "
    };
    let inner = render_modal(
        frame,
        modal_title,
        Style::default().fg(app.theme.form_border_task),
        64,
        18,
        app.theme.overlay_surface(),
    );

    if inner.height < 8 {
        return;
    }

    let field = app.github_issue_form_field;
    let cursor = app.comment_compose_cursor;

    let render_field = |label: &str, value: &str, active: bool, area: Rect, frame: &mut Frame| {
        let label_style = if active {
            Style::default()
                .fg(app.theme.text_accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.text_secondary)
        };
        let content = if active {
            format_with_cursor(value, cursor)
        } else {
            value.to_string()
        };
        let line = Line::from(vec![
            Span::styled(format!("{label}: "), label_style),
            Span::styled(content, Style::default().fg(app.theme.text_primary)),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    };

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Title
            Constraint::Length(1), // Body
            Constraint::Length(1), // Labels
            Constraint::Length(1), // Assignees
            Constraint::Length(1), // spacer
            Constraint::Length(1), // hints
            Constraint::Min(0),
        ])
        .split(inner);

    render_field(
        "Title",
        &app.github_issue_form_title,
        field == 0,
        sections[0],
        frame,
    );
    render_field(
        "Body",
        &app.github_issue_form_body,
        field == 1,
        sections[1],
        frame,
    );
    render_field(
        "Labels",
        &app.github_issue_form_labels,
        field == 2,
        sections[2],
        frame,
    );
    render_field(
        "Assignees",
        &app.github_issue_form_assignees,
        field == 3,
        sections[3],
        frame,
    );

    let hints = Line::from(vec![
        Span::styled("Tab", Style::default().fg(app.theme.text_accent)),
        Span::styled(":next  ", Style::default().fg(app.theme.text_secondary)),
        Span::styled("Alt+Enter", Style::default().fg(app.theme.text_accent)),
        Span::styled(":submit  ", Style::default().fg(app.theme.text_secondary)),
        Span::styled("Esc", Style::default().fg(app.theme.text_accent)),
        Span::styled(":cancel", Style::default().fg(app.theme.text_secondary)),
    ]);
    frame.render_widget(Paragraph::new(hints), sections[5]);
}

/// Draw a right-aligned side drawer showing issue details, comments, and a compose bar.
/// Draw a right-aligned side drawer with GitHub-style two-column layout.
///
/// Left column: title, body, comments timeline.
/// Right column: metadata sidebar (status, sprint, assignees, labels, project fields).
pub(super) fn draw_board_issue_drawer(frame: &mut Frame, app: &App) {
    let Some(item) = app.selected_board_issue() else {
        return;
    };
    let theme = &app.theme;
    let area = frame.area();

    // Dim the background behind the drawer for visual separation
    let drawer_width = (area.width * 55 / 100)
        .max(50)
        .min(area.width.saturating_sub(16));
    let backdrop_area = Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(drawer_width),
        area.height,
    );
    frame.render_widget(DimOverlay, backdrop_area);

    let drawer_area = Rect::new(
        area.x + area.width - drawer_width,
        area.y,
        drawer_width,
        area.height,
    );

    frame.render_widget(Clear, drawer_area);

    let is_composing = app.input_mode == super::super::app::InputMode::CommentCompose;
    let is_picking_field = app.input_mode == super::super::app::InputMode::FieldPicker;

    // Vertical: title bar + body + optional compose + hints
    let compose_height = if is_composing { 3 } else { 0 };
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // title bar
            Constraint::Min(1),    // two-column body
            Constraint::Length(compose_height),
            Constraint::Length(1), // hints
        ])
        .split(drawer_area);

    // ── Title bar ────────────────────────────────────────────────
    let state_style = if item.state.eq_ignore_ascii_case("open") {
        Style::default()
            .fg(theme.status_working)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme.status_done)
            .add_modifier(Modifier::BOLD)
    };
    let state_badge = format!(" {} ", item.state.to_uppercase());
    let title_line = Line::from(vec![
        Span::styled(state_badge, state_style),
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("{} #{}", item.kind, item.number),
            Style::default()
                .fg(theme.text_accent)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let title_block = Block::default()
        .title(title_line)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if is_composing {
            theme.accent_secondary
        } else {
            theme.accent_primary
        }))
        .style(theme.inspector_surface());
    let title_inner = title_block.inner(outer[0]);
    frame.render_widget(title_block, outer[0]);

    // Truncated issue title inside title bar
    let max_title_w = title_inner.width as usize;
    let title_text = if item.title.len() > max_title_w {
        format!("{}...", &item.title[..max_title_w.saturating_sub(3)])
    } else {
        item.title.clone()
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            title_text,
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        )),
        title_inner,
    );

    // ── Two-column body ──────────────────────────────────────────
    let meta_width = 28u16.min(outer[1].width / 3);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(meta_width)])
        .split(outer[1]);

    // Left column: body + comments with padding
    let left_block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(theme.border_unfocused))
        .style(theme.inspector_surface());
    let left_inner = left_block.inner(cols[0]);
    frame.render_widget(left_block, cols[0]);

    // Inset for padding (3 cells left, 2 right, 1 top)
    let padded = Rect {
        x: left_inner.x.saturating_add(3),
        y: left_inner.y.saturating_add(1),
        width: left_inner.width.saturating_sub(5),
        height: left_inner.height.saturating_sub(1),
    };

    let mut body_md = String::new();

    // Show linked PR banner if this issue has one
    if item.kind == crate::store::GitHubItemKind::Issue
        && let Some(pr_number) = item.linked_pr_number
    {
        let _ = std::fmt::Write::write_fmt(
            &mut body_md,
            format_args!("**Linked PR** #{pr_number}\n\n"),
        );
    }

    if let Some(body) = item.body.as_deref() {
        body_md.push_str(body);
    } else {
        body_md.push_str("_No description._");
    }

    if !app.board_selected_comments.is_empty() {
        body_md.push_str("\n\n---\n");
        let _ = std::fmt::Write::write_fmt(
            &mut body_md,
            format_args!("### Comments ({})\n", app.board_selected_comments.len()),
        );
        for comment in &app.board_selected_comments {
            let author = comment.author_login.as_deref().unwrap_or("unknown");
            let date = &comment.created_at;
            let _ = std::fmt::Write::write_fmt(
                &mut body_md,
                format_args!("\n**@{author}** — {date}\n"),
            );
            if let Some(body) = comment.body_text.as_deref().or(comment.body.as_deref()) {
                body_md.push('\n');
                body_md.push_str(body);
                body_md.push('\n');
            }
        }
    }

    frame.render_widget(
        Paragraph::new(markdown_from_str(&body_md))
            .scroll((app.task_details_scroll, 0))
            .style(theme.inspector_surface())
            .wrap(Wrap { trim: false }),
        padded,
    );

    // Right column: metadata sidebar with card-like depth
    let meta_block = Block::default()
        .borders(Borders::NONE)
        .style(theme.card_surface());
    let meta_inner = Rect {
        x: cols[1].x.saturating_add(1),
        y: cols[1].y,
        width: cols[1].width.saturating_sub(1),
        height: cols[1].height,
    };
    frame.render_widget(meta_block, cols[1]);
    draw_issue_metadata_sidebar(frame, theme, &item, meta_inner, is_picking_field);

    // ── Compose bar ──────────────────────────────────────────────
    if is_composing {
        let compose_text = super::super::form::format_with_cursor(
            &app.comment_compose_buffer,
            app.comment_compose_cursor,
        );
        let compose_block = Block::default()
            .title(Span::styled(
                " Comment (Enter to send, Esc to cancel) ",
                Style::default()
                    .fg(theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent_primary))
            .style(theme.inspector_surface());
        let compose_inner = compose_block.inner(outer[2]);
        frame.render_widget(compose_block, outer[2]);
        frame.render_widget(
            Paragraph::new(compose_text).style(Style::default().fg(theme.text_primary)),
            compose_inner,
        );
    }

    // ── Field picker overlay ─────────────────────────────────────
    if is_picking_field {
        draw_field_picker_overlay(frame, app, drawer_area);
    }

    // ── Hint line ────────────────────────────────────────────────
    let hints = Line::from(vec![
        Span::styled(" L", Style::default().fg(theme.text_accent)),
        Span::styled(":launch  ", Style::default().fg(theme.text_secondary)),
        Span::styled("j/k", Style::default().fg(theme.text_accent)),
        Span::styled(":scroll  ", Style::default().fg(theme.text_secondary)),
        Span::styled("s", Style::default().fg(theme.text_accent)),
        Span::styled(":status  ", Style::default().fg(theme.text_secondary)),
        Span::styled("c", Style::default().fg(theme.text_accent)),
        Span::styled(":comment  ", Style::default().fg(theme.text_secondary)),
        Span::styled("x", Style::default().fg(theme.text_accent)),
        Span::styled(":close  ", Style::default().fg(theme.text_secondary)),
        Span::styled("o", Style::default().fg(theme.text_accent)),
        Span::styled(":open  ", Style::default().fg(theme.text_secondary)),
        Span::styled("Esc", Style::default().fg(theme.text_accent)),
        Span::styled(":back", Style::default().fg(theme.text_secondary)),
    ]);
    frame.render_widget(
        Paragraph::new(hints).style(theme.inspector_surface()),
        outer[3],
    );
}

/// Render the right-side metadata sidebar (GitHub-style).
fn draw_issue_metadata_sidebar(
    frame: &mut Frame,
    theme: &Theme,
    item: &GitHubBoardItem,
    area: Rect,
    _is_picking: bool,
) {
    let surface = theme.inspector_surface();
    let label_style = Style::default().fg(theme.text_secondary);
    let value_style = Style::default().fg(theme.text_primary);
    let accent_style = Style::default()
        .fg(theme.text_accent)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Assignees
    lines.push(Line::from(Span::styled(" Assignees", accent_style)));
    if item.assignees.is_empty() {
        lines.push(Line::from(Span::styled("  none", label_style)));
    } else {
        for assignee in &item.assignees {
            lines.push(Line::from(Span::styled(
                format!("  @{}", assignee.login),
                value_style,
            )));
        }
    }
    lines.push(Line::from(""));

    // Labels
    lines.push(Line::from(Span::styled(" Labels", accent_style)));
    if item.labels.is_empty() {
        lines.push(Line::from(Span::styled("  none", label_style)));
    } else {
        for label in &item.labels {
            let pill_style = theme.label_pill_style(label.color.as_deref());
            lines.push(Line::from(vec![
                Span::styled("  ", surface),
                Span::styled(format!(" {} ", label.name), pill_style),
            ]));
        }
    }
    lines.push(Line::from(""));

    // Project fields
    if let Some(fields) = item.project_field_values.as_object() {
        let mut sorted: Vec<_> = fields
            .iter()
            .filter_map(|(name, value)| value.as_str().map(|v| (name.as_str(), v)))
            .collect();
        sorted.sort_by_key(|(name, _)| *name);

        for (name, value) in sorted {
            let is_status = name.eq_ignore_ascii_case("status");
            lines.push(Line::from(Span::styled(
                format!(" {name}"),
                if is_status {
                    Style::default()
                        .fg(theme.accent_secondary)
                        .add_modifier(Modifier::BOLD)
                } else {
                    accent_style
                },
            )));
            let suffix = if is_status { " [s]" } else { "" };
            lines.push(Line::from(Span::styled(
                format!("  {value}{suffix}"),
                value_style,
            )));
            lines.push(Line::from(""));
        }
    }

    // Linked PR (for issues that have a PR referencing them)
    if item.kind == crate::store::GitHubItemKind::Issue {
        if let Some(pr_number) = item.linked_pr_number {
            lines.push(Line::from(Span::styled(
                " Linked PR",
                Style::default()
                    .fg(theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                format!("  PR #{pr_number}"),
                Style::default().fg(theme.pr_link),
            )));
            lines.push(Line::from(""));
        }
    } else if item.kind == crate::store::GitHubItemKind::PullRequest {
        // Show branch info for PRs
        lines.push(Line::from(Span::styled(
            " Pull Request",
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!("  #{}", item.number),
            Style::default().fg(theme.pr_link),
        )));
        lines.push(Line::from(""));
    }

    // GitHub link
    let link_label = match item.kind {
        crate::store::GitHubItemKind::Issue => {
            format!("  Open Issue #{} on GitHub", item.number)
        }
        crate::store::GitHubItemKind::PullRequest => {
            format!("  Open PR #{} on GitHub", item.number)
        }
        crate::store::GitHubItemKind::ProjectItem => "  Open on GitHub".to_string(),
    };
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            link_label,
            Style::default()
                .fg(theme.pr_link)
                .add_modifier(Modifier::UNDERLINED),
        ),
        Span::styled("  [o]", Style::default().fg(theme.text_secondary)),
    ]));

    frame.render_widget(
        Paragraph::new(lines)
            .style(surface)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Draw the field picker overlay (for Status, Sprint, etc.)
pub(super) fn draw_my_task_drawer(frame: &mut Frame, app: &App) {
    let Some(item) = app.selected_my_task_item() else {
        return;
    };
    let theme = &app.theme;
    let area = frame.area();

    // Dim the background behind the drawer for visual separation
    let drawer_width = (area.width * 55 / 100)
        .max(50)
        .min(area.width.saturating_sub(16));
    let backdrop_area = Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(drawer_width),
        area.height,
    );
    frame.render_widget(DimOverlay, backdrop_area);

    let drawer_area = Rect::new(
        area.x + area.width - drawer_width,
        area.y,
        drawer_width,
        area.height,
    );

    frame.render_widget(Clear, drawer_area);

    let is_composing = app.input_mode == super::super::app::InputMode::CommentCompose;

    // Vertical: title bar + body + optional compose + hints
    let compose_height = if is_composing { 3 } else { 0 };
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // title bar
            Constraint::Min(1),    // two-column body
            Constraint::Length(compose_height),
            Constraint::Length(1), // hints
        ])
        .split(drawer_area);

    // ── Title bar ────────────────────────────────────────────────
    let state_style = if item.github_item.state.eq_ignore_ascii_case("open") {
        Style::default()
            .fg(theme.status_working)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme.status_done)
            .add_modifier(Modifier::BOLD)
    };
    let state_badge = format!(" {} ", item.github_item.state.to_uppercase());
    let kind_label = match item.github_item.kind {
        crate::store::GitHubItemKind::Issue => "Issue",
        crate::store::GitHubItemKind::PullRequest => "PR",
        crate::store::GitHubItemKind::ProjectItem => "Item",
    };
    let title_line = Line::from(vec![
        Span::styled(state_badge, state_style),
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("{kind_label} #{}", item.github_item.number),
            Style::default()
                .fg(theme.text_accent)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let title_block = Block::default()
        .title(title_line)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if is_composing {
            theme.accent_secondary
        } else {
            theme.accent_primary
        }))
        .style(theme.inspector_surface());
    let title_inner = title_block.inner(outer[0]);
    frame.render_widget(title_block, outer[0]);

    // Truncated issue title inside title bar
    let max_title_w = title_inner.width as usize;
    let title_text = if item.github_item.title.len() > max_title_w {
        format!(
            "{}...",
            &item.github_item.title[..max_title_w.saturating_sub(3)]
        )
    } else {
        item.github_item.title.clone()
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            title_text,
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        )),
        title_inner,
    );

    // ── Two-column body ──────────────────────────────────────────
    let meta_width = 28u16.min(outer[1].width / 3);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(meta_width)])
        .split(outer[1]);

    // Left column: body + comments with proper padding
    let left_block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(theme.border_unfocused))
        .style(theme.inspector_surface());
    let left_inner = left_block.inner(cols[0]);
    frame.render_widget(left_block, cols[0]);

    // Inset for padding (3 cells left, 2 right, 1 top)
    let padded = Rect {
        x: left_inner.x.saturating_add(3),
        y: left_inner.y.saturating_add(1),
        width: left_inner.width.saturating_sub(5),
        height: left_inner.height.saturating_sub(1),
    };

    let mut body_md = String::new();

    // Show linked PR banner at the top if this issue has one
    if item.github_item.kind == crate::store::GitHubItemKind::Issue {
        if let Some(linked_pr) = item.linked_pr.as_ref() {
            // Use the pre-fetched linked PR from the store
            let pr_cache = app.store.get_github_pr_cache(&linked_pr.id).ok();
            let pr_branch = pr_cache
                .as_ref()
                .and_then(|pr| pr.head_ref.as_deref().map(|h| format!(" `{h}`")))
                .unwrap_or_default();
            let _ = std::fmt::Write::write_fmt(
                &mut body_md,
                format_args!("**Linked PR** #{}{pr_branch}\n\n", linked_pr.number),
            );
        } else if let Some(pr_number) = item.github_item.linked_pr_number {
            let _ = std::fmt::Write::write_fmt(
                &mut body_md,
                format_args!("**Linked PR** #{pr_number}\n\n"),
            );
        }
    }

    // Show existing thread info if there is one
    if let Some(thread) = item.linked_thread.as_ref() {
        let _ = std::fmt::Write::write_fmt(
            &mut body_md,
            format_args!("**Active Thread** {} [{}]\n\n", thread.title, thread.status),
        );
    }

    // Use issue_cache or pr_cache body, falling back to github_item body_text
    let body_text = item
        .issue_cache
        .as_ref()
        .and_then(|c| c.body_text.as_deref().or(c.body.as_deref()))
        .or_else(|| {
            item.pr_cache
                .as_ref()
                .and_then(|c| c.body_text.as_deref().or(c.body.as_deref()))
        })
        .or(item.github_item.body_text.as_deref());
    if let Some(body) = body_text {
        body_md.push_str(body);
    } else {
        body_md.push_str("_No description._");
    }

    if !app.my_task_selected_comments.is_empty() {
        body_md.push_str("\n\n---\n");
        let _ = std::fmt::Write::write_fmt(
            &mut body_md,
            format_args!("### Comments ({})\n", app.my_task_selected_comments.len()),
        );
        for comment in &app.my_task_selected_comments {
            let author = comment.author_login.as_deref().unwrap_or("unknown");
            let date = &comment.created_at;
            let _ = std::fmt::Write::write_fmt(
                &mut body_md,
                format_args!("\n**@{author}** — {date}\n"),
            );
            if let Some(body) = comment.body_text.as_deref().or(comment.body.as_deref()) {
                body_md.push('\n');
                body_md.push_str(body);
                body_md.push('\n');
            }
        }
    }

    frame.render_widget(
        Paragraph::new(markdown_from_str(&body_md))
            .scroll((app.task_details_scroll, 0))
            .style(theme.inspector_surface())
            .wrap(Wrap { trim: false }),
        padded,
    );

    // Right column: metadata sidebar with card-like background
    let meta_block = Block::default()
        .borders(Borders::NONE)
        .style(theme.card_surface());
    let meta_inner = Rect {
        x: cols[1].x.saturating_add(1),
        y: cols[1].y,
        width: cols[1].width.saturating_sub(1),
        height: cols[1].height,
    };
    frame.render_widget(meta_block, cols[1]);
    draw_my_task_metadata_sidebar(frame, theme, item, meta_inner);

    // ── Compose bar ──────────────────────────────────────────────
    if is_composing {
        let compose_text = super::super::form::format_with_cursor(
            &app.comment_compose_buffer,
            app.comment_compose_cursor,
        );
        let compose_block = Block::default()
            .title(Span::styled(
                " Comment (Enter to send, Esc to cancel) ",
                Style::default()
                    .fg(theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent_primary))
            .style(theme.inspector_surface());
        let compose_inner = compose_block.inner(outer[2]);
        frame.render_widget(compose_block, outer[2]);
        frame.render_widget(
            Paragraph::new(compose_text).style(Style::default().fg(theme.text_primary)),
            compose_inner,
        );
    }

    // ── Hint line ────────────────────────────────────────────────
    let has_thread = item.linked_thread.is_some();
    let launch_hint = if has_thread {
        ":resume thread  "
    } else {
        ":launch thread  "
    };
    let hints = Line::from(vec![
        Span::styled(" j/k", Style::default().fg(theme.text_accent)),
        Span::styled(":scroll  ", Style::default().fg(theme.text_secondary)),
        Span::styled("l", Style::default().fg(theme.text_accent)),
        Span::styled(launch_hint, Style::default().fg(theme.text_secondary)),
        Span::styled("c", Style::default().fg(theme.text_accent)),
        Span::styled(":comment  ", Style::default().fg(theme.text_secondary)),
        Span::styled("o", Style::default().fg(theme.text_accent)),
        Span::styled(":open  ", Style::default().fg(theme.text_secondary)),
        Span::styled("Esc", Style::default().fg(theme.text_accent)),
        Span::styled(":back", Style::default().fg(theme.text_secondary)),
    ]);
    frame.render_widget(
        Paragraph::new(hints).style(theme.inspector_surface()),
        outer[3],
    );
}

/// Render the right-side metadata sidebar for a My Task item.
fn draw_my_task_metadata_sidebar(frame: &mut Frame, theme: &Theme, item: &MyTaskItem, area: Rect) {
    let surface = theme.inspector_surface();
    let label_style = Style::default().fg(theme.text_secondary);
    let value_style = Style::default().fg(theme.text_primary);
    let accent_style = Style::default()
        .fg(theme.text_accent)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Assignees
    lines.push(Line::from(Span::styled(" Assignees", accent_style)));
    if item.github_item.assignee_logins.is_empty() {
        lines.push(Line::from(Span::styled("  none", label_style)));
    } else {
        for login in &item.github_item.assignee_logins {
            lines.push(Line::from(Span::styled(format!("  @{login}"), value_style)));
        }
    }
    lines.push(Line::from(""));

    // Labels
    lines.push(Line::from(Span::styled(" Labels", accent_style)));
    if item.github_item.label_names.is_empty() {
        lines.push(Line::from(Span::styled("  none", label_style)));
    } else {
        let color_map = item.github_item.label_colors.as_object();
        for name in &item.github_item.label_names {
            let hex = color_map
                .and_then(|m| m.get(name))
                .and_then(serde_json::Value::as_str);
            let pill_style = theme.label_pill_style(hex);
            lines.push(Line::from(vec![
                Span::styled("  ", surface),
                Span::styled(format!(" {name} "), pill_style),
            ]));
        }
    }
    lines.push(Line::from(""));

    // Project fields
    if let Some(fields) = item.github_item.project_field_values.as_object() {
        let mut sorted: Vec<_> = fields
            .iter()
            .filter_map(|(name, value)| value.as_str().map(|v| (name.as_str(), v)))
            .collect();
        sorted.sort_by_key(|(name, _)| *name);

        for (name, value) in sorted {
            lines.push(Line::from(Span::styled(format!(" {name}"), accent_style)));
            lines.push(Line::from(Span::styled(format!("  {value}"), value_style)));
            lines.push(Line::from(""));
        }
    }

    // Linked PR (for issues that have a PR referencing them)
    let linked_pr_number = item
        .linked_pr
        .as_ref()
        .map(|pr| pr.number)
        .or(item.github_item.linked_pr_number);
    if item.github_item.kind == crate::store::GitHubItemKind::Issue
        && let Some(pr_number) = linked_pr_number
    {
        lines.push(Line::from(Span::styled(
            " Linked PR",
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!("  PR #{pr_number}"),
            Style::default().fg(theme.pr_link),
        )));
        lines.push(Line::from(""));
    }

    // PR info
    if let Some(pr) = item.pr_cache.as_ref() {
        if let Some(base) = pr.base_ref.as_deref() {
            let head = pr.head_ref.as_deref().unwrap_or("?");
            lines.push(Line::from(Span::styled(" Branch", accent_style)));
            lines.push(Line::from(Span::styled(
                format!("  {head} \u{2192} {base}"),
                value_style,
            )));
            lines.push(Line::from(""));
        }
        if let Some(review_decision) = pr.review_decision.as_deref() {
            lines.push(Line::from(Span::styled(" Review", accent_style)));
            lines.push(Line::from(Span::styled(
                format!("  {review_decision}"),
                value_style,
            )));
            lines.push(Line::from(""));
        }
        if let Some(merge_state) = pr.merge_state_status.as_deref() {
            lines.push(Line::from(Span::styled(" Merge state", accent_style)));
            lines.push(Line::from(Span::styled(
                format!("  {merge_state}"),
                value_style,
            )));
            lines.push(Line::from(""));
        }
    }

    // Thread link
    if let Some(thread) = item.linked_thread.as_ref() {
        lines.push(Line::from(Span::styled(" Thread", accent_style)));
        lines.push(Line::from(Span::styled(
            format!("  {} ({})", thread.title, thread.status),
            value_style,
        )));
        lines.push(Line::from(""));
    }

    // Repository
    lines.push(Line::from(Span::styled(" Repository", accent_style)));
    lines.push(Line::from(Span::styled(
        format!("  {}", item.repo.full_name),
        value_style,
    )));
    lines.push(Line::from(""));

    // GitHub link — show as friendly label, not raw URL
    let link_label = match item.github_item.kind {
        crate::store::GitHubItemKind::Issue => {
            format!("  Open Issue #{} on GitHub", item.github_item.number)
        }
        crate::store::GitHubItemKind::PullRequest => {
            format!("  Open PR #{} on GitHub", item.github_item.number)
        }
        crate::store::GitHubItemKind::ProjectItem => "  Open on GitHub".to_string(),
    };
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            link_label,
            Style::default()
                .fg(theme.pr_link)
                .add_modifier(Modifier::UNDERLINED),
        ),
        Span::styled("  [o]", Style::default().fg(theme.text_secondary)),
    ]));

    frame.render_widget(
        Paragraph::new(lines)
            .style(surface)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_field_picker_overlay(frame: &mut Frame, app: &App, parent_area: Rect) {
    let theme = &app.theme;
    let options = &app.field_picker_options;
    if options.is_empty() {
        return;
    }

    let height = (options.len() as u16 + 2).clamp(4, 16);
    let width = 36u16.min(parent_area.width.saturating_sub(4));
    let x = parent_area.x + (parent_area.width.saturating_sub(width)) / 2;
    let y = parent_area.y + (parent_area.height.saturating_sub(height)) / 2;
    let picker_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, picker_area);

    let title = app.field_picker_field_name.as_deref().unwrap_or("Select");
    let block = Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(theme.accent_secondary)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent_secondary))
        .style(theme.overlay_surface());
    let inner = block.inner(picker_area);
    frame.render_widget(block, picker_area);

    let items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let is_selected = i == app.field_picker_index;
            let prefix = if is_selected { "\u{25b8} " } else { "  " };
            let style = if is_selected {
                theme.selected_fill()
            } else {
                Style::default().fg(theme.text_primary)
            };
            ListItem::new(Line::from(Span::styled(
                format!("{prefix}{}", opt.name),
                style,
            )))
        })
        .collect();

    frame.render_widget(List::new(items).style(theme.overlay_surface()), inner);
}

pub(super) fn draw_review_drawer(frame: &mut Frame, app: &mut App) {
    let Some(review_item) = app.selected_review_item().cloned() else {
        return;
    };
    let area = frame.area();

    // Drawer takes ~65% of screen width, right-aligned, full height
    let drawer_width = (area.width * 65 / 100)
        .max(60)
        .min(area.width.saturating_sub(16));

    // Dim the background behind the drawer
    let backdrop_area = Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(drawer_width),
        area.height,
    );
    frame.render_widget(DimOverlay, backdrop_area);

    let drawer_area = Rect::new(
        area.x + area.width - drawer_width,
        area.y,
        drawer_width,
        area.height,
    );

    frame.render_widget(Clear, drawer_area);

    let is_composing = app.input_mode == super::super::app::InputMode::CommentCompose;
    let drawer_tab = app.review_drawer_tab;

    // Vertical: title bar + tab bar + body + optional compose + hints
    let compose_height = if is_composing { 3 } else { 0 };
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // title bar
            Constraint::Length(1), // tab bar
            Constraint::Min(1),    // body
            Constraint::Length(compose_height),
            Constraint::Length(1), // hints
        ])
        .split(drawer_area);

    // ── Title bar ────────────────────────────────────────────────
    {
        let theme = &app.theme;
        let state_style = Style::default()
            .fg(theme.status_working)
            .add_modifier(Modifier::BOLD);
        let state_badge = format!(" PR #{} ", review_item.github_item.number);
        let title_line = Line::from(vec![
            Span::styled(state_badge, state_style),
            Span::styled(" ", Style::default()),
            Span::styled(
                review_item
                    .pr_cache
                    .review_decision
                    .as_deref()
                    .unwrap_or("open")
                    .replace('_', " ")
                    .to_uppercase(),
                Style::default()
                    .fg(theme.text_accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        let title_block = Block::default()
            .title(title_line)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if is_composing {
                theme.accent_secondary
            } else {
                theme.accent_primary
            }))
            .style(theme.inspector_surface());
        let title_inner = title_block.inner(outer[0]);
        frame.render_widget(title_block, outer[0]);

        let max_title_w = title_inner.width as usize;
        let title_text = if review_item.github_item.title.len() > max_title_w {
            format!(
                "{}...",
                &review_item.github_item.title[..max_title_w.saturating_sub(3)]
            )
        } else {
            review_item.github_item.title.clone()
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                title_text,
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            )),
            title_inner,
        );
    }

    // ── Tab bar ────────────────────────────────────────────────
    {
        let theme = &app.theme;
        let tabs: Vec<Span<'_>> = ReviewDrawerTab::ALL
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let label = format!(" {}:{} ", index + 1, tab.label());
                if *tab == drawer_tab {
                    Span::styled(label, theme.chip_style(theme.tab_active))
                } else {
                    Span::styled(label, Style::default().fg(theme.text_secondary))
                }
            })
            .collect();
        frame.render_widget(
            Paragraph::new(Line::from(tabs)).style(theme.inspector_surface()),
            outer[1],
        );
    }

    // ── Body (may mutate app for diff caching) ──────────────────
    let body_area = outer[2];
    match drawer_tab {
        ReviewDrawerTab::Description => {
            draw_review_drawer_description(frame, app, &review_item, body_area);
        }
        ReviewDrawerTab::Diff => {
            draw_review_drawer_diff(frame, app, &review_item, body_area);
        }
        ReviewDrawerTab::Comments => {
            draw_review_drawer_comments(frame, app, body_area);
        }
    }

    // ── Compose bar ──────────────────────────────────────────────
    let theme = &app.theme;
    if is_composing {
        let compose_text =
            format_with_cursor(&app.comment_compose_buffer, app.comment_compose_cursor);
        let compose_block = Block::default()
            .title(Span::styled(
                " Comment (Enter to send, Esc to cancel) ",
                Style::default()
                    .fg(theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent_primary))
            .style(theme.inspector_surface());
        let compose_inner = compose_block.inner(outer[3]);
        frame.render_widget(compose_block, outer[3]);
        frame.render_widget(
            Paragraph::new(compose_text).style(Style::default().fg(theme.text_primary)),
            compose_inner,
        );
    }

    // ── Hint line ────────────────────────────────────────────────
    let hints = Line::from(vec![
        Span::styled(" 1/2/3", Style::default().fg(theme.text_accent)),
        Span::styled(":tab  ", Style::default().fg(theme.text_secondary)),
        Span::styled("j/k", Style::default().fg(theme.text_accent)),
        Span::styled(":scroll  ", Style::default().fg(theme.text_secondary)),
        Span::styled("l", Style::default().fg(theme.text_accent)),
        Span::styled(":launch  ", Style::default().fg(theme.text_secondary)),
        Span::styled("c", Style::default().fg(theme.text_accent)),
        Span::styled(":comment  ", Style::default().fg(theme.text_secondary)),
        Span::styled("o", Style::default().fg(theme.text_accent)),
        Span::styled(":open  ", Style::default().fg(theme.text_secondary)),
        Span::styled("Esc", Style::default().fg(theme.text_accent)),
        Span::styled(":back", Style::default().fg(theme.text_secondary)),
    ]);
    frame.render_widget(
        Paragraph::new(hints).style(theme.inspector_surface()),
        outer[4],
    );
}

fn draw_review_drawer_description(
    frame: &mut Frame,
    app: &App,
    review_item: &super::super::app::ReviewQueueItem,
    area: Rect,
) {
    let theme = &app.theme;
    // Two-column layout: body | metadata
    let meta_width = 28u16.min(area.width / 3);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(meta_width)])
        .split(area);

    // Left: PR description
    let left_block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(theme.border_unfocused))
        .style(theme.inspector_surface());
    let left_inner = left_block.inner(cols[0]);
    frame.render_widget(left_block, cols[0]);

    let mut body_md = String::new();
    if let Some(author) = review_item.author_login.as_deref() {
        let _ = std::fmt::Write::write_fmt(&mut body_md, format_args!("**@{author}**\n\n"));
    }
    if let (Some(head), Some(base)) = (
        review_item.pr_cache.head_ref.as_deref(),
        review_item.pr_cache.base_ref.as_deref(),
    ) {
        let _ = std::fmt::Write::write_fmt(&mut body_md, format_args!("`{head}` → `{base}`\n\n"));
    }
    let body = review_item
        .pr_cache
        .body
        .as_deref()
        .or(review_item.pr_cache.body_text.as_deref())
        .or(review_item
            .issue_cache
            .as_ref()
            .and_then(|c| c.body.as_deref()))
        .or(review_item
            .issue_cache
            .as_ref()
            .and_then(|c| c.body_text.as_deref()));
    if let Some(body) = body {
        body_md.push_str(body);
    } else {
        body_md.push_str("_No description._");
    }

    frame.render_widget(
        Paragraph::new(markdown_from_str(&body_md))
            .scroll((app.review_drawer_scroll, 0))
            .style(theme.inspector_surface())
            .wrap(Wrap { trim: false }),
        left_inner,
    );

    // Right: metadata
    draw_review_drawer_metadata(frame, theme, review_item, cols[1]);
}

fn draw_review_drawer_metadata(
    frame: &mut Frame,
    theme: &Theme,
    review_item: &super::super::app::ReviewQueueItem,
    area: Rect,
) {
    let surface = theme.inspector_surface();
    let label_style = Style::default().fg(theme.text_secondary);
    let value_style = Style::default().fg(theme.text_primary);
    let accent_style = Style::default()
        .fg(theme.text_accent)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Author
    lines.push(Line::from(Span::styled(" Author", accent_style)));
    if let Some(author) = review_item.author_login.as_deref() {
        lines.push(Line::from(Span::styled(
            format!("  @{author}"),
            value_style,
        )));
    } else {
        lines.push(Line::from(Span::styled("  unknown", label_style)));
    }
    lines.push(Line::from(""));

    // Review decision
    lines.push(Line::from(Span::styled(" Review State", accent_style)));
    let decision = review_item
        .pr_cache
        .review_decision
        .as_deref()
        .unwrap_or("open")
        .replace('_', " ");
    lines.push(Line::from(Span::styled(
        format!("  {decision}"),
        value_style,
    )));
    if review_item.pr_cache.is_draft {
        lines.push(Line::from(Span::styled("  (draft)", label_style)));
    }
    lines.push(Line::from(""));

    // Requested reviewers
    if !review_item.requested_reviewer_logins.is_empty() {
        lines.push(Line::from(Span::styled(" Reviewers", accent_style)));
        for login in &review_item.requested_reviewer_logins {
            lines.push(Line::from(Span::styled(format!("  @{login}"), value_style)));
        }
        lines.push(Line::from(""));
    }

    // Activity counts
    lines.push(Line::from(Span::styled(" Activity", accent_style)));
    lines.push(Line::from(Span::styled(
        format!("  {} comments", review_item.comment_count),
        value_style,
    )));
    lines.push(Line::from(Span::styled(
        format!("  {} reviews", review_item.review_count),
        value_style,
    )));
    lines.push(Line::from(Span::styled(
        format!("  {} inline", review_item.review_comment_count),
        value_style,
    )));

    frame.render_widget(Paragraph::new(lines).style(surface), area);
}

fn draw_review_drawer_diff(
    frame: &mut Frame,
    app: &mut App,
    review_item: &super::super::app::ReviewQueueItem,
    area: Rect,
) {
    let theme = &app.theme;

    // Load diff (reuse the cache mechanism)
    let key = format!(
        "review-drawer:{}:{}",
        review_item.github_item.id, review_item.pr_cache.cached_at
    );
    let cache_expired = app
        .diff_preview_generated_at
        .is_none_or(|instant| instant.elapsed() >= std::time::Duration::from_secs(2));
    if app.diff_preview_cache_key.as_deref() != Some(key.as_str()) || cache_expired {
        app.diff_preview_cache =
            super::workbench::load_remote_pr_diff_preview_full(&app.store, review_item);
        app.diff_preview_cache_key = Some(key);
        app.diff_preview_generated_at = Some(std::time::Instant::now());
    }

    let styled_lines = styled_diff_lines(&app.diff_preview_cache, theme);
    frame.render_widget(
        Paragraph::new(styled_lines)
            .style(theme.inspector_surface())
            .scroll((app.review_drawer_scroll, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_review_drawer_comments(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let mut text = String::new();

    // Top-level reviews
    if !app.review_selected_reviews.is_empty() {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!("### Reviews ({})\n\n", app.review_selected_reviews.len()),
        );
        for review in &app.review_selected_reviews {
            let author = review.author_login.as_deref().unwrap_or("unknown");
            let state = &review.state;
            let date = review.submitted_at.as_deref().unwrap_or("");
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!("**@{author}** ({state}) — {date}\n"),
            );
            if let Some(body) = review.body_text.as_deref().or(review.body.as_deref())
                && !body.is_empty()
            {
                text.push_str(body);
                text.push('\n');
            }
            text.push('\n');
        }
    }

    // Top-level comments
    if !app.review_selected_comments.is_empty() {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!("### Comments ({})\n\n", app.review_selected_comments.len()),
        );
        for comment in &app.review_selected_comments {
            let author = comment.author_login.as_deref().unwrap_or("unknown");
            let date = &comment.created_at;
            let _ = std::fmt::Write::write_fmt(&mut text, format_args!("**@{author}** — {date}\n"));
            if let Some(body) = comment.body_text.as_deref().or(comment.body.as_deref()) {
                text.push_str(body);
                text.push('\n');
            }
            text.push('\n');
        }
    }

    // Inline review comments grouped by file
    if !app.review_selected_review_comments.is_empty() {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!(
                "### Inline Comments ({})\n\n",
                app.review_selected_review_comments.len()
            ),
        );
        for rc in &app.review_selected_review_comments {
            let author = rc.author_login.as_deref().unwrap_or("unknown");
            let path = rc.path.as_deref().unwrap_or("unknown");
            let line_info = rc
                .line
                .map_or_else(String::new, |line| format!(" line {line}"));
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!("**@{author}** on `{path}`{line_info}\n"),
            );
            if let Some(hunk) = &rc.diff_hunk {
                let _ = std::fmt::Write::write_fmt(
                    &mut text,
                    format_args!(
                        "> {}\n",
                        hunk.lines().take(3).collect::<Vec<_>>().join("\n> ")
                    ),
                );
            }
            if let Some(body) = rc.body_text.as_deref().or(rc.body.as_deref()) {
                text.push_str(body);
                text.push('\n');
            }
            text.push('\n');
        }
    }

    if text.is_empty() {
        text.push_str("_No comments or reviews cached for this PR._");
    }

    frame.render_widget(
        Paragraph::new(markdown_from_str(&text))
            .scroll((app.review_drawer_scroll, 0))
            .style(theme.inspector_surface())
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Syntax-highlight diff lines: +green, -red, @@cyan, headers bold.
pub(super) fn styled_diff_lines<'a>(raw: &str, theme: &Theme) -> Vec<Line<'a>> {
    raw.lines()
        .map(|line| {
            if line.starts_with('+') && !line.starts_with("+++") {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(theme.status_done),
                ))
            } else if line.starts_with('-') && !line.starts_with("---") {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(theme.status_error),
                ))
            } else if line.starts_with("@@") {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(theme.accent_secondary),
                ))
            } else if line.starts_with("diff --git")
                || line.starts_with("---")
                || line.starts_with("+++")
            {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(line.to_string())
            }
        })
        .collect()
}
