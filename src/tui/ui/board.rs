//! Sprint board rendering -- Kanban columns showing GitHub issues.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use serde_json::Value;

use crate::github::GitHubBoardItem;

use super::super::app::{App, BoardScope, Focus, InputMode};
use super::super::theme::Theme;

/// Draw the sprint board view within the given area.
/// Shows issues grouped into Kanban columns.
pub(super) fn draw_board(frame: &mut Frame, app: &App, area: Rect) {
    let show_filter_bar = app.input_mode == InputMode::BoardFilter || !app.board_filter.is_empty();

    if show_filter_bar {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
        draw_board_header(frame, app, layout[0]);
        draw_filter_bar(frame, app, layout[1]);
        draw_board_columns(frame, app, layout[2]);
    } else {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(area);
        draw_board_header(frame, app, layout[0]);
        draw_board_columns(frame, app, layout[1]);
    }
}

fn draw_board_header(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;

    let github_project_text = app.board_project_title.as_deref().map_or_else(
        || "Board: none".to_string(),
        |project| format!("Board: {project}"),
    );
    let sprint_text = app.board_sprint_filter.as_deref().map_or_else(
        || "Sprint: All items".to_string(),
        |sprint| format!("Sprint: {sprint}"),
    );

    let total_issues: usize = app.board_issues.iter().map(Vec::len).sum();

    let header = Line::from(vec![
        Span::styled(
            format!(" {github_project_text} "),
            Style::default().fg(theme.text_primary),
        ),
        Span::styled(" \u{2502} ", Style::default().fg(theme.border_unfocused)),
        Span::styled(&sprint_text, Style::default().fg(theme.status_in_review)),
        Span::styled(
            format!(" ({total_issues} issues)"),
            Style::default().fg(theme.text_secondary),
        ),
    ]);

    let mut scope_spans = Vec::new();
    for (index, scope) in BoardScope::ALL.iter().copied().enumerate() {
        if index > 0 {
            scope_spans.push(Span::raw(" "));
        }
        let style = if app.board_scope == scope {
            theme.chip_style(theme.tab_active)
        } else {
            Style::default().fg(theme.text_secondary)
        };
        scope_spans.push(Span::styled(format!(" {} ", scope.label()), style));
    }
    scope_spans.push(Span::styled(
        "  (t toggles scope)",
        Style::default().fg(theme.text_secondary),
    ));

    let hints = Line::from(vec![
        Span::styled("  h/l", Style::default().fg(theme.text_accent)),
        Span::styled(":column  ", Style::default().fg(theme.text_secondary)),
        Span::styled("j/k", Style::default().fg(theme.text_accent)),
        Span::styled(":issue  ", Style::default().fg(theme.text_secondary)),
        Span::styled("c", Style::default().fg(theme.text_accent)),
        Span::styled(":comment  ", Style::default().fg(theme.text_secondary)),
        Span::styled("x", Style::default().fg(theme.text_accent)),
        Span::styled(":close  ", Style::default().fg(theme.text_secondary)),
        Span::styled("n", Style::default().fg(theme.text_accent)),
        Span::styled(":new  ", Style::default().fg(theme.text_secondary)),
        Span::styled("e", Style::default().fg(theme.text_accent)),
        Span::styled(":edit  ", Style::default().fg(theme.text_secondary)),
        Span::styled("o", Style::default().fg(theme.text_accent)),
        Span::styled(":open  ", Style::default().fg(theme.text_secondary)),
        Span::styled("/", Style::default().fg(theme.text_accent)),
        Span::styled(":filter  ", Style::default().fg(theme.text_secondary)),
        Span::styled("R", Style::default().fg(theme.text_accent)),
        Span::styled(":refresh  ", Style::default().fg(theme.text_secondary)),
        Span::styled("b/Esc", Style::default().fg(theme.text_accent)),
        Span::styled(":back", Style::default().fg(theme.text_secondary)),
    ]);

    frame.render_widget(
        Paragraph::new(header).style(theme.main_surface()),
        Rect::new(area.x, area.y, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from(scope_spans)).style(theme.main_surface()),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(hints).style(theme.main_surface()),
        Rect::new(area.x, area.y + 2, area.width, 1),
    );
}

fn draw_filter_bar(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let is_editing = app.input_mode == InputMode::BoardFilter;

    let label_style = Style::default().fg(if is_editing {
        theme.text_accent
    } else {
        theme.text_secondary
    });
    let text_style = Style::default().fg(theme.text_primary);

    let filtered_total: usize = app.board_issues.iter().map(Vec::len).sum();
    let all_total: usize = app.board_all_issues.iter().map(Vec::len).sum();

    let mut spans = vec![
        Span::styled(" / ", label_style),
        Span::styled(&app.board_filter, text_style),
    ];

    if is_editing {
        spans.push(Span::styled(
            "\u{258f}",
            Style::default().fg(theme.text_accent),
        ));
    }

    if !app.board_filter.is_empty() {
        spans.push(Span::styled(
            format!("  ({filtered_total}/{all_total})"),
            Style::default().fg(theme.text_secondary),
        ));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(theme.main_surface()),
        area,
    );
}

fn draw_board_columns(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let col_count = app.board_columns.len();

    if col_count == 0 || area.width < 4 || area.height < 3 {
        return;
    }

    // Show error message if present
    if let Some(ref error) = app.board_error {
        let msg = format!(" Error: {error} ");
        let x = area.x + (area.width.saturating_sub(msg.len() as u16)) / 2;
        let y = area.y + area.height / 2;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(theme.status_error),
            ))),
            Rect::new(x.max(area.x), y.max(area.y), area.width, 1),
        );
        return;
    }

    // Show loading spinner when board is syncing and columns are empty
    if app.board_loading {
        let total_issues: usize = app.board_issues.iter().map(Vec::len).sum();
        if total_issues == 0 {
            let spinner = super::spinner_char();
            let x = area.x
                + (area
                    .width
                    .saturating_sub("Syncing board data...".len() as u16 + 2))
                    / 2;
            let y = area.y + area.height / 2;
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("{spinner} "),
                        Style::default().fg(theme.accent_secondary),
                    ),
                    Span::styled(
                        "Syncing board data...",
                        Style::default().fg(theme.text_secondary),
                    ),
                ])),
                Rect::new(x.max(area.x), y.max(area.y), area.width, 1),
            );
            return;
        }
    }

    // Build list of non-empty columns (original index → issues), keeping the
    // selected column even if empty so the cursor doesn't jump unexpectedly.
    let non_empty: Vec<(usize, &Vec<crate::github::GitHubBoardItem>)> = (0..col_count)
        .filter_map(|idx| {
            let issues = app.board_issues.get(idx)?;
            if !issues.is_empty() || idx == app.board_column_index {
                Some((idx, issues))
            } else {
                None
            }
        })
        .collect();

    if non_empty.is_empty() {
        let msg = " No issues matching current filters ";
        let x = area.x + (area.width.saturating_sub(msg.len() as u16)) / 2;
        let y = area.y + area.height / 2;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(theme.text_secondary),
            )))
            .style(theme.main_surface()),
            Rect::new(x.max(area.x), y.max(area.y), area.width, 1),
        );
        return;
    }

    // Determine how many columns fit on screen and apply horizontal scroll.
    let min_col_width: u16 = 26;
    let max_visible = ((area.width / min_col_width) as usize)
        .max(1)
        .min(non_empty.len());
    let scroll = app
        .board_scroll_offset
        .min(non_empty.len().saturating_sub(max_visible));
    let visible_slice = &non_empty[scroll..(scroll + max_visible).min(non_empty.len())];
    let visible_count = visible_slice.len();

    // Split area into equal columns for the visible set
    let constraints: Vec<Constraint> = (0..visible_count)
        .map(|_| Constraint::Ratio(1, visible_count as u32))
        .collect();
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    for (vis_idx, &(orig_idx, issues)) in visible_slice.iter().enumerate() {
        let col_area = columns[vis_idx];
        let is_selected_col = orig_idx == app.board_column_index;
        let is_focused_col = is_selected_col && app.focus == Focus::Tasks;
        let col_name = app.board_columns.get(orig_idx).map_or("", String::as_str);
        let issue_count = issues.len();

        // Column border colour
        let border_color = if is_focused_col {
            theme.border_focused
        } else {
            theme.border_unfocused
        };

        // Column header with count badge
        let status_color = column_status_color(orig_idx, col_count, theme);

        let block = Block::default()
            .title(Line::from(vec![
                Span::styled(
                    format!(" {col_name} "),
                    if is_selected_col {
                        theme.chip_style(status_color)
                    } else {
                        Style::default()
                            .fg(status_color)
                            .add_modifier(Modifier::BOLD)
                    },
                ),
                Span::styled(
                    format!("{issue_count} "),
                    Style::default().fg(theme.text_secondary),
                ),
            ]))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .style(if is_selected_col {
                theme.card_surface()
            } else {
                theme.main_surface()
            });

        let inner = block.inner(col_area);
        frame.render_widget(block, col_area);

        // Render issues in this column
        if inner.height == 0 || inner.width < 4 {
            continue;
        }

        // Calculate scroll offset for this column.
        // Cards are 5 lines when labels exist, 4 otherwise. Use 5 for layout.
        let card_height = 5usize;
        let visible_cards = usize::from((inner.height / card_height as u16).max(1));
        let scroll_offset = if is_selected_col && app.board_issue_index >= visible_cards {
            app.board_issue_index.saturating_sub(visible_cards - 1)
        } else {
            0
        };

        for (i, issue) in issues.iter().enumerate().skip(scroll_offset) {
            let row = ((i - scroll_offset) * card_height) as u16;
            if row + card_height as u16 > inner.height {
                break;
            }

            let is_selected = is_focused_col && i == app.board_issue_index;
            draw_issue_card(
                frame,
                theme,
                issue,
                is_selected,
                Rect::new(inner.x, inner.y + row, inner.width, card_height as u16),
            );
        }

        // Show scroll indicator if there are more issues below
        if issues.len() > visible_cards + scroll_offset {
            let remaining = issues.len() - visible_cards - scroll_offset;
            let indicator = format!(" \u{25bc} +{remaining} more ");
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    indicator,
                    Style::default().fg(theme.text_secondary),
                )))
                .style(if is_selected_col {
                    theme.card_surface()
                } else {
                    theme.main_surface()
                }),
                Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
            );
        }
    }

    // Show scroll hints if there are off-screen columns
    if scroll > 0 {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "\u{25c0}",
                Style::default().fg(theme.text_secondary),
            )),
            Rect::new(area.x, area.y + area.height / 2, 1, 1),
        );
    }
    if scroll + max_visible < non_empty.len() {
        let right_x = area.x + area.width.saturating_sub(1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                "\u{25b6}",
                Style::default().fg(theme.text_secondary),
            )),
            Rect::new(right_x, area.y + area.height / 2, 1, 1),
        );
    }
}

fn draw_issue_card(
    frame: &mut Frame,
    theme: &Theme,
    issue: &GitHubBoardItem,
    is_selected: bool,
    area: Rect,
) {
    if area.width < 12 || area.height < 4 {
        return;
    }

    let surface_style = if is_selected {
        theme.selected_fill()
    } else {
        theme.card_surface()
    };
    let surface_bg = surface_style
        .bg
        .unwrap_or(theme.card_surface().bg.unwrap_or_default());
    let text_style = Style::default().fg(theme.text_primary).bg(surface_bg);
    let muted_style = Style::default().fg(theme.text_secondary).bg(surface_bg);

    // Line 1: ▌ repo/owner #123                @assignee
    let border_char = if is_selected { "\u{258c}" } else { "\u{2502}" };
    let border_style = if is_selected {
        Style::default().fg(theme.accent_primary).bg(surface_bg)
    } else {
        Style::default().fg(theme.border_unfocused).bg(surface_bg)
    };
    let number_text = match issue.kind {
        crate::store::GitHubItemKind::PullRequest => format!("PR#{}", issue.number),
        _ => format!("#{}", issue.number),
    };
    let assignee_text = issue
        .assignees
        .first()
        .map_or_else(String::new, |assignee| format!("@{}", assignee.login));
    let assignee_width = assignee_text.len().min(14);
    let number_avail = (area.width as usize).saturating_sub(2 + assignee_width + 1);

    let mut line1_spans = vec![
        Span::styled(border_char, border_style),
        Span::styled(" ", Style::default().bg(surface_bg)),
        Span::styled(
            truncate(&number_text, number_avail),
            if is_selected {
                Style::default()
                    .fg(theme.accent_secondary)
                    .bg(surface_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                muted_style
            },
        ),
    ];
    if !assignee_text.is_empty() {
        // Right-align assignee
        let used = 2 + number_text.len().min(number_avail);
        let gap = (area.width as usize).saturating_sub(used + assignee_width);
        if gap > 0 {
            line1_spans.push(Span::styled(
                " ".repeat(gap),
                Style::default().bg(surface_bg),
            ));
        }
        line1_spans.push(Span::styled(
            truncate(&assignee_text, assignee_width),
            Style::default().fg(theme.accent_tertiary).bg(surface_bg),
        ));
    }

    // Line 2: ▌ Issue title here (bold, truncated)
    let max_title = (area.width as usize).saturating_sub(2);
    let title = truncate(&issue.title, max_title);
    let line2 = Line::from(vec![
        Span::styled(border_char, border_style),
        Span::styled(" ", Style::default().bg(surface_bg)),
        Span::styled(title, text_style.add_modifier(Modifier::BOLD)),
    ]);

    // Line 3: ▌ [bug] [enhancement] [P1]  ← colored label pills
    let mut label_spans = vec![
        Span::styled(border_char, border_style),
        Span::styled(" ", Style::default().bg(surface_bg)),
    ];
    let label_max_width = (area.width as usize).saturating_sub(2);
    render_label_pills(
        &issue.labels,
        label_max_width,
        theme,
        surface_bg,
        &mut label_spans,
    );

    // Line 4: ▌ Status • Priority • Type
    let meta = [
        project_field_label(issue, &["status"]),
        project_field_label(issue, &["priority", "size"]),
        project_field_label(issue, &["type"]),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" \u{2022} ");
    let meta_text = if meta.is_empty() {
        issue.kind.as_str().to_string()
    } else {
        meta
    };
    let line4 = Line::from(vec![
        Span::styled(border_char, border_style),
        Span::styled(" ", Style::default().bg(surface_bg)),
        Span::styled(
            truncate(&meta_text, (area.width as usize).saturating_sub(2)),
            muted_style,
        ),
    ]);

    // Line 5: separator
    let sep = "\u{2500}".repeat((area.width as usize).saturating_sub(1));
    let line5 = Line::from(Span::styled(
        sep,
        Style::default().fg(theme.border_unfocused).bg(surface_bg),
    ));

    let card_lines = vec![
        Line::from(line1_spans),
        line2,
        Line::from(label_spans),
        line4,
        line5,
    ];

    frame.render_widget(Paragraph::new(card_lines).style(surface_style), area);
}

/// Render label pills inline, respecting `max_width`.
fn render_label_pills(
    labels: &[crate::github::GitHubLabel],
    max_width: usize,
    theme: &Theme,
    surface_bg: ratatui::style::Color,
    spans: &mut Vec<Span<'_>>,
) {
    if labels.is_empty() {
        spans.push(Span::styled(
            "no labels",
            Style::default().fg(theme.text_secondary).bg(surface_bg),
        ));
        return;
    }
    let mut used = 0;
    for (i, label) in labels.iter().enumerate() {
        // Each pill: " name " + 1 space gap
        let pill_text = format!(" {} ", label.name);
        let pill_width = pill_text.len();
        let gap = usize::from(i > 0);

        if used + gap + pill_width > max_width {
            let remaining = labels.len() - i;
            let overflow = format!("+{remaining}");
            if used + gap + overflow.len() <= max_width {
                if gap > 0 {
                    spans.push(Span::styled(" ", Style::default().bg(surface_bg)));
                }
                spans.push(Span::styled(
                    overflow,
                    Style::default().fg(theme.text_secondary).bg(surface_bg),
                ));
            }
            break;
        }
        if gap > 0 {
            spans.push(Span::styled(" ", Style::default().bg(surface_bg)));
            used += gap;
        }
        let style = theme.label_pill_style(label.color.as_deref());
        spans.push(Span::styled(pill_text, style));
        used += pill_width;
    }
}

fn project_field_label(issue: &GitHubBoardItem, names: &[&str]) -> Option<String> {
    issue.project_field_values.as_object().and_then(|fields| {
        fields.iter().find_map(|(name, value)| {
            names
                .iter()
                .any(|candidate| name.eq_ignore_ascii_case(candidate))
                .then(|| value_to_string(value))
                .flatten()
        })
    })
}

fn value_to_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|number| number.to_string()))
        .or_else(|| value.as_u64().map(|number| number.to_string()))
        .or_else(|| value.as_f64().map(|number| format!("{number:.0}")))
}

fn truncate(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text.chars().count() <= max_width {
        return text.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }

    let mut out = String::new();
    for ch in text.chars().take(max_width.saturating_sub(3)) {
        out.push(ch);
    }
    out.push_str("...");
    out
}

/// Map column index to a status colour for the header.
///
/// First column uses the pending/backlog colour, the last column uses the done
/// colour, the second-to-last uses the in-review colour, and everything in
/// between uses the working colour.
fn column_status_color(col_idx: usize, col_count: usize, theme: &Theme) -> ratatui::style::Color {
    if col_count == 0 {
        return theme.text_primary;
    }
    if col_idx == 0 {
        theme.status_pending
    } else if col_idx == col_count - 1 {
        theme.status_done
    } else if col_idx == col_count - 2 {
        theme.status_in_review
    } else {
        theme.status_working
    }
}

/// Draw the sprint filter overlay (centred popup).
pub(super) fn draw_sprint_overlay(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 48u16.min(area.width.saturating_sub(4));
    let height = (app.board_sprints.len() as u16 + 5)
        .min(area.height.saturating_sub(4))
        .max(6);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(" Select Sprint ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.form_border_project))
        .style(app.theme.overlay_surface());
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    if inner.height < 3 {
        return;
    }

    let dim = Style::default().fg(app.theme.text_secondary);
    let highlight = Style::default().fg(app.theme.text_accent);

    let all_selected = app.board_sprint_index == 0;
    let all_style = if all_selected {
        Style::default()
            .fg(app.theme.text_primary)
            .bg(app.theme.border_unfocused)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.theme.text_primary)
    };
    let all_prefix = if all_selected { "\u{25b8} " } else { "  " };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(all_prefix, all_style),
            Span::styled("All Items", all_style),
        ]))
        .style(app.theme.overlay_surface()),
        Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(2), 1),
    );

    for (i, sprint) in app.board_sprints.iter().enumerate() {
        let row = (i + 1) as u16 + 1;
        if inner.y + row >= inner.y + inner.height - 1 {
            break;
        }
        let is_selected = i + 1 == app.board_sprint_index;
        let style = if is_selected {
            Style::default()
                .fg(app.theme.text_primary)
                .bg(app.theme.border_unfocused)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.text_primary)
        };
        let prefix = if is_selected { "\u{25b8} " } else { "  " };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(sprint.as_str(), style),
            ]))
            .style(app.theme.overlay_surface()),
            Rect::new(inner.x + 1, inner.y + row, inner.width.saturating_sub(2), 1),
        );
    }

    // Hints at the bottom
    let hint_y = inner.y + inner.height - 1;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  j/k", highlight),
            Span::styled(":navigate  ", dim),
            Span::styled("Enter", highlight),
            Span::styled(":select  ", dim),
            Span::styled("Esc", highlight),
            Span::styled(":cancel", dim),
        ]))
        .style(app.theme.overlay_surface()),
        Rect::new(inner.x, hint_y, inner.width, 1),
    );
}

pub(super) fn draw_project_overlay(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 72u16.min(area.width.saturating_sub(4));
    let visible_indices = app.visible_github_project_indices();
    let height = (visible_indices.len() as u16 + 8)
        .min(area.height.saturating_sub(4))
        .max(11);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(" Switch GitHub Board ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.form_border_project))
        .style(app.theme.overlay_surface());
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    if inner.height < 6 {
        return;
    }

    frame.render_widget(
        Paragraph::new(
            " Select the default GitHub Project v2 board for this repository workspace.",
        )
        .style(Style::default().fg(app.theme.text_secondary)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let cursor = app.input_cursor.min(app.input_buffer.len());
    let (before, after) = app.input_buffer.split_at(cursor);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" Filter: ", Style::default().fg(app.theme.accent_primary)),
            Span::raw(before.to_string()),
            Span::styled("\u{2588}", Style::default().fg(app.theme.accent_primary)),
            Span::raw(after.to_string()),
        ]))
        .style(app.theme.overlay_surface()),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );

    let list_area = Rect::new(
        inner.x,
        inner.y + 3,
        inner.width,
        inner.height.saturating_sub(4),
    );

    if app.github_projects_v2.is_empty() {
        frame.render_widget(
            Paragraph::new("  No GitHub Projects v2 loaded.\n  Press R to refresh.")
                .style(Style::default().fg(app.theme.text_secondary)),
            list_area,
        );
    } else if visible_indices.is_empty() {
        frame.render_widget(
            Paragraph::new("  No GitHub boards match the current filter.")
                .style(Style::default().fg(app.theme.text_secondary)),
            list_area,
        );
    } else {
        let default_project = app.config.github_app.default_project_id.as_deref();
        let selected_visible_index = visible_indices
            .iter()
            .position(|&index| index == app.github_project_index)
            .unwrap_or(0);
        let rows_per_page = usize::from(list_area.height.max(1));
        let max_start = visible_indices.len().saturating_sub(rows_per_page);
        let start = selected_visible_index
            .saturating_sub(rows_per_page / 2)
            .min(max_start);
        let end = (start + rows_per_page).min(visible_indices.len());
        let mut lines = Vec::with_capacity(end.saturating_sub(start));
        for &index in &visible_indices[start..end] {
            let project = &app.github_projects_v2[index];
            let selected = index == app.github_project_index;
            let project_number = project.project_number.to_string();
            let is_default = default_project == Some(project_number.as_str());
            let prefix = if selected { "▸ " } else { "  " };
            let suffix = if is_default { "  default" } else { "" };
            let style = if selected {
                app.theme.selected_fill()
            } else {
                Style::default().fg(app.theme.text_primary)
            };

            lines.push(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(format!("{}{}", project.display_label(), suffix), style),
            ]));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .style(app.theme.overlay_surface())
                .wrap(ratatui::widgets::Wrap { trim: false }),
            list_area,
        );
    }

    let hint_y = panel_area.y + panel_area.height.saturating_sub(1);
    let highlight = Style::default().fg(app.theme.accent_primary);
    let dim = Style::default().fg(app.theme.text_secondary);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" j/k", highlight),
            Span::styled(":nav  ", dim),
            Span::styled("Enter", highlight),
            Span::styled(":set default board  ", dim),
            Span::styled("x", highlight),
            Span::styled(":auto-select  ", dim),
            Span::styled("type", highlight),
            Span::styled(":filter  ", dim),
            Span::styled("Esc", highlight),
            Span::styled(":cancel", dim),
        ]))
        .style(app.theme.overlay_surface()),
        Rect::new(inner.x, hint_y, inner.width, 1),
    );
}
