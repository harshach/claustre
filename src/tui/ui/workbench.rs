use std::collections::HashSet;
use std::process::Command;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use std::fmt::Write as _;

use regex::Regex;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use tui_markdown::from_str as markdown_from_str;

use crate::{
    github::GitHubBoardItem,
    store::{GitHubItemKind, RuntimeServiceStatus, Task, WorkflowRunStatus, WorkflowStageStatus},
    tui::theme::Theme,
};

use super::super::app::{
    App, Focus, InputMode, InspectorTab, MyTaskItem, ReviewQueueItem, ReviewQueueTab,
    SelectedThreadContext, SettingsSection, SidebarItem, WorkbenchView,
};
use super::super::form::format_with_cursor;
use super::board::{draw_board, draw_project_overlay, draw_sprint_overlay};
use super::spinner_char;
use super::toast_line;
use super::usage::draw_usage_bars;

const DIFF_CACHE_TTL: Duration = Duration::from_secs(2);
const DEFAULT_MAIN_MIN_WIDTH: u16 = 24;
const SIDEBAR_MIN_WIDTH: u16 = 18;
const INSPECTOR_MIN_WIDTH: u16 = 26;

#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkbenchLayout {
    pub title: Rect,
    pub body: Rect,
    pub status: Rect,
    pub hints: Rect,
    pub sidebar: Rect,
    pub main: Rect,
    pub inspector: Rect,
}

pub(crate) fn workbench_uses_persistent_inspector(view: WorkbenchView) -> bool {
    matches!(view, WorkbenchView::Threads | WorkbenchView::Reviews)
}

pub(crate) fn compute_workbench_layout(
    area: Rect,
    desired_sidebar_width: u16,
    desired_inspector_width: u16,
    show_inspector: bool,
) -> WorkbenchLayout {
    compute_workbench_layout_ex(
        area,
        desired_sidebar_width,
        desired_inspector_width,
        show_inspector,
        false,
    )
}

pub(crate) fn compute_workbench_layout_ex(
    area: Rect,
    desired_sidebar_width: u16,
    desired_inspector_width: u16,
    show_inspector: bool,
    inspector_expanded: bool,
) -> WorkbenchLayout {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    let title = outer[0];
    let body = outer[1];
    let status = outer[2];
    let hints = outer[3];
    let (sidebar, main, inspector) = if show_inspector && inspector_expanded {
        // Expanded: sidebar + inspector takes all remaining space (main = 0)
        let sidebar_width = desired_sidebar_width.clamp(1, body.width.saturating_sub(1).max(1));
        let _inspector_width = body.width.saturating_sub(sidebar_width);
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(sidebar_width),
                Constraint::Length(0),
                Constraint::Min(1),
            ])
            .split(body);
        (chunks[0], chunks[1], chunks[2])
    } else if show_inspector {
        let (sidebar_width, inspector_width) =
            normalize_workbench_widths(body.width, desired_sidebar_width, desired_inspector_width);
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(sidebar_width),
                Constraint::Min(1),
                Constraint::Length(inspector_width),
            ])
            .split(body);
        (chunks[0], chunks[1], chunks[2])
    } else {
        let sidebar_width = desired_sidebar_width.clamp(1, body.width.saturating_sub(1).max(1));
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar_width), Constraint::Min(1)])
            .split(body);
        (
            chunks[0],
            chunks[1],
            Rect::new(
                chunks[1].x.saturating_add(chunks[1].width),
                body.y,
                0,
                body.height,
            ),
        )
    };

    WorkbenchLayout {
        title,
        body,
        status,
        hints,
        sidebar,
        main,
        inspector,
    }
}

pub(crate) fn normalize_workbench_widths(
    total_width: u16,
    desired_sidebar_width: u16,
    desired_inspector_width: u16,
) -> (u16, u16) {
    if total_width <= 2 {
        return (1, 1);
    }

    let mut sidebar_width = desired_sidebar_width.min(total_width.saturating_sub(2));
    let mut inspector_width = desired_inspector_width.min(
        total_width
            .saturating_sub(sidebar_width)
            .saturating_sub(1)
            .max(1),
    );
    let mut main_width = total_width
        .saturating_sub(sidebar_width)
        .saturating_sub(inspector_width);

    if main_width < DEFAULT_MAIN_MIN_WIDTH {
        let need = DEFAULT_MAIN_MIN_WIDTH - main_width;
        let shrink_inspector = need.min(inspector_width.saturating_sub(INSPECTOR_MIN_WIDTH));
        inspector_width = inspector_width.saturating_sub(shrink_inspector);
        main_width = total_width
            .saturating_sub(sidebar_width)
            .saturating_sub(inspector_width);

        let need = DEFAULT_MAIN_MIN_WIDTH.saturating_sub(main_width);
        let shrink_sidebar = need.min(sidebar_width.saturating_sub(SIDEBAR_MIN_WIDTH));
        sidebar_width = sidebar_width.saturating_sub(shrink_sidebar);
    }

    sidebar_width = sidebar_width.clamp(1, total_width.saturating_sub(2));
    inspector_width = inspector_width.clamp(
        1,
        total_width
            .saturating_sub(sidebar_width)
            .saturating_sub(1)
            .max(1),
    );

    (sidebar_width, inspector_width)
}

pub(super) fn draw_active(frame: &mut Frame, app: &mut App) {
    draw_active_impl(frame, app, frame.area());
}

pub(super) fn draw_active_in_area(frame: &mut Frame, app: &mut App, area: Rect) {
    draw_active_impl(frame, app, area);
}

fn draw_active_impl(frame: &mut Frame, app: &mut App, size: Rect) {
    let show_inspector = workbench_uses_persistent_inspector(app.workbench_view);
    let layout = compute_workbench_layout_ex(
        size,
        app.workbench_sidebar_width,
        app.workbench_inspector_width,
        show_inspector,
        app.inspector_expanded,
    );

    let thread_ctx = app.selected_thread_context();
    let selected_task = app.selected_task().cloned();
    let selected_board_item = app.selected_board_issue();
    let selected_review_item = app.selected_review_item().cloned();

    draw_title_bar(frame, app, layout.title);

    draw_sidebar(frame, app, layout.sidebar);
    if !app.inspector_expanded {
        draw_main(frame, app, layout.main, thread_ctx.as_ref());
    }
    if show_inspector {
        draw_inspector(
            frame,
            app,
            layout.inspector,
            selected_task.as_ref(),
            selected_board_item.as_ref(),
            selected_review_item.as_ref(),
            thread_ctx.as_ref(),
        );
    }

    draw_status_line(frame, app, layout.status);
    draw_hint_line(frame, app, layout.hints);

    if app.input_mode == InputMode::MilestoneFilter {
        draw_sprint_overlay(frame, app);
    } else if app.input_mode == InputMode::GitHubProjectPicker {
        draw_project_overlay(frame, app);
    }
}

pub(super) fn draw_thread_session_view(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    session_label: &str,
    thread_ctx: &SelectedThreadContext,
) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    // Slim header: just session label + status + Ctrl+O hint
    let status_icon = match thread_ctx.thread.status {
        crate::store::ThreadStatus::Running => "\u{25cf} ", // ●
        crate::store::ThreadStatus::Done => "\u{2713} ",    // ✓
        _ => "",
    };
    let status_color = match thread_ctx.thread.status {
        crate::store::ThreadStatus::Running => app.theme.status_working,
        crate::store::ThreadStatus::Done => app.theme.status_done,
        _ => app.theme.text_secondary,
    };
    let mut header_spans = vec![
        Span::styled(format!(" {status_icon}"), Style::default().fg(status_color)),
        Span::styled(
            format!("{session_label} "),
            Style::default()
                .fg(app.theme.accent_tertiary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "Ctrl+O: terminal",
            Style::default().fg(app.theme.text_secondary),
        ),
    ];
    // Append workflow badge when a workflow is active
    if let Some(run) = thread_ctx.workflow_run.as_ref() {
        let workflow_name = thread_ctx
            .workflow_def
            .as_ref()
            .map_or("workflow", |def| def.name.as_str());
        let stage_name = run.current_stage.as_deref().unwrap_or("...");
        let (badge_color, hint) = match run.status {
            WorkflowRunStatus::WaitingApproval => (app.theme.status_paused, " [\u{23F8} g]"),
            WorkflowRunStatus::Running => (app.theme.status_working, ""),
            WorkflowRunStatus::Completed => (app.theme.status_done, ""),
            _ => (app.theme.text_secondary, ""),
        };
        header_spans.push(Span::styled(
            "  \u{2502} ",
            Style::default()
                .fg(app.theme.border_unfocused)
                .add_modifier(Modifier::DIM),
        ));
        header_spans.push(Span::styled(
            format!("{workflow_name}: {stage_name}{hint}"),
            Style::default().fg(badge_color),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(header_spans)), vertical[0]);

    let selected_task = thread_ctx
        .thread
        .task_id
        .as_deref()
        .and_then(|task_id| app.tasks.iter().find(|task| task.id == task_id))
        .cloned();

    if app.inspector_expanded {
        // Expanded: inspector takes full body width, no chat panel
        draw_inspector(
            frame,
            app,
            vertical[1],
            selected_task.as_ref(),
            None,
            None,
            Some(thread_ctx),
        );
    } else {
        let inspector_width = app.workbench_inspector_width.clamp(
            INSPECTOR_MIN_WIDTH,
            vertical[1]
                .width
                .saturating_sub(DEFAULT_MAIN_MIN_WIDTH)
                .max(INSPECTOR_MIN_WIDTH),
        );
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(DEFAULT_MAIN_MIN_WIDTH),
                Constraint::Length(inspector_width),
            ])
            .split(vertical[1]);

        draw_thread_chat(frame, app, body[0], Some(thread_ctx));
        draw_inspector(
            frame,
            app,
            body[1],
            selected_task.as_ref(),
            None,
            None,
            Some(thread_ctx),
        );
    }
}

fn draw_title_bar(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(
        " claustre ",
        Style::default()
            .fg(app.theme.text_primary)
            .bg(app.theme.accent_tertiary)
            .add_modifier(Modifier::BOLD),
    )];
    spans.push(Span::styled(
        crate::update::VERSION,
        Style::default()
            .fg(app.theme.accent_secondary)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        "  •  ",
        Style::default().fg(app.theme.border_unfocused),
    ));
    spans.push(Span::styled(
        app.workbench_view.label(),
        Style::default()
            .fg(app.theme.accent_primary)
            .add_modifier(Modifier::BOLD),
    ));
    if let Some(project) = app.selected_project() {
        spans.push(Span::styled(
            format!("  •  {}", project.name),
            Style::default().fg(app.theme.text_primary),
        ));
    }
    if let Some(activity) = app.busy_indicator_label() {
        spans.push(Span::styled(
            format!("  •  {} {}", spinner_char(), activity),
            Style::default()
                .fg(app.theme.spinner)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(ref warning) = app.config_warning {
        spans.push(Span::styled(
            format!("  •  warning: {warning}"),
            Style::default()
                .fg(app.theme.accent_secondary)
                .add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(Line::from(vec![Span::styled(
            " Sidebar ",
            Style::default()
                .fg(app.theme.text_primary)
                .add_modifier(Modifier::BOLD),
        )]))
        .borders(Borders::ALL)
        .border_style(if app.focus == Focus::Projects {
            app.theme.focused_border()
        } else {
            app.theme.unfocused_border()
        })
        .style(app.theme.sidebar_surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![Line::from(vec![Span::styled(
        " Navigation",
        Style::default()
            .fg(app.theme.accent_secondary)
            .add_modifier(Modifier::BOLD),
    )])];

    for view in WorkbenchView::ALL {
        let sidebar_selected = app.focus == Focus::Projects
            && app.selected_sidebar_item() == SidebarItem::Navigation(view);
        let current_view = view == app.workbench_view;
        let marker = if sidebar_selected {
            ">"
        } else if current_view {
            "*"
        } else {
            " "
        };
        let style = if sidebar_selected {
            app.theme.selected_fill()
        } else if current_view {
            Style::default()
                .fg(app.theme.accent_primary)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.text_primary)
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker} "), style),
            Span::styled(view.label(), style),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        " Repositories",
        Style::default()
            .fg(app.theme.accent_secondary)
            .add_modifier(Modifier::BOLD),
    )]));

    if app.projects.is_empty() {
        lines.push(Line::from(Span::styled(
            "   no projects",
            Style::default().fg(app.theme.text_secondary),
        )));
    } else {
        for (idx, project) in app.projects.iter().enumerate() {
            let sidebar_selected = app.focus == Focus::Projects
                && app.selected_sidebar_item() == SidebarItem::Repository(idx);
            let selected = idx == app.project_index;
            let summary = app
                .project_summaries
                .get(&project.id)
                .cloned()
                .unwrap_or_default();
            let style = if sidebar_selected {
                app.theme.selected_fill()
            } else if selected {
                Style::default()
                    .fg(app.theme.accent_primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.text_primary)
            };
            let marker = if sidebar_selected {
                ">"
            } else if selected {
                "*"
            } else {
                " "
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {marker} "), style),
                Span::styled(project.name.as_str(), style),
                Span::styled(
                    format!(
                        " [{} t / {} s]",
                        summary.task_counts.pending
                            + summary.task_counts.working
                            + summary.task_counts.in_review
                            + summary.task_counts.conflict
                            + summary.task_counts.ci_failed,
                        summary.active_sessions.len()
                    ),
                    Style::default().fg(app.theme.text_secondary),
                ),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        " Focus",
        Style::default()
            .fg(app.theme.accent_secondary)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(Span::styled(
        match app.focus {
            Focus::Projects => "   sidebar",
            Focus::Tasks => "   main",
            Focus::Inspector => "   inspector",
        },
        Style::default().fg(app.theme.text_primary),
    )));

    if !app.threads.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            " Threads",
            Style::default()
                .fg(app.theme.accent_secondary)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(Span::styled(
            format!("   {} local thread(s)", app.threads.len()),
            Style::default().fg(app.theme.text_primary),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .style(app.theme.sidebar_surface())
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn draw_main(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    thread_ctx: Option<&SelectedThreadContext>,
) {
    match app.workbench_view {
        WorkbenchView::MyTasks => {
            if app.uses_github_my_tasks() {
                draw_my_task_index(frame, app, area);
            } else {
                let tasks = app.visible_tasks();
                draw_task_index(frame, app, area, " My Tasks ", &tasks, app.task_index);
            }
        }
        WorkbenchView::Reviews => {
            draw_review_queue(frame, app, area);
        }
        WorkbenchView::Threads => draw_thread_workspace(frame, app, area, thread_ctx),
        WorkbenchView::Agents => draw_agents_view(frame, app, area),
        WorkbenchView::Settings => draw_settings_view(frame, app, area),
        WorkbenchView::SprintBoard => draw_board(frame, app, area),
    }
}

fn draw_task_index(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    title: &str,
    tasks: &[&Task],
    selected_index: usize,
) {
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                title.to_owned(),
                Style::default()
                    .fg(app.theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "({}/{}) ",
                    selected_index.saturating_add(1).min(tasks.len().max(1)),
                    tasks.len()
                ),
                Style::default().fg(app.theme.text_secondary),
            ),
        ]))
        .borders(Borders::ALL)
        .border_style(if app.focus == Focus::Tasks {
            app.theme.focused_border()
        } else {
            app.theme.unfocused_border()
        })
        .style(app.theme.main_surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if tasks.is_empty() {
        let empty_lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "No tasks yet",
                Style::default()
                    .fg(app.theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Press n to create a task",
                Style::default()
                    .fg(app.theme.text_secondary)
                    .add_modifier(Modifier::DIM),
            )),
        ];
        frame.render_widget(
            Paragraph::new(empty_lines)
                .alignment(ratatui::layout::Alignment::Center)
                .style(app.theme.main_surface())
                .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    let mut lines = Vec::with_capacity(tasks.len());
    for (idx, task) in tasks.iter().enumerate() {
        let selected = idx == selected_index;
        let style = if selected {
            app.theme.selected_fill()
        } else {
            Style::default().fg(app.theme.text_primary)
        };
        let marker = if selected && app.focus == Focus::Tasks {
            ">"
        } else {
            " "
        };
        let pr_marker = if task.pr_url.is_some() { "PR" } else { "--" };
        let thread_marker = if app
            .threads
            .iter()
            .any(|thread| thread.task_id.as_deref() == Some(task.id.as_str()))
        {
            "thread"
        } else {
            "task"
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker} "), style),
            Span::styled(
                task.status.symbol(),
                app.theme.task_status_style(task.status),
            ),
            Span::raw(" "),
            Span::styled(
                truncate(task.title.as_str(), inner.width.saturating_sub(22) as usize),
                style,
            ),
            Span::styled(
                format!("  {:<10} {pr_marker:<2} {thread_marker}", task.status),
                Style::default().fg(app.theme.text_secondary),
            ),
        ]));
    }

    let scroll_y = (selected_index as u16 + 1).saturating_sub(inner.height);

    frame.render_widget(
        Paragraph::new(lines)
            .style(app.theme.main_surface())
            .scroll((scroll_y, 0)),
        inner,
    );
}

fn draw_my_task_index(frame: &mut Frame, app: &App, area: Rect) {
    let items = app.visible_my_tasks();
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                " My Tasks ".to_owned(),
                Style::default()
                    .fg(app.theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "({}/{}) ",
                    app.task_index.saturating_add(1).min(items.len().max(1)),
                    items.len()
                ),
                Style::default().fg(app.theme.text_secondary),
            ),
        ]))
        .borders(Borders::ALL)
        .border_style(if app.focus == Focus::Tasks {
            app.theme.focused_border()
        } else {
            app.theme.unfocused_border()
        })
        .style(app.theme.main_surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if items.is_empty() {
        let empty_lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "No assigned items",
                Style::default()
                    .fg(app.theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Sync GitHub data or switch repositories",
                Style::default()
                    .fg(app.theme.text_secondary)
                    .add_modifier(Modifier::DIM),
            )),
            Line::from(Span::styled(
                "to load work assigned to you.",
                Style::default()
                    .fg(app.theme.text_secondary)
                    .add_modifier(Modifier::DIM),
            )),
        ];
        frame.render_widget(
            Paragraph::new(empty_lines)
                .alignment(ratatui::layout::Alignment::Center)
                .style(app.theme.main_surface())
                .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    let lines_per_item: u16 = 3; // title + meta + blank
    let mut lines = Vec::with_capacity(items.len() * 4);
    for (idx, item) in items.iter().enumerate() {
        lines.extend(draw_my_task_row(
            app,
            item,
            idx == app.task_index,
            inner.width,
        ));
    }

    // Scroll so the selected item is always visible.
    let selected_top = (app.task_index as u16) * lines_per_item;
    let scroll_y = (selected_top + lines_per_item).saturating_sub(inner.height);

    frame.render_widget(
        Paragraph::new(lines)
            .style(app.theme.main_surface())
            .scroll((scroll_y, 0)),
        inner,
    );
}

fn draw_my_task_row(
    app: &App,
    item: &MyTaskItem,
    selected: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let is_focused = selected && app.focus == Focus::Tasks;

    // Selection indicator: colored left bar when focused, dim when selected but not focused
    let bar = if is_focused {
        Span::styled(" \u{258e} ", Style::default().fg(app.theme.accent_primary))
    } else if selected {
        Span::styled(
            " \u{258e} ",
            Style::default().fg(app.theme.border_unfocused),
        )
    } else {
        Span::raw("   ")
    };

    let title_style = if selected {
        Style::default()
            .fg(app.theme.text_primary)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.theme.text_primary)
    };
    let number_style = if selected {
        Style::default()
            .fg(app.theme.accent_secondary)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.theme.text_secondary)
    };
    let meta_style = Style::default().fg(app.theme.text_secondary);
    let kind_badge_style = match item.github_item.kind {
        GitHubItemKind::Issue => Style::default().fg(app.theme.accent_secondary),
        GitHubItemKind::PullRequest => Style::default().fg(app.theme.accent_primary),
        GitHubItemKind::ProjectItem => Style::default().fg(app.theme.accent_tertiary),
    };
    let kind_badge = match item.github_item.kind {
        GitHubItemKind::Issue => "issue",
        GitHubItemKind::PullRequest => "pr",
        GitHubItemKind::ProjectItem => "item",
    };

    let available_width = usize::from(width.saturating_sub(10)).max(16);
    let title_line = item.title_line();
    let title = truncate(title_line.as_str(), available_width);

    let repo_name = truncate(item.repo.full_name.as_str(), 18);

    let fields = item.github_item.project_field_values.as_object();
    let mut meta_parts: Vec<String> = [
        project_field_from_map(fields, &["type"]),
        project_field_from_map(fields, &["domain"]),
        project_field_from_map(fields, &["release", "release version"]),
        project_field_from_map(fields, &["priority", "size"]),
    ]
    .into_iter()
    .flatten()
    .collect();

    meta_parts.push(repo_name);

    // Only show thread/PR info when there's something meaningful
    if let Some(thread) = item.linked_thread.as_ref() {
        meta_parts.push(format!("{} thread", thread.status));
    }
    if let Some(pr) = item.linked_pr.as_ref() {
        meta_parts.push(format!("PR #{}", pr.number));
    } else if let Some(pr_num) = item.github_item.linked_pr_number {
        meta_parts.push(format!("PR #{pr_num}"));
    }

    let meta_text = meta_parts.join(" \u{00B7} ");

    vec![
        Line::from(vec![
            bar.clone(),
            Span::styled(format!("#{} ", item.github_item.number), number_style),
            Span::styled(title, title_style),
            Span::styled(format!("  {kind_badge}"), kind_badge_style),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(
                truncate(&meta_text, width.saturating_sub(4) as usize),
                meta_style,
            ),
        ]),
        Line::from(""),
    ]
}

fn project_field_from_map(
    fields: Option<&serde_json::Map<String, serde_json::Value>>,
    names: &[&str],
) -> Option<String> {
    fields.and_then(|fields| {
        fields.iter().find_map(|(name, value)| {
            names
                .iter()
                .any(|candidate| name.eq_ignore_ascii_case(candidate))
                .then(|| {
                    value
                        .as_str()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .or_else(|| value.as_i64().map(|number| number.to_string()))
                        .or_else(|| value.as_u64().map(|number| number.to_string()))
                })
                .flatten()
        })
    })
}

fn draw_review_queue(frame: &mut Frame, app: &App, area: Rect) {
    let items = app.review_items();
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                " Reviews ",
                Style::default()
                    .fg(app.theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "({}/{}) ",
                    app.review_index.saturating_add(1).min(items.len().max(1)),
                    items.len()
                ),
                Style::default().fg(app.theme.text_secondary),
            ),
        ]))
        .borders(Borders::ALL)
        .border_style(if app.focus == Focus::Tasks {
            app.theme.focused_border()
        } else {
            app.theme.unfocused_border()
        })
        .style(app.theme.main_surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 12 || inner.height < 3 {
        return;
    }

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    draw_review_tabs(frame, app, sections[0]);

    if items.is_empty() {
        let empty_message = match app.review_queue_tab {
            ReviewQueueTab::Authored => {
                "  No open pull requests authored by you were found in the synced repo."
            }
            ReviewQueueTab::NeedsReview => {
                "  No pull requests currently look assigned to your review queue."
            }
        };
        frame.render_widget(
            Paragraph::new(format!(
                "{empty_message}\n  Sync GitHub data or switch projects to load more PRs."
            ))
            .style(Style::default().fg(app.theme.text_secondary))
            .wrap(Wrap { trim: false }),
            sections[1],
        );
        return;
    }

    let mut lines = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let selected = idx == app.review_index;
        lines.push(draw_review_row(app, item, selected, sections[1].width));
    }
    let scroll_y = (app.review_index as u16 + 1).saturating_sub(sections[1].height);
    frame.render_widget(
        Paragraph::new(lines)
            .style(app.theme.main_surface())
            .scroll((scroll_y, 0)),
        sections[1],
    );
}

fn draw_review_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let active_style = app.theme.chip_style(app.theme.tab_active);
    let inactive_style = Style::default().fg(app.theme.text_secondary);
    let mut spans = Vec::new();
    for (idx, tab) in ReviewQueueTab::ALL.iter().copied().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" "));
        }
        let count = match tab {
            ReviewQueueTab::Authored => app.review_authored_items.len(),
            ReviewQueueTab::NeedsReview => app.review_requested_items.len(),
        };
        let style = if app.review_queue_tab == tab {
            active_style
        } else {
            inactive_style
        };
        spans.push(Span::styled(format!(" {} ({count}) ", tab.label()), style));
    }
    spans.push(Span::styled(
        "  (t toggles queue)",
        Style::default().fg(app.theme.text_secondary),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_review_row(app: &App, item: &ReviewQueueItem, selected: bool, width: u16) -> Line<'static> {
    let marker = if selected && app.focus == Focus::Tasks {
        ">"
    } else {
        " "
    };
    let mut title = item.title_line();
    let meta = review_row_meta(app, item);
    let available_width = usize::from(width.saturating_sub(20));
    if title.chars().count() > available_width.max(12) {
        title = truncate(&title, available_width.max(12));
    }
    let style = if selected {
        app.theme.selected_fill()
    } else {
        Style::default().fg(app.theme.text_primary)
    };

    Line::from(vec![
        Span::styled(format!(" {marker} "), style),
        Span::styled(
            if item.pr_cache.is_draft { "◐" } else { "○" },
            if item.pr_cache.is_draft {
                Style::default().fg(app.theme.text_secondary)
            } else {
                Style::default().fg(app.theme.accent_primary)
            },
        ),
        Span::raw(" "),
        Span::styled(title, style),
        Span::styled(
            format!("  {meta}"),
            Style::default().fg(app.theme.text_secondary),
        ),
    ])
}

fn review_row_meta(app: &App, item: &ReviewQueueItem) -> String {
    let queue_label = match app.review_queue_tab {
        ReviewQueueTab::Authored => "continue",
        ReviewQueueTab::NeedsReview => "review",
    };
    let review_state = item
        .pr_cache
        .review_decision
        .as_deref()
        .unwrap_or(if item.pr_cache.is_draft {
            "draft"
        } else {
            "open"
        })
        .to_ascii_lowercase()
        .replace('_', " ");
    let thread_marker = if app
        .threads
        .iter()
        .any(|thread| thread.github_item_id.as_deref() == Some(item.github_item.id.as_str()))
    {
        "thread"
    } else {
        "--"
    };
    format!(
        "{queue_label:<8} {review_state:<14} c{} r{} d{} {thread_marker}",
        item.comment_count, item.review_count, item.review_comment_count
    )
}

fn draw_thread_workspace(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    thread_ctx: Option<&SelectedThreadContext>,
) {
    if app.threads.is_empty() {
        let block = Block::default()
            .title(Line::from(vec![Span::styled(
                " Threads ",
                Style::default()
                    .fg(app.theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            )]))
            .borders(Borders::ALL)
            .border_style(if app.focus == Focus::Tasks {
                app.theme.focused_border()
            } else {
                app.theme.unfocused_border()
            })
            .style(app.theme.main_surface());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(
                "  No threads yet.\n  Press n or l to launch an ad hoc thread for this repo,\n  or launch a linked thread from My Tasks / Sprint Board.",
            )
            .style(Style::default().fg(app.theme.text_secondary))
            .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    let list_width = if area.width < 56 {
        (area.width / 2).max(18)
    } else {
        area.width.saturating_sub(24).clamp(26, 38)
    };
    let sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(list_width), Constraint::Min(24)])
        .split(area);

    draw_thread_list(frame, app, sections[0]);
    draw_thread_chat(frame, app, sections[1], thread_ctx);
}

fn draw_thread_list(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                " Threads ",
                Style::default()
                    .fg(app.theme.accent_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "({}/{}) ",
                    app.thread_index
                        .saturating_add(1)
                        .min(app.threads.len().max(1)),
                    app.threads.len()
                ),
                Style::default().fg(app.theme.text_secondary),
            ),
        ]))
        .borders(Borders::ALL)
        .border_style(app.theme.unfocused_border())
        .style(app.theme.main_surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = Vec::with_capacity(app.threads.len());
    for (index, thread) in app.threads.iter().enumerate() {
        let selected = index == app.thread_index;
        let cursor = if selected { ">" } else { " " };
        let session = thread_session_state(app, thread);
        let source = thread_source_label(thread);
        let row_style = if selected {
            app.theme.selected_fill()
        } else {
            Style::default().fg(app.theme.text_primary)
        };
        let status_style = app.theme.thread_status_style(thread.status);
        let provider_style = Style::default().fg(app.theme.accent_primary);
        let meta_style = Style::default().fg(app.theme.text_secondary);
        let title = truncate(
            thread.title.as_str(),
            inner.width.saturating_sub(26) as usize,
        );
        lines.push(Line::from(vec![
            Span::styled(format!("{cursor} "), row_style),
            Span::styled(title, row_style),
            Span::styled(format!(" [{}]", thread.status), status_style),
            Span::styled(format!(" {}", thread.provider_kind), provider_style),
            Span::styled(format!(" {session} {source}"), meta_style),
        ]));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .style(app.theme.main_surface())
            .wrap(Wrap { trim: false }),
        inner,
    );
}

pub(super) fn draw_thread_chat(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    thread_ctx: Option<&SelectedThreadContext>,
) {
    let title = thread_ctx.map_or_else(
        || "Conversation".to_string(),
        |ctx| truncate(&ctx.thread.title, area.width.saturating_sub(8) as usize),
    );
    let block = Block::default()
        .title(Line::from(vec![Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(app.theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        )]))
        .borders(Borders::ALL)
        .border_style(if app.focus == Focus::Tasks {
            app.theme.focused_border()
        } else {
            app.theme.unfocused_border()
        })
        .style(app.theme.main_surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(thread_ctx) = thread_ctx else {
        frame.render_widget(
            Paragraph::new("  Select a thread to start chatting.")
                .style(Style::default().fg(app.theme.text_secondary)),
            inner,
        );
        return;
    };

    // Check if Claude is actively working (used for choices and compose height)
    let session_is_working = thread_ctx.thread.session_id.as_deref().is_some_and(|sid| {
        app.sessions
            .iter()
            .any(|s| s.id == sid && s.claude_status == crate::store::ClaudeStatus::Working)
            && !app.pty_idle_sessions.contains(sid)
    });

    // Dynamic compose height: grows with content up to 40% of the panel
    let compose_active = app.input_mode == InputMode::ThreadCompose
        && app.thread_compose_thread_id.as_deref() == Some(thread_ctx.thread.id.as_str());
    let choices_visible = !session_is_working && !app.quick_reply_choices.is_empty();
    let compose_content_height = if compose_active {
        let text = format!("> {}", app.input_buffer);
        let available_w = inner.width.saturating_sub(2);
        super::super::form::measure_wrapped_height(&text, available_w).saturating_add(1) // +1 for "Reply" label
    } else if !app.thread_compose_buffer.is_empty()
        && app.thread_compose_thread_id.as_deref() == Some(thread_ctx.thread.id.as_str())
    {
        let text = format!("> {}", app.thread_compose_buffer);
        let available_w = inner.width.saturating_sub(2);
        super::super::form::measure_wrapped_height(&text, available_w).saturating_add(1)
    } else if choices_visible {
        // choices: 1 header + 1 per choice + 1 compose hint
        (app.quick_reply_choices.len() as u16 + 2).max(3)
    } else {
        3 // default: label + placeholder
    };
    let max_compose = (inner.height * 2 / 5).max(4);
    let compose_height = compose_content_height.clamp(3, max_compose);

    // Compute header height: 1 base line (metadata) + optional pipeline strip.
    // The thread title is already shown in the slim session header above,
    // so we only show runtime/source metadata + workflow pipeline here.
    let pipeline = workflow_pipeline_line(thread_ctx, &app.theme, inner.width);
    let header_height: u16 = if pipeline.is_some() { 2 } else { 1 };

    // Compute approval banner height
    let approval_banner = workflow_approval_banner(thread_ctx, &app.theme, inner.width);
    let banner_height: u16 = approval_banner.as_ref().map_or(0, |b| b.len() as u16);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(8),
            Constraint::Length(banner_height),
            Constraint::Length(compose_height),
        ])
        .split(inner);

    let session_state = thread_session_state(app, &thread_ctx.thread);
    // Single metadata line: runtime + source
    let mut header = vec![
        Line::from(vec![
            Span::styled(
                thread_ctx.runtime_summary(),
                Style::default().fg(app.theme.text_secondary),
            ),
            Span::styled("  ", Style::default()),
            Span::styled(
                format!(
                    "source: {}",
                    if let Some(github_item) = thread_ctx.github_item.as_ref() {
                        format!(
                            "{} #{} {}",
                            github_item.kind, github_item.number, github_item.url
                        )
                    } else if let Some(task_id) = thread_ctx.thread.task_id.as_deref() {
                        format!("task {task_id}")
                    } else {
                        "ad hoc".to_string()
                    }
                ),
                Style::default().fg(app.theme.text_secondary),
            ),
        ]),
    ];
    if let Some(pipeline_line) = pipeline {
        header.push(pipeline_line);
    }
    frame.render_widget(
        Paragraph::new(header)
            .style(app.theme.main_surface())
            .wrap(Wrap { trim: false }),
        vertical[0],
    );

    // Prefer JSONL conversation cache when available for this session.
    // Use pre-built lines cache to avoid rebuilding on every 60fps frame.
    let width = vertical[1].width;
    let jsonl_lines: Option<Vec<Line<'static>>> =
        thread_ctx.thread.session_id.as_deref().and_then(|sid| {
            let conv = app
                .conversation_cache
                .as_ref()
                .filter(|c| c.session_id == sid && !c.entries.is_empty())?;

            // Check if cached lines are still valid
            if let Some(ref cached) = app.cached_chat_lines {
                if cached.session_id == sid && cached.entry_count == conv.entries.len() {
                    return Some(cached.lines.clone());
                }
            }

            // Rebuild and cache
            let lines = build_jsonl_conversation_lines(&conv.entries, &app.theme, width);
            app.cached_chat_lines = Some(super::super::app::CachedChatLines {
                session_id: sid.to_string(),
                entry_count: conv.entries.len(),
                lines: lines.clone(),
            });
            Some(lines)
        });

    let has_jsonl = jsonl_lines.is_some();
    let mut timeline = jsonl_lines.unwrap_or_else(|| build_thread_timeline(app, thread_ctx));

    // For sessions without JSONL (Codex, etc.), show live PTY content
    // in the chat view so the user can see what the provider is doing.
    if !has_jsonl {
        if let Some(ref sid) = thread_ctx.thread.session_id {
            let pty_content: Option<Vec<String>> = app.tabs.iter().find_map(|tab| {
                let super::super::app::Tab::Session {
                    session_id,
                    terminals,
                    ..
                } = tab
                else {
                    return None;
                };
                if session_id != sid {
                    return None;
                }
                terminals.with_claude_live_screen(|screen| {
                    let rows = screen.size().0;
                    let cols = screen.size().1;
                    let mut lines: Vec<String> = Vec::new();
                    for row in 0..rows {
                        let line = screen.contents_between(row, 0, row, cols);
                        lines.push(line);
                    }
                    // Trim trailing empty lines
                    while lines.last().is_some_and(|l| l.trim().is_empty()) {
                        lines.pop();
                    }
                    lines
                })
            });

            if let Some(ref content) = pty_content {
                if !content.is_empty() {
                    timeline.push(Line::from(""));
                    timeline.push(Line::from(Span::styled(
                        format!(
                            "  \u{2500}\u{2500}\u{2500} {} Live Terminal \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                            spinner_char()
                        ),
                        Style::default()
                            .fg(app.theme.accent_secondary)
                            .add_modifier(Modifier::DIM),
                    )));
                    timeline.push(Line::from(""));
                    for line in content {
                        timeline.push(Line::from(Span::styled(
                            format!("  {line}"),
                            Style::default().fg(app.theme.text_primary),
                        )));
                    }
                }
            }
        }
    }

    // Append live "Working..." indicator when the agent is busy (JSONL sessions only).
    if has_jsonl {
        if let Some(ref sid) = thread_ctx.thread.session_id {
            let pty_preview = app.pty_activity_preview.get(sid.as_str());
            let db_working = app
                .sessions
                .iter()
                .any(|s| s.id == *sid && s.claude_status == crate::store::ClaudeStatus::Working);
            let not_idle = !app.pty_idle_sessions.contains(sid.as_str());
            let is_working = db_working && not_idle;
            if is_working {
                timeline.push(Line::from(""));
                timeline.push(Line::from(Span::styled(
                    "  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                    Style::default()
                        .fg(app.theme.border_unfocused)
                        .add_modifier(Modifier::DIM),
                )));
                timeline.push(Line::from(Span::styled(
                    format!("  {} Agent working...", spinner_char()),
                    Style::default()
                        .fg(app.theme.status_working)
                        .add_modifier(Modifier::BOLD),
                )));
                if let Some(lines) = pty_preview {
                    for line in lines.iter().take(6) {
                        timeline.push(Line::from(Span::styled(
                            format!("    {line}"),
                            Style::default()
                                .fg(app.theme.text_secondary)
                                .add_modifier(Modifier::DIM),
                        )));
                    }
                }
            }
        }
    }

    let timeline_paragraph = Paragraph::new(timeline)
        .style(app.theme.main_surface())
        .wrap(Wrap { trim: false });
    // Use visual line count (after wrapping) for scroll calculation
    let visual_lines = timeline_paragraph.line_count(vertical[1].width) as u16;
    let max_scroll = visual_lines.saturating_sub(vertical[1].height);
    let transcript_scroll = if app.session_chat_auto_scroll {
        max_scroll
    } else {
        max_scroll.saturating_sub(app.session_chat_scroll)
    };
    frame.render_widget(
        timeline_paragraph.scroll((transcript_scroll, 0)),
        vertical[1],
    );

    // Render approval banner between timeline and compose when waiting
    if let Some(banner_lines) = approval_banner {
        frame.render_widget(
            Paragraph::new(banner_lines).style(app.theme.main_surface()),
            vertical[2],
        );
    }

    let compose_focused = app.input_mode == InputMode::ThreadCompose
        && app.thread_compose_thread_id.as_deref() == Some(thread_ctx.thread.id.as_str());
    let compose_block = Block::default()
        .borders(Borders::TOP)
        .border_style(if compose_focused {
            app.theme.focused_border()
        } else {
            app.theme.unfocused_border()
        })
        .style(app.theme.main_surface());
    let compose_inner = compose_block.inner(vertical[3]);
    frame.render_widget(compose_block, vertical[3]);

    // Build compose lines — show quick-reply choices only when Claude is idle
    let has_choices = !compose_focused && choices_visible;

    let compose_lines: Vec<Line<'_>> = if has_choices {
        // Show selectable choices instead of plain compose prompt
        let mut lines = vec![Line::from(vec![
            Span::styled(
                "Reply ",
                Style::default()
                    .fg(app.theme.accent_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "pick an option or press i to compose",
                Style::default().fg(app.theme.text_secondary),
            ),
        ])];
        for (i, choice) in app.quick_reply_choices.iter().enumerate() {
            let num = i + 1;
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {num}"),
                    Style::default()
                        .fg(app.theme.accent_tertiary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {choice}"),
                    Style::default().fg(app.theme.text_primary),
                ),
            ]));
        }
        lines
    } else {
        let has_queued = app
            .queued_compose_message
            .as_ref()
            .is_some_and(|(tid, _, _)| tid == &thread_ctx.thread.id);
        let compose_text = if compose_focused {
            format!(
                "> {}",
                format_with_cursor(&app.input_buffer, app.input_cursor)
            )
        } else if has_queued {
            let msg = &app.queued_compose_message.as_ref().unwrap().1;
            let preview = if msg.len() > 60 {
                format!("{}...", &msg[..57])
            } else {
                msg.clone()
            };
            format!("> {preview}")
        } else if !app.thread_compose_buffer.is_empty()
            && app.thread_compose_thread_id.as_deref() == Some(thread_ctx.thread.id.as_str())
        {
            format!("> {}", app.thread_compose_buffer)
        } else if session_state == "live" {
            "> Type a message to continue the conversation.".to_string()
        } else {
            "> Type a message here. Press l to continue the thread if no live provider is attached."
                .to_string()
        };
        let (reply_label, reply_color) = if has_queued {
            (
                "Reply  \u{23F3} queued — Ctrl+C to interrupt and send now",
                app.theme.status_paused,
            )
        } else {
            (
                "Reply",
                if compose_focused {
                    app.theme.accent_tertiary
                } else {
                    app.theme.accent_secondary
                },
            )
        };
        let mut reply_spans = vec![Span::styled(
            reply_label,
            Style::default()
                .fg(reply_color)
                .add_modifier(Modifier::BOLD),
        )];
        if app.clipboard_has_image {
            reply_spans.push(Span::styled(
                "  Ctrl+V to attach image",
                Style::default()
                    .fg(app.theme.text_secondary)
                    .add_modifier(Modifier::DIM),
            ));
        }
        vec![
            Line::from(reply_spans),
            Line::from(Span::styled(
                compose_text,
                Style::default().fg(if compose_focused || has_queued {
                    app.theme.text_primary
                } else {
                    app.theme.text_secondary
                }),
            )),
        ]
    };

    let compose_paragraph = Paragraph::new(compose_lines)
        .style(app.theme.main_surface())
        .wrap(Wrap { trim: false });

    // Scroll the compose area to keep the cursor visible
    let total_lines = compose_paragraph.line_count(compose_inner.width) as u16;
    let visible_lines = compose_inner.height;
    let compose_scroll = total_lines.saturating_sub(visible_lines);
    frame.render_widget(compose_paragraph.scroll((compose_scroll, 0)), compose_inner);

    // Slash command autocomplete popup
    if !app.slash_suggestions.is_empty() && compose_focused {
        let popup_height = (app.slash_suggestions.len() as u16 + 1).min(8);
        let popup_width = 32u16.min(compose_inner.width);
        let popup_y = compose_inner.y.saturating_sub(popup_height);
        let popup_area = Rect::new(compose_inner.x, popup_y, popup_width, popup_height);

        frame.render_widget(Clear, popup_area);
        let mut lines: Vec<Line<'_>> = Vec::new();
        for (i, cmd) in app.slash_suggestions.iter().enumerate() {
            let selected = i == app.slash_suggestion_index;
            let style = if selected {
                app.theme.selected_fill()
            } else {
                Style::default().fg(app.theme.text_primary)
            };
            lines.push(Line::from(Span::styled(format!(" /{cmd}"), style)));
        }
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(app.theme.accent_primary))
                    .style(app.theme.overlay_surface()),
            ),
            popup_area,
        );
    }
}

fn build_thread_timeline(app: &App, thread_ctx: &SelectedThreadContext) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if thread_ctx.messages.is_empty() {
        lines.push(Line::from(Span::styled(
            "No conversation yet.",
            Style::default()
                .fg(app.theme.text_primary)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "Start typing to add context or continue the thread.",
            Style::default().fg(app.theme.text_secondary),
        )));
        return lines;
    }

    for message in thread_ctx.messages.iter().rev().take(24).rev() {
        // Render [Workflow] Stage: messages as styled dividers
        if message.role == "system"
            && let Some(stage_name) = message.content.trim().strip_prefix("[Workflow] Stage: ")
        {
            let stage_name = stage_name.trim().to_string();
            // Look up stage status from the workflow_stages context
            let (icon, color, label) = thread_ctx
                .workflow_stages
                .iter()
                .find(|s| s.stage_name == stage_name)
                .map_or(
                    (
                        "\u{25C9}".to_string(),
                        app.theme.status_working,
                        "running".to_string(),
                    ),
                    |s| match s.status {
                        WorkflowStageStatus::Completed => (
                            "\u{2713}".to_string(),
                            app.theme.status_done,
                            "completed".to_string(),
                        ),
                        WorkflowStageStatus::Running => (
                            "\u{25C9}".to_string(),
                            app.theme.status_working,
                            "running".to_string(),
                        ),
                        WorkflowStageStatus::WaitingApproval => (
                            "\u{23F8}".to_string(),
                            app.theme.status_paused,
                            "waiting approval".to_string(),
                        ),
                        WorkflowStageStatus::Pending => (
                            "\u{25CB}".to_string(),
                            app.theme.status_pending,
                            "pending".to_string(),
                        ),
                        WorkflowStageStatus::Failed => (
                            "\u{2717}".to_string(),
                            app.theme.status_error,
                            "failed".to_string(),
                        ),
                        WorkflowStageStatus::Skipped => (
                            "\u{2212}".to_string(),
                            app.theme.text_secondary,
                            "skipped".to_string(),
                        ),
                    },
                );
            let rule = "\u{2500}".repeat(3);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {rule} {icon} Stage: "),
                    Style::default().fg(color),
                ),
                Span::styled(
                    format!("{stage_name} "),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{label} "), Style::default().fg(color)),
                Span::styled(
                    "\u{2500}".repeat(20),
                    Style::default().fg(color).add_modifier(Modifier::DIM),
                ),
            ]));

            // Show stage output summary below the divider if available
            if let Some(stage) = thread_ctx
                .workflow_stages
                .iter()
                .find(|s| s.stage_name == stage_name)
            {
                if let Some(ref summary) = stage.output_summary {
                    let preview = if summary.len() > 300 {
                        format!("{}...", &summary[..297])
                    } else {
                        summary.clone()
                    };
                    for summary_line in preview.lines().take(6) {
                        lines.push(Line::from(Span::styled(
                            format!("    {summary_line}"),
                            Style::default()
                                .fg(app.theme.text_secondary)
                                .add_modifier(Modifier::DIM),
                        )));
                    }
                }
            }

            lines.push(Line::from(""));
            continue;
        }

        let (role_label, role_style, body_style) = match message.role.as_str() {
            "user" => (
                "You",
                Style::default()
                    .fg(app.theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(app.theme.text_primary),
            ),
            "assistant" => (
                "Agent",
                Style::default()
                    .fg(app.theme.accent_tertiary)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(app.theme.text_primary),
            ),
            "system" => (
                "System",
                Style::default()
                    .fg(app.theme.accent_secondary)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(app.theme.text_secondary),
            ),
            _ => (
                "Message",
                Style::default()
                    .fg(app.theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(app.theme.text_primary),
            ),
        };
        lines.push(Line::from(vec![
            Span::styled(role_label.to_string(), role_style),
            Span::styled(
                format!("  {}", compact_timestamp(&message.created_at)),
                Style::default().fg(app.theme.text_secondary),
            ),
        ]));
        let trimmed = message.content.trim();
        if trimmed.is_empty() {
            lines.push(Line::from(Span::styled("  ", body_style)));
        } else {
            for line in trimmed.lines() {
                let mut spans = vec![Span::raw("  ")];
                spans.extend(spans_with_urls(line, body_style, app.theme.pr_link));
                lines.push(Line::from(spans));
            }
        }
        if !message.attachments.is_empty() {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("attachments: {}", message.attachments.join(", ")),
                    Style::default().fg(app.theme.text_secondary),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Show session status indicator
    if let Some(session_id) = thread_ctx.thread.session_id.as_deref() {
        let session = app.sessions.iter().find(|s| s.id == session_id);
        let is_working = session
            .is_some_and(|s| s.claude_status == crate::store::ClaudeStatus::Working)
            && !app.pty_idle_sessions.contains(session_id);
        let is_idle = session.is_some_and(|s| s.claude_status == crate::store::ClaudeStatus::Idle)
            || app.pty_idle_sessions.contains(session_id);
        let is_paused = app.paused_sessions.contains(session_id);
        let is_waiting = app.waiting_sessions.contains(session_id);

        if is_paused {
            lines.push(Line::from(vec![
                Span::styled(
                    "\u{23F8} Needs approval ",
                    Style::default()
                        .fg(app.theme.status_paused)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Ctrl+O: terminal",
                    Style::default().fg(app.theme.text_secondary),
                ),
            ]));
        } else if is_waiting {
            lines.push(Line::from(vec![
                Span::styled(
                    "\u{23F3} Agent asked a question ",
                    Style::default()
                        .fg(app.theme.status_waiting)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Ctrl+O: terminal",
                    Style::default().fg(app.theme.text_secondary),
                ),
            ]));
        } else if is_working {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", super::spinner_char()),
                    Style::default()
                        .fg(app.theme.spinner)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("Working... ", Style::default().fg(app.theme.text_secondary)),
                Span::styled(
                    "Ctrl+O: terminal  Esc: exit compose",
                    Style::default().fg(app.theme.border_unfocused),
                ),
            ]));
            // Show live PTY activity preview
            if let Some(preview_lines) = app.pty_activity_preview.get(session_id) {
                for line in preview_lines {
                    lines.push(Line::from(Span::styled(
                        format!("  {line}"),
                        Style::default()
                            .fg(app.theme.text_secondary)
                            .add_modifier(Modifier::DIM),
                    )));
                }
            }
        } else if is_idle {
            lines.push(Line::from(Span::styled(
                "> Ready for instructions",
                Style::default().fg(app.theme.text_secondary),
            )));
        }
    }

    lines
}

fn compact_timestamp(timestamp: &str) -> String {
    let trimmed = timestamp.trim_end_matches('Z');
    let normalized = trimmed.replace('T', " ");
    truncate(&normalized, 16)
}

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[^\s)<>\]]+").expect("URL regex is valid"));

/// Split a line of text into spans, highlighting URLs with underline + link color.
fn spans_with_urls<'a>(
    text: &str,
    base_style: Style,
    link_color: ratatui::style::Color,
) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    let mut last_end = 0;

    for mat in URL_RE.find_iter(text) {
        if mat.start() > last_end {
            spans.push(Span::styled(
                text[last_end..mat.start()].to_string(),
                base_style,
            ));
        }
        spans.push(Span::styled(
            mat.as_str().to_string(),
            Style::default()
                .fg(link_color)
                .add_modifier(Modifier::UNDERLINED),
        ));
        last_end = mat.end();
    }

    if last_end < text.len() {
        spans.push(Span::styled(text[last_end..].to_string(), base_style));
    } else if spans.is_empty() {
        spans.push(Span::styled(text.to_string(), base_style));
    }

    spans
}

fn thread_source_label(thread: &crate::store::Thread) -> &'static str {
    if thread.github_item_id.is_some() {
        "github"
    } else if thread.task_id.is_some() {
        "task"
    } else {
        "adhoc"
    }
}

fn thread_session_state(app: &App, thread: &crate::store::Thread) -> &'static str {
    let Some(session_id) = thread.session_id.as_deref() else {
        return "no session";
    };

    let session = app.sessions.iter().find(|session| session.id == session_id);
    match session {
        Some(session) if session.closed_at.is_some() => "closed",
        Some(session) => {
            // Check if a session tab exists and Claude pane is alive
            let has_live_tab = app.tabs.iter().any(|tab| {
                matches!(tab, super::super::app::Tab::Session { session_id: sid, terminals, .. }
                    if sid == session_id
                    && terminals.with_claude_live_screen(|_| ()).is_some())
            });

            if !has_live_tab {
                // No tab or Claude pane exited
                return "claude exited";
            }

            if app.pty_idle_sessions.contains(session_id) {
                "claude exited"
            } else if app.paused_sessions.contains(session_id) {
                "needs approval"
            } else if app.waiting_sessions.contains(session_id) {
                "waiting for input"
            } else {
                match session.claude_status {
                    crate::store::ClaudeStatus::Idle => "idle",
                    crate::store::ClaudeStatus::Working => "working",
                    crate::store::ClaudeStatus::Interrupted => "interrupted",
                    crate::store::ClaudeStatus::Done => "done",
                    crate::store::ClaudeStatus::Error => "error",
                }
            }
        }
        None => "no session",
    }
}

fn draw_agents_view(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(Line::from(vec![Span::styled(
            " Agents ",
            Style::default()
                .fg(app.theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        )]))
        .borders(Borders::ALL)
        .border_style(if app.focus == Focus::Tasks {
            app.theme.focused_border()
        } else {
            app.theme.unfocused_border()
        })
        .style(app.theme.main_surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut provider_counts = std::collections::BTreeMap::<String, usize>::new();
    let mut runtime_ready = 0usize;
    let mut workflow_attached = 0usize;
    for thread in &app.threads {
        *provider_counts
            .entry(thread.provider_kind.to_string())
            .or_default() += 1;
        if thread.runtime_profile.is_some() {
            runtime_ready += 1;
        }
        if thread.workflow_run_id.is_some() {
            workflow_attached += 1;
        }
    }

    let mut lines = vec![
        Line::from(Span::styled(
            format!(
                "  {} thread(s) in {}",
                app.threads.len(),
                app.selected_project()
                    .map_or("workspace", |project| project.name.as_str())
            ),
            Style::default().fg(app.theme.text_primary),
        )),
        Line::from(""),
    ];
    for (provider, count) in provider_counts {
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(provider, Style::default().fg(app.theme.text_primary)),
            Span::styled(
                format!("  {count} thread(s)"),
                Style::default().fg(app.theme.text_secondary),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("  runtime-enabled threads: {runtime_ready}"),
        Style::default().fg(app.theme.text_primary),
    )));
    lines.push(Line::from(Span::styled(
        format!("  workflow-attached threads: {workflow_attached}"),
        Style::default().fg(app.theme.text_primary),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .style(app.theme.main_surface())
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn draw_settings_view(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(Line::from(vec![Span::styled(
            " Settings ",
            Style::default()
                .fg(app.theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        )]))
        .borders(Borders::ALL)
        .border_style(if app.focus == Focus::Tasks {
            app.theme.focused_border()
        } else {
            app.theme.unfocused_border()
        })
        .style(app.theme.main_surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(0)])
        .split(inner);

    draw_settings_sections(frame, app, sections[0]);
    draw_settings_detail(frame, app, sections[1]);
}

fn draw_settings_sections(frame: &mut Frame, app: &App, area: Rect) {
    let block =
        Block::default()
            .borders(Borders::RIGHT)
            .border_style(if app.focus == Focus::Tasks {
                app.theme.focused_border()
            } else {
                app.theme.unfocused_border()
            });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![Line::from(Span::styled(
        " Sections",
        Style::default()
            .fg(app.theme.accent_secondary)
            .add_modifier(Modifier::BOLD),
    ))];

    for (idx, section) in SettingsSection::ALL.iter().enumerate() {
        let selected = idx == app.settings_section_index;
        let style = if selected {
            app.theme.selected_fill()
        } else {
            Style::default().fg(app.theme.text_primary)
        };
        let marker = if selected && app.focus == Focus::Tasks {
            ">"
        } else if selected {
            "*"
        } else {
            " "
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker} "), style),
            Span::styled(section.label(), style),
        ]));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .style(app.theme.main_surface())
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn draw_settings_detail(frame: &mut Frame, app: &App, area: Rect) {
    let inner = Rect::new(
        area.x.saturating_add(2),
        area.y,
        area.width.saturating_sub(3),
        area.height,
    );
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let section = SettingsSection::ALL
        .get(app.settings_section_index)
        .copied()
        .unwrap_or(SettingsSection::AiProviders);
    let github_repos = app.store.list_github_repos().unwrap_or_default();
    let workflow_defs = app.store.list_workflow_defs().unwrap_or_default();
    let profiles = app.config.providers.len();
    let tools = &app.tool_status;
    let _settings_path = dirs::home_dir().map_or_else(
        || "~/.claude/settings.json".to_string(),
        |home| home.join(".claude/settings.json").display().to_string(),
    );
    let theme = &app.theme;

    // Helper styles
    let heading = Style::default()
        .fg(theme.accent_primary)
        .add_modifier(Modifier::BOLD);
    let label_s = Style::default().fg(theme.text_secondary);
    let value_s = Style::default()
        .fg(theme.text_primary)
        .add_modifier(Modifier::BOLD);
    let key_s = Style::default().fg(theme.accent_secondary);
    let on_s = Style::default().fg(theme.status_done);
    let off_s = Style::default().fg(theme.text_secondary);
    let check_ok = Style::default().fg(theme.status_done);
    let check_fail = Style::default().fg(theme.status_error);
    let dim = Style::default().fg(theme.text_secondary);

    // Use styled Lines for all sections now
    let mut lines: Vec<Line<'_>> = Vec::new();
    let mut text = String::new();

    match section {
        SettingsSection::AiProviders => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Claude", heading)));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Model        ", label_s),
                Span::styled(&app.config.claude.model, value_s),
                Span::styled("  ", dim),
                Span::styled("[m]", key_s),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Effort       ", label_s),
                Span::styled(&app.config.claude.effort, value_s),
                Span::styled("  ", dim),
                Span::styled("[e]", key_s),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Remote mode  ", label_s),
                if app.config.remote_enabled {
                    Span::styled("enabled", on_s)
                } else {
                    Span::styled("disabled", off_s)
                },
                Span::styled("  ", dim),
                Span::styled("[r]", key_s),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Auto-update  ", label_s),
                if app.config.auto_update {
                    Span::styled("enabled", on_s)
                } else {
                    Span::styled("disabled", off_s)
                },
                Span::styled("  ", dim),
                Span::styled("[u]", key_s),
            ]));

            if profiles > 0 {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("Provider Profiles", heading)));
                lines.push(Line::from(""));
                for name in app.config.providers.keys().take(8) {
                    lines.push(Line::from(Span::styled(
                        format!("  \u{25CB} {name}"),
                        Style::default().fg(theme.text_primary),
                    )));
                }
            }

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Tools", heading)));
            lines.push(Line::from(""));

            let tool_items: [(&str, bool); 4] = [
                ("Claude Code CLI", tools.claude),
                ("OpenAI Codex CLI", tools.codex),
                ("Node.js", tools.node),
                ("GitHub CLI", tools.gh),
            ];
            for (name, ok) in tool_items {
                let (icon, style) = if ok {
                    ("\u{2713}", check_ok)
                } else {
                    ("\u{2717}", check_fail)
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {icon} "), style),
                    Span::styled(name, Style::default().fg(theme.text_primary)),
                ]));
            }
        }
        SettingsSection::Permissions => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Claude Permissions", heading)));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Allow rules  ", label_s),
                Span::styled(app.config.permissions.allow.len().to_string(), value_s),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Deny rules   ", label_s),
                Span::styled(app.config.permissions.deny.len().to_string(), value_s),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Ask rules    ", label_s),
                Span::styled(app.config.permissions.ask.len().to_string(), value_s),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Enter or c   ", key_s),
                Span::styled("Run configure wizard", dim),
            ]));
            if app.config.rtk.enabled {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("RTK", heading)));
                lines.push(Line::from(""));
                let (icon, style) = if tools.rtk {
                    ("\u{2713}", check_ok)
                } else {
                    ("\u{2717}", check_fail)
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {icon} "), style),
                    Span::styled(
                        if tools.rtk { "installed" } else { "missing" },
                        Style::default().fg(theme.text_primary),
                    ),
                ]));
            }
        }
        SettingsSection::GitHub => {
            let auth_backend = if app.github_status.authenticated {
                if app.github_status.custom_app_configured {
                    "Custom GitHub App"
                } else {
                    "Claustre GitHub App"
                }
            } else if app.github_status.gh_authenticated {
                "GitHub CLI"
            } else if app.github_status.configured {
                if app.github_status.custom_app_configured {
                    "Custom GitHub App"
                } else {
                    "Claustre GitHub App"
                }
            } else if tools.gh {
                "GitHub CLI"
            } else {
                "Not available"
            };
            let connected_account = app
                .github_status
                .user_login
                .as_deref()
                .or(app.github_status.gh_user_login.as_deref())
                .unwrap_or("not connected");
            let _ = writeln!(text, " GitHub");
            let _ = writeln!(text);
            let _ = writeln!(text, " Setup");
            let _ = writeln!(text, "   auth backend: {auth_backend}");
            let _ = writeln!(
                text,
                "   linked repo: {}",
                app.selected_project()
                    .map_or("none", |project| project.name.as_str())
            );
            let _ = writeln!(
                text,
                "   connect flow: {}",
                if app.github_status.configured {
                    "browser device flow"
                } else if tools.gh {
                    "GitHub CLI browser login / refresh"
                } else {
                    "install GitHub CLI or configure a custom app"
                }
            );
            let _ = writeln!(text);
            let _ = writeln!(text, " Connection");
            let _ = writeln!(
                text,
                "   status: {}",
                if app.github_status.authenticated || app.github_status.gh_authenticated {
                    "connected"
                } else {
                    "not connected"
                }
            );
            let _ = writeln!(text, "   account: {connected_account}");
            let _ = writeln!(
                text,
                "   app session: {}",
                if app.github_status.authenticated {
                    "active"
                } else {
                    "inactive"
                }
            );
            let _ = writeln!(
                text,
                "   gh CLI: {}",
                if app.github_status.gh_authenticated {
                    "authenticated"
                } else if tools.gh {
                    "available"
                } else {
                    "missing"
                }
            );
            if app.github_status.gh_authenticated {
                let scopes = if app.github_status.gh_scopes.is_empty() {
                    "unknown".to_string()
                } else {
                    app.github_status.gh_scopes.join(", ")
                };
                let _ = writeln!(text, "   gh scopes: {scopes}");
                let _ = writeln!(
                    text,
                    "   projects v2: {}",
                    if app.github_status.gh_has_project_scope {
                        "ready"
                    } else {
                        "missing read:project"
                    }
                );
            }
            if app.github_status.authenticated {
                let _ = writeln!(
                    text,
                    "   app scope: {}",
                    if app.github_status.app_has_project_scope {
                        "includes project access"
                    } else {
                        "project scope unknown or missing"
                    }
                );
            }
            let _ = writeln!(
                text,
                "   token expiry: {}",
                app.github_status.expires_at.as_deref().unwrap_or("unknown")
            );
            if let Some(ref prompt) = app.github_auth_prompt {
                let _ = writeln!(text);
                let _ = writeln!(text, " Device flow");
                let _ = writeln!(text, "   code: {}", prompt.user_code);
                let _ = writeln!(text, "   verify: {}", prompt.verification_uri);
                let _ = writeln!(text, "   expires: {}", prompt.expires_at);
            }
            if let Some(ref error) = app.github_auth_error {
                let _ = writeln!(text);
                let _ = writeln!(text, " Last error");
                let _ = writeln!(text, "   {error}");
            }
            let _ = writeln!(text);
            let _ = writeln!(text, " Sync");
            let _ = writeln!(text, "   cached repos: {}", github_repos.len());
            let default_project_label = app
                .config
                .github_app
                .default_project_id
                .as_deref()
                .map_or_else(
                    || "none".to_string(),
                    |project_id| {
                        app.github_projects_v2
                            .iter()
                            .find(|project| {
                                project.id == project_id
                                    || project.project_number.to_string() == project_id
                                    || project.title.eq_ignore_ascii_case(project_id)
                            })
                            .map_or_else(
                                || project_id.to_string(),
                                crate::store::GitHubProjectV2Cache::display_label,
                            )
                    },
                );
            let _ = writeln!(text, "   default board: {default_project_label}");
            if app.config.github_app.default_project_id.is_none()
                && let Some(project) = app.github_projects_v2.first()
            {
                let _ = writeln!(
                    text,
                    "   board fallback: {} (until you choose a default)",
                    project.display_label()
                );
            }
            let _ = writeln!(
                text,
                "   cached projects v2: {}",
                app.github_projects_v2.len()
            );
            let _ = writeln!(text, "   y sync selected repo");
            if !app.github_projects_v2.is_empty() {
                let _ = writeln!(text, "   g choose default board");
                for (idx, project) in app.github_projects_v2.iter().take(6).enumerate() {
                    let project_number = project.project_number.to_string();
                    let marker = if app.config.github_app.default_project_id.as_deref()
                        == Some(project_number.as_str())
                    {
                        " default"
                    } else {
                        ""
                    };
                    let cursor = if idx == app.github_project_index {
                        "▸"
                    } else {
                        " "
                    };
                    let _ = writeln!(text, " {} {}{}", cursor, project.display_label(), marker);
                }
                if app.github_projects_v2.len() > 6 {
                    let _ = writeln!(
                        text,
                        "   … {} more",
                        app.github_projects_v2.len().saturating_sub(6)
                    );
                }
            }
            if app.github_status.authenticated || app.github_status.configured {
                let _ = writeln!(text);
                let _ = writeln!(text, " App install");
                let _ = writeln!(
                    text,
                    "   install URL: {}",
                    crate::github_app::install_url(&app.config.github_app)
                        .unwrap_or_else(|| "not available".to_string())
                );
                let _ = writeln!(
                    text,
                    "   default installation: {}",
                    app.config
                        .github_app
                        .default_installation_id
                        .as_deref()
                        .unwrap_or("none")
                );
                let _ = writeln!(
                    text,
                    "   installations: {}",
                    if app.github_installations.is_empty() {
                        if app.github_status.authenticated {
                            "not loaded"
                        } else {
                            "connect first"
                        }
                    } else {
                        "loaded"
                    }
                );
                if app.github_installations.is_empty() {
                    let _ = writeln!(text, "   none loaded");
                } else {
                    for (idx, installation) in app.github_installations.iter().take(8).enumerate() {
                        let installation_id = installation.id.to_string();
                        let marker = if app.config.github_app.default_installation_id.as_deref()
                            == Some(installation_id.as_str())
                        {
                            " default"
                        } else {
                            ""
                        };
                        let cursor = if idx == app.github_installation_index {
                            "▸"
                        } else {
                            " "
                        };
                        let _ = writeln!(
                            text,
                            " {} {} ({}){}",
                            cursor, installation.account.login, installation_id, marker
                        );
                    }
                    if app.github_installations.len() > 8 {
                        let _ = writeln!(
                            text,
                            "   … {} more",
                            app.github_installations.len().saturating_sub(8)
                        );
                    }
                }
            } else {
                let _ = writeln!(text);
                let _ = writeln!(text, " Claustre App");
                let _ = writeln!(
                    text,
                    "   no bundled GitHub App metadata is configured in this build"
                );
                let _ = writeln!(
                    text,
                    "   Claustre can still connect through GitHub CLI and use Projects v2"
                );
            }
            let _ = writeln!(text);
            let _ = writeln!(text, " Actions");
            let _ = writeln!(text, "   Enter or a connect / refresh GitHub");
            if app.github_status.gh_authenticated && !app.github_status.gh_has_project_scope {
                let _ = writeln!(text, "   grants read:project for Projects v2");
            }
            let _ = writeln!(text, "   f refresh connection");
            if app.github_status.authenticated || app.github_status.configured {
                let _ = writeln!(text, "   o open app install page");
            }
            let _ = writeln!(text, "   y sync selected repo");
            let _ = writeln!(text, "   p choose project");
            let _ = writeln!(text, "   x disconnect");
            let _ = writeln!(
                text,
                "   . {} advanced app settings",
                if app.github_settings_show_advanced {
                    "hide"
                } else {
                    "show"
                }
            );
            if app.github_settings_show_advanced {
                let _ = writeln!(text);
                let _ = writeln!(text, " Advanced custom app");
                let _ = writeln!(
                    text,
                    "   client ID: {}",
                    app.config
                        .github_app
                        .client_id
                        .as_deref()
                        .unwrap_or("not set")
                );
                let _ = writeln!(
                    text,
                    "   app slug: {}",
                    app.config
                        .github_app
                        .app_slug
                        .as_deref()
                        .unwrap_or("not set")
                );
                let _ = writeln!(
                    text,
                    "   install URL override: {}",
                    app.config
                        .github_app
                        .install_url
                        .as_deref()
                        .unwrap_or("not set")
                );
                let _ = writeln!(
                    text,
                    "   relay URL: {}",
                    app.config
                        .github_app
                        .relay_url
                        .as_deref()
                        .unwrap_or("not set")
                );
                let _ = writeln!(
                    text,
                    "   default installation: {}",
                    app.config
                        .github_app
                        .default_installation_id
                        .as_deref()
                        .unwrap_or("none")
                );
                let _ = writeln!(
                    text,
                    "   default board: {}",
                    app.config
                        .github_app
                        .default_project_id
                        .as_deref()
                        .map_or_else(
                            || "none".to_string(),
                            |project_id| {
                                app.github_projects_v2
                                    .iter()
                                    .find(|project| {
                                        project.id == project_id
                                            || project.project_number.to_string() == project_id
                                            || project.title.eq_ignore_ascii_case(project_id)
                                    })
                                    .map_or_else(
                                        || project_id.to_string(),
                                        crate::store::GitHubProjectV2Cache::display_label,
                                    )
                            }
                        )
                );
                let _ = writeln!(text, "   c client ID");
                let _ = writeln!(text, "   s app slug");
                let _ = writeln!(text, "   u install URL");
                let _ = writeln!(text, "   r relay URL");
                let _ = writeln!(text, "   i default installation");
                let _ = writeln!(text, "   g choose default board");
            } else {
                let _ = writeln!(text, "   advanced overrides are hidden");
            }
        }
        SettingsSection::Layout => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Keymap", heading)));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Preset           ", label_s),
                Span::styled(&app.config.ui.keymap.preset, value_s),
                Span::styled("  ", dim),
                Span::styled("[p]", key_s),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Leader key       ", label_s),
                Span::styled(&app.config.ui.keymap.leader, value_s),
                Span::styled("  ", dim),
                Span::styled("[l]", key_s),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Key overrides    ", label_s),
                Span::styled(
                    app.config.ui.keymap.overrides.len().to_string(),
                    Style::default().fg(theme.text_primary),
                ),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Panel Sizes", heading)));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Sidebar width    ", label_s),
                Span::styled(
                    app.workbench_sidebar_width.to_string(),
                    Style::default().fg(theme.text_primary),
                ),
                Span::styled("  [ / ] to resize", dim),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Inspector width  ", label_s),
                Span::styled(
                    app.workbench_inspector_width.to_string(),
                    Style::default().fg(theme.text_primary),
                ),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Session", heading)));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Layout           ", label_s),
                Span::styled(
                    if app.config.layout.is_some() {
                        "custom"
                    } else {
                        "shell | claude"
                    },
                    Style::default().fg(theme.text_primary),
                ),
            ]));
        }
        SettingsSection::Notifications => {
            let toggle = |enabled: bool| -> (Span<'static>, Style) {
                if enabled {
                    (Span::styled("on ", on_s), on_s)
                } else {
                    (Span::styled("off", off_s), off_s)
                }
            };

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Delivery", heading)));
            lines.push(Line::from(""));
            let (voice_span, _) = toggle(app.config.notifications.enabled);
            lines.push(Line::from(vec![
                Span::styled("  Voice notifications  ", label_s),
                voice_span,
                Span::styled("  ", dim),
                Span::styled("[e]", key_s),
            ]));
            let (sys_span, _) = toggle(app.config.notifications.system);
            lines.push(Line::from(vec![
                Span::styled("  System banners       ", label_s),
                sys_span,
                Span::styled("  ", dim),
                Span::styled("[s]", key_s),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Command              ", label_s),
                Span::styled(app.config.notifications.command.clone(), value_s),
                Span::styled("  ", dim),
                Span::styled("[c]", key_s),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Template             ", label_s),
                Span::styled(app.config.notifications.template.clone(), value_s),
                Span::styled("  ", dim),
                Span::styled("[t]", key_s),
            ]));

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Rules", heading)));
            lines.push(Line::from(""));

            let rules: [(&str, bool, &str); 5] = [
                (
                    "Ask-user questions",
                    app.config.notifications.rules.ask_user,
                    "[a]",
                ),
                (
                    "Approval required",
                    app.config.notifications.rules.approval_required,
                    "[p]",
                ),
                (
                    "Run complete",
                    app.config.notifications.rules.run_complete,
                    "[r]",
                ),
                (
                    "Review requested",
                    app.config.notifications.rules.review_requested,
                    "[v]",
                ),
                ("CI failed", app.config.notifications.rules.ci_failed, "[f]"),
            ];
            for (label, enabled, shortcut) in rules {
                let (toggle_span, _) = toggle(enabled);
                lines.push(Line::from(vec![
                    Span::styled(format!("  {label:<23}"), label_s),
                    toggle_span,
                    Span::styled("  ", dim),
                    Span::styled(shortcut, key_s),
                ]));
            }
        }
        SettingsSection::ReviewLoop => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Review Loop", heading)));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Poll interval    ", label_s),
                Span::styled(
                    format!("{}s", app.config.review_loop.poll_interval_secs),
                    value_s,
                ),
                Span::styled("  ", dim),
                Span::styled("[i]", key_s),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Prompt           ", label_s),
                Span::styled(
                    if app.config.review_loop.prompt.is_some() {
                        "custom"
                    } else {
                        "built-in"
                    },
                    Style::default().fg(theme.text_primary),
                ),
                Span::styled("  ", dim),
                Span::styled("[p]", key_s),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Workflows", heading)));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Definitions      ", label_s),
                Span::styled(
                    workflow_defs.len().to_string(),
                    Style::default().fg(theme.text_primary),
                ),
            ]));
        }
        SettingsSection::Sandbox => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Runtime", heading)));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Default profile  ", label_s),
                Span::styled(
                    app.config
                        .runtime
                        .default_profile
                        .as_deref()
                        .unwrap_or("none"),
                    value_s,
                ),
                Span::styled("  ", dim),
                Span::styled("[p]", key_s),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Sandbox path     ", label_s),
                Span::styled(
                    app.config.runtime.sandbox_path.as_deref().unwrap_or("auto"),
                    Style::default().fg(theme.text_primary),
                ),
                Span::styled("  ", dim),
                Span::styled("[s]", key_s),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Attachments dir  ", label_s),
                Span::styled(
                    app.config
                        .runtime
                        .attachments_dir
                        .as_deref()
                        .unwrap_or("default"),
                    Style::default().fg(theme.text_primary),
                ),
                Span::styled("  ", dim),
                Span::styled("[a]", key_s),
            ]));
        }
        SettingsSection::Workflows => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Workflows", heading)));
            lines.push(Line::from(""));
            if workflow_defs.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  No workflows defined. Press [n] to create one.",
                    dim,
                )));
            } else {
                let selected_idx = app.settings_workflow_index;
                for (i, def) in workflow_defs.iter().enumerate() {
                    let is_selected = i == selected_idx;
                    let scope_label = match def.scope.as_str() {
                        "builtin" => "[built-in]",
                        "repo" => "[repo]",
                        _ => "[custom]",
                    };
                    let parsed: Option<crate::workflows::WorkflowDefinition> =
                        serde_yaml::from_str(&def.definition_yaml).ok();
                    let stage_count = parsed.as_ref().map_or(0, |d| d.stages.len());
                    let scope_style = if def.scope == "builtin" { dim } else { on_s };
                    let marker = if is_selected { "\u{25B8} " } else { "  " };
                    let name_style = if is_selected {
                        Style::default()
                            .fg(theme.accent_primary)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.text_primary)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(marker, name_style),
                        Span::styled(format!("{:<24}", def.name), name_style),
                        Span::styled(
                            format!("{stage_count} stages  "),
                            Style::default().fg(theme.text_secondary),
                        ),
                        Span::styled(scope_label, scope_style),
                    ]));

                    // Show stage details for the selected workflow
                    if is_selected {
                        if let Some(ref description) = def.description {
                            lines.push(Line::from(Span::styled(
                                format!("    {description}"),
                                Style::default().fg(theme.text_secondary),
                            )));
                        }
                        if let Some(ref def) = parsed {
                            lines.push(Line::from(""));
                            lines.push(Line::from(Span::styled(
                                "    Stages:",
                                Style::default()
                                    .fg(theme.text_primary)
                                    .add_modifier(Modifier::BOLD),
                            )));
                            for (si, stage) in def.stages.iter().enumerate() {
                                let gate_marker = if stage.gate.is_some() {
                                    " \u{23F8}"
                                } else {
                                    ""
                                };
                                let provider = stage.provider.as_deref().unwrap_or("none");
                                let prompt_hint = stage
                                    .prompt_template
                                    .as_deref()
                                    .map(|p| {
                                        if p.len() > 40 {
                                            format!(" \u{2192} {}...", &p[..37])
                                        } else {
                                            format!(" \u{2192} {p}")
                                        }
                                    })
                                    .unwrap_or_default();
                                lines.push(Line::from(vec![
                                    Span::styled(
                                        format!("    {}. ", si + 1),
                                        Style::default().fg(theme.text_secondary),
                                    ),
                                    Span::styled(
                                        format!("{}{gate_marker}", stage.name),
                                        Style::default().fg(theme.accent_tertiary),
                                    ),
                                    Span::styled(
                                        format!("  [{provider}]"),
                                        Style::default().fg(theme.text_secondary),
                                    ),
                                    Span::styled(
                                        prompt_hint,
                                        Style::default().fg(theme.text_secondary),
                                    ),
                                ]));
                            }
                        }
                        lines.push(Line::from(""));
                    }
                }
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  ", dim),
                Span::styled("[n]", key_s),
                Span::styled(" New  ", dim),
                Span::styled("[e]", key_s),
                Span::styled(" Edit name  ", dim),
                Span::styled("[d]", key_s),
                Span::styled(" Delete  ", dim),
                Span::styled("[j/k]", key_s),
                Span::styled(" Navigate", dim),
            ]));
        }
        SettingsSection::General => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("General", heading)));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Sync auto-push   ", label_s),
                if app.config.sync.auto_push {
                    Span::styled("on ", on_s)
                } else {
                    Span::styled("off", off_s)
                },
                Span::styled("  ", dim),
                Span::styled("[a]", key_s),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  RTK integration  ", label_s),
                if app.config.rtk.enabled {
                    Span::styled("on ", on_s)
                } else {
                    Span::styled("off", off_s)
                },
                Span::styled("  ", dim),
                Span::styled("[r]", key_s),
            ]));
            if let Some(ref warning) = app.config_warning {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("  Warning  ", Style::default().fg(theme.status_error)),
                    Span::styled(warning.as_str(), Style::default().fg(theme.text_secondary)),
                ]));
            }
        }
    }

    if lines.is_empty() {
        frame.render_widget(
            Paragraph::new(text)
                .style(app.theme.main_surface())
                .wrap(Wrap { trim: false }),
            inner,
        );
    } else {
        frame.render_widget(
            Paragraph::new(lines)
                .style(app.theme.main_surface())
                .wrap(Wrap { trim: false }),
            inner,
        );
    }
}

pub(super) fn draw_inspector(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    selected_task: Option<&Task>,
    selected_board_item: Option<&GitHubBoardItem>,
    selected_review_item: Option<&ReviewQueueItem>,
    thread_ctx: Option<&SelectedThreadContext>,
) {
    let block = Block::default()
        .title(" Inspector ")
        .borders(Borders::ALL)
        .border_style(if app.focus == Focus::Inspector {
            app.theme.focused_border()
        } else {
            app.theme.unfocused_border()
        })
        .style(app.theme.inspector_surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Thread context summary at the top of the inspector
    let ctx_height = if thread_ctx.is_some() { 2 } else { 0 };

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(ctx_height),
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(if app.rate_limit_state.is_rate_limited {
                7
            } else {
                5
            }),
        ])
        .split(inner);

    // Thread metadata in the inspector (moved from the header)
    if let Some(ctx) = thread_ctx {
        let mut meta_lines = vec![Line::from(vec![
            Span::styled(
                ctx.identity_summary(),
                Style::default().fg(app.theme.accent_primary),
            ),
            Span::styled(" · ", Style::default().fg(app.theme.border_unfocused)),
            Span::styled(
                ctx.runtime_summary(),
                Style::default().fg(app.theme.accent_secondary),
            ),
        ])];
        meta_lines.push(Line::from(vec![Span::styled(
            ctx.activity_summary(),
            Style::default().fg(app.theme.text_secondary),
        )]));
        frame.render_widget(
            Paragraph::new(meta_lines).style(app.theme.inspector_surface()),
            sections[0],
        );
    }

    let tabs = InspectorTab::ALL
        .iter()
        .map(|tab| {
            let style = if *tab == app.inspector_tab {
                app.theme.chip_style(app.theme.tab_active)
            } else {
                Style::default().fg(app.theme.text_secondary)
            };
            Span::styled(format!(" {} ", tab.label()), style)
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Line::from(tabs)).style(app.theme.inspector_surface()),
        sections[1],
    );

    // Inset the content area for horizontal padding (1 cell each side)
    let content_area = Rect {
        x: sections[2].x.saturating_add(1),
        y: sections[2].y,
        width: sections[2].width.saturating_sub(2),
        height: sections[2].height,
    };

    // Diff and Runtime tabs get special styled rendering; other tabs use plain or markdown text
    if app.inspector_tab == InspectorTab::Diff {
        let max_lines = if app.inspector_expanded { 2000 } else { 180 };
        let raw = diff_content_with_limit(app, selected_review_item, thread_ctx, max_lines);
        let styled = super::overlays::styled_diff_lines(&raw, &app.theme);
        frame.render_widget(
            Paragraph::new(styled)
                .style(app.theme.inspector_surface())
                .scroll((app.inspector_scroll, 0)),
            content_area,
        );
    } else if app.inspector_tab == InspectorTab::Runtime {
        let styled = styled_runtime_lines(selected_review_item, thread_ctx, &app.theme);
        frame.render_widget(
            Paragraph::new(styled)
                .style(app.theme.inspector_surface())
                .scroll((app.inspector_scroll, 0)),
            content_area,
        );
    } else if app.inspector_tab == InspectorTab::Plan {
        let styled = styled_plan_lines(selected_review_item, thread_ctx, &app.theme);
        frame.render_widget(
            Paragraph::new(styled)
                .style(app.theme.inspector_surface())
                .wrap(Wrap { trim: false })
                .scroll((app.inspector_scroll, 0)),
            content_area,
        );
    } else {
        let content = match app.inspector_tab {
            InspectorTab::Issue => issue_content(
                selected_task,
                selected_board_item,
                &app.board_selected_comments,
                selected_review_item,
                thread_ctx,
            ),
            InspectorTab::Tests => tests_content(selected_task, selected_review_item, thread_ctx),
            InspectorTab::Attachments => attachments_content(thread_ctx),
            InspectorTab::Diff | InspectorTab::Runtime | InspectorTab::Plan => unreachable!(),
        };
        match app.inspector_tab {
            InspectorTab::Issue => frame.render_widget(
                Paragraph::new(markdown_from_str(&content))
                    .style(app.theme.inspector_surface())
                    .wrap(Wrap { trim: false })
                    .scroll((app.inspector_scroll, 0)),
                content_area,
            ),
            _ => frame.render_widget(
                Paragraph::new(content)
                    .style(app.theme.inspector_surface())
                    .wrap(Wrap { trim: false })
                    .scroll((app.inspector_scroll, 0)),
                content_area,
            ),
        }
    }

    let token_usage = selected_task.map(|t| (t.input_tokens, t.output_tokens));
    draw_usage_bars(frame, app, sections[3], token_usage);
}

fn issue_content(
    selected_task: Option<&Task>,
    selected_board_item: Option<&GitHubBoardItem>,
    board_comments: &[crate::store::GitHubCommentCache],
    selected_review_item: Option<&ReviewQueueItem>,
    thread_ctx: Option<&SelectedThreadContext>,
) -> String {
    let mut text = String::new();
    if let Some(thread_ctx) = thread_ctx
        && let Some(item) = &thread_ctx.github_item
    {
        let _ = writeln!(text, "# {} #{} [{}]", item.kind, item.number, item.state);
        let _ = writeln!(text, "## {}", item.title);
        let _ = writeln!(text);
        let _ = writeln!(text, "- URL: {}", item.url);
        if let Some(pr) = &thread_ctx.pr_cache {
            if let (Some(head), Some(base)) = (&pr.head_ref, &pr.base_ref) {
                let _ = writeln!(text, "- Branch: `{head}` -> `{base}`");
            }
            let _ = writeln!(
                text,
                "- Reviews: {} top-level, {} inline",
                thread_ctx.review_count, thread_ctx.review_comment_count
            );
            let _ = writeln!(text, "- Comments: {}", thread_ctx.comment_count);
            if let Some(body) = pr.body.as_deref().or(pr.body_text.as_deref()) {
                let _ = writeln!(text);
                text.push_str(body);
            }
        } else if let Some(issue) = &thread_ctx.issue_cache
            && let Some(body) = issue.body.as_deref().or(issue.body_text.as_deref())
        {
            let _ = writeln!(text, "- Comments: {}", thread_ctx.comment_count);
            let _ = writeln!(text);
            text.push_str(body);
        }
        return text;
    }

    if let Some(review_item) = selected_review_item {
        let _ = writeln!(
            text,
            "# Pull request #{} [{}]",
            review_item.github_item.number, review_item.github_item.state
        );
        let _ = writeln!(text, "## {}", review_item.github_item.title);
        let _ = writeln!(text);
        let _ = writeln!(text, "- URL: {}", review_item.github_item.url);
        if let Some(author) = review_item.author_login.as_deref() {
            let _ = writeln!(text, "- Author: `{author}`");
        }
        if let (Some(head), Some(base)) = (
            review_item.pr_cache.head_ref.as_deref(),
            review_item.pr_cache.base_ref.as_deref(),
        ) {
            let _ = writeln!(text, "- Branch: `{head}` -> `{base}`");
        }
        let _ = writeln!(
            text,
            "- Review state: {}{}",
            review_item
                .pr_cache
                .review_decision
                .as_deref()
                .unwrap_or("open")
                .replace('_', " "),
            if review_item.pr_cache.is_draft {
                " (draft)"
            } else {
                ""
            }
        );
        let _ = writeln!(
            text,
            "- Activity: {} comments, {} reviews, {} inline comments",
            review_item.comment_count, review_item.review_count, review_item.review_comment_count
        );
        let _ = writeln!(text);
        if let Some(body) = review_item
            .pr_cache
            .body
            .as_deref()
            .or(review_item.pr_cache.body_text.as_deref())
            .or(review_item
                .issue_cache
                .as_ref()
                .and_then(|issue| issue.body.as_deref()))
            .or(review_item
                .issue_cache
                .as_ref()
                .and_then(|issue| issue.body_text.as_deref()))
        {
            text.push_str(body);
        } else {
            let _ = writeln!(text, "_No PR body cached._");
        }
        if !review_item.requested_reviewer_logins.is_empty() {
            let _ = writeln!(text);
            let _ = writeln!(
                text,
                "Requested reviewers: {}",
                review_item.requested_reviewer_logins.join(", ")
            );
        }
        return text;
    }

    if let Some(issue) = selected_board_item {
        let _ = writeln!(text, "# {} #{} [{}]", issue.kind, issue.number, issue.state);
        let _ = writeln!(text, "## {}", issue.title);
        let _ = writeln!(text);
        let _ = writeln!(text, "- URL: {}", issue.url);

        // Assignees
        if !issue.assignees.is_empty() {
            let logins: Vec<&str> = issue.assignees.iter().map(|a| a.login.as_str()).collect();
            let _ = writeln!(text, "- Assignees: {}", logins.join(", "));
        }

        // Labels
        if !issue.labels.is_empty() {
            let names: Vec<&str> = issue.labels.iter().map(|l| l.name.as_str()).collect();
            let _ = writeln!(text, "- Labels: {}", names.join(", "));
        }

        // Project fields
        if let Some(fields) = issue.project_field_values.as_object()
            && !fields.is_empty()
        {
            let _ = writeln!(text);
            let _ = writeln!(text, "### Project Fields");
            for (name, value) in fields {
                if let Some(value) = value.as_str() {
                    let _ = writeln!(text, "- **{name}**: {value}");
                }
            }
        }

        // Body
        if let Some(body) = issue.body.as_deref() {
            let _ = writeln!(text);
            text.push_str(body);
        } else {
            let _ = writeln!(text, "\n_No body cached for this project item._");
        }

        // Comments timeline
        if !board_comments.is_empty() {
            let _ = writeln!(text);
            let _ = writeln!(text, "---");
            let _ = writeln!(text, "### Comments ({})", board_comments.len());
            for comment in board_comments {
                let author = comment.author_login.as_deref().unwrap_or("unknown");
                let date = &comment.created_at;
                let _ = writeln!(text);
                let _ = writeln!(text, "**@{author}** — {date}");
                if let Some(body) = comment.body_text.as_deref().or(comment.body.as_deref()) {
                    let _ = writeln!(text);
                    text.push_str(body);
                    let _ = writeln!(text);
                }
            }
        }
        return text;
    }

    if let Some(task) = selected_task {
        let _ = writeln!(text, "# Task");
        let _ = writeln!(text, "## {}", task.title);
        let _ = writeln!(text);
        text.push_str(&task.description);
        return text;
    }

    "No issue or thread selected.".to_string()
}

fn diff_content_with_limit(
    app: &mut App,
    selected_review_item: Option<&ReviewQueueItem>,
    thread_ctx: Option<&SelectedThreadContext>,
    max_lines: usize,
) -> String {
    if let Some(thread_ctx) = thread_ctx {
        let Some(worktree_path) = thread_ctx.thread.worktree_path.as_deref() else {
            return "Thread has no worktree yet.".to_string();
        };

        let key = format!(
            "thread:{}:{}:{}:{}",
            thread_ctx.thread.id, thread_ctx.thread.updated_at, worktree_path, max_lines
        );
        let cache_expired = app
            .diff_preview_generated_at
            .is_none_or(|instant| instant.elapsed() >= DIFF_CACHE_TTL);
        if app.diff_preview_cache_key.as_deref() != Some(key.as_str()) || cache_expired {
            app.diff_preview_cache = load_diff_preview(worktree_path);
            app.diff_preview_cache_key = Some(key);
            app.diff_preview_generated_at = Some(Instant::now());
        }

        let mut text = String::new();
        if let Some(pr) = &thread_ctx.pr_cache
            && let (Some(head), Some(base)) = (&pr.head_ref, &pr.base_ref)
        {
            let _ = writeln!(text, "{head} -> {base}");
            let _ = writeln!(text);
        }
        text.push_str(&app.diff_preview_cache);
        return text;
    }

    if let Some(review_item) = selected_review_item {
        let key = format!(
            "review:{}:{}:{}",
            review_item.github_item.id, review_item.pr_cache.cached_at, max_lines
        );
        let cache_expired = app
            .diff_preview_generated_at
            .is_none_or(|instant| instant.elapsed() >= DIFF_CACHE_TTL);
        if app.diff_preview_cache_key.as_deref() != Some(key.as_str()) || cache_expired {
            app.diff_preview_cache =
                load_remote_pr_diff_with_limit(&app.store, review_item, max_lines);
            app.diff_preview_cache_key = Some(key);
            app.diff_preview_generated_at = Some(Instant::now());
        }

        let mut text = String::new();
        if let (Some(head), Some(base)) = (
            review_item.pr_cache.head_ref.as_deref(),
            review_item.pr_cache.base_ref.as_deref(),
        ) {
            let _ = writeln!(text, "{head} -> {base}");
            let _ = writeln!(text);
        }
        text.push_str(&app.diff_preview_cache);
        return text;
    }

    "Select a pull request or thread to inspect the diff.".to_string()
}

fn styled_runtime_lines<'a>(
    selected_review_item: Option<&ReviewQueueItem>,
    thread_ctx: Option<&SelectedThreadContext>,
    theme: &super::super::theme::Theme,
) -> Vec<Line<'a>> {
    let Some(thread_ctx) = thread_ctx else {
        let msg = if selected_review_item.is_some() {
            "Launch a PR thread to boot the worktree runtime and inspect local services."
        } else {
            "No runtime context for the current selection."
        };
        return vec![Line::from(Span::styled(
            msg.to_string(),
            Style::default().fg(theme.text_secondary),
        ))];
    };

    let mut lines: Vec<Line<'a>> = Vec::new();

    // Summary line
    lines.push(Line::from(Span::styled(
        thread_ctx.runtime_summary(),
        Style::default()
            .fg(theme.accent_secondary)
            .add_modifier(Modifier::BOLD),
    )));

    // Last error
    if let Some(state) = &thread_ctx.runtime_state
        && let Some(error) = &state.last_error
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("last error: {error}"),
            Style::default().fg(theme.status_error),
        )));
    }

    // Services with colored status
    if !thread_ctx.runtime_services.is_empty() {
        lines.push(Line::from(""));
        for service in &thread_ctx.runtime_services {
            let (status_str, status_color) = match service.status {
                RuntimeServiceStatus::Running | RuntimeServiceStatus::Healthy => {
                    (service.status.as_str(), theme.status_done)
                }
                RuntimeServiceStatus::Starting => (service.status.as_str(), theme.accent_secondary),
                RuntimeServiceStatus::Failed => (service.status.as_str(), theme.status_error),
                RuntimeServiceStatus::Stopped => (service.status.as_str(), theme.text_secondary),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", service.name),
                    Style::default().fg(theme.text_primary),
                ),
                Span::styled(
                    format!("[{status_str}]"),
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            if let Some(log_path) = &service.log_path {
                lines.push(Line::from(Span::styled(
                    format!("    log: {log_path}"),
                    Style::default().fg(theme.text_secondary),
                )));
            }
        }
    }

    lines
}

fn tests_content(
    selected_task: Option<&Task>,
    selected_review_item: Option<&ReviewQueueItem>,
    thread_ctx: Option<&SelectedThreadContext>,
) -> String {
    let mut text = String::new();
    if let Some(task) = selected_task {
        let _ = writeln!(text, "task status: {}", task.status);
        let _ = writeln!(
            text,
            "session: {}",
            if task.session_id.is_some() {
                "attached"
            } else {
                "none"
            }
        );
        let _ = writeln!(
            text,
            "ci: {}",
            task.ci_status
                .map_or_else(|| "unknown".to_string(), |status| status.to_string())
        );
    }
    if let Some(thread_ctx) = thread_ctx {
        let _ = writeln!(text);
        let _ = writeln!(text, "{} runs captured", thread_ctx.run_count);
        let _ = writeln!(
            text,
            "{} review(s), {} inline review comment(s)",
            thread_ctx.review_count, thread_ctx.review_comment_count
        );
    } else if let Some(review_item) = selected_review_item {
        let _ = writeln!(text, "cached review state");
        let _ = writeln!(
            text,
            "{} top-level comment(s), {} review(s), {} inline review comment(s)",
            review_item.comment_count, review_item.review_count, review_item.review_comment_count
        );
        let _ = writeln!(
            text,
            "review decision: {}",
            review_item
                .pr_cache
                .review_decision
                .as_deref()
                .unwrap_or("open")
                .replace('_', " ")
        );
    }
    if text.is_empty() {
        "No test or review state for the current selection.".to_string()
    } else {
        text
    }
}

/// Build a compact horizontal pipeline strip for the conversation header.
///
/// Returns `None` when no workflow is active, so the caller can skip the line.
fn workflow_pipeline_line(
    thread_ctx: &SelectedThreadContext,
    theme: &Theme,
    max_width: u16,
) -> Option<Line<'static>> {
    if thread_ctx.workflow_stages.is_empty() {
        return None;
    }
    let workflow_name = thread_ctx
        .workflow_def
        .as_ref()
        .map_or("workflow", |def| def.name.as_str())
        .to_string();

    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        format!(" {workflow_name}  "),
        Style::default()
            .fg(theme.accent_secondary)
            .add_modifier(Modifier::BOLD),
    )];

    let mut stage_spans: Vec<(Vec<Span<'static>>, usize)> = Vec::new();
    for stage in &thread_ctx.workflow_stages {
        let (icon, color) = match stage.status {
            WorkflowStageStatus::Completed => ("\u{2713}", theme.status_done),
            WorkflowStageStatus::Running => ("\u{25C9}", theme.status_working),
            WorkflowStageStatus::WaitingApproval => ("\u{23F8}", theme.status_paused),
            WorkflowStageStatus::Pending => ("\u{25CB}", theme.status_pending),
            WorkflowStageStatus::Failed => ("\u{2717}", theme.status_error),
            WorkflowStageStatus::Skipped => ("\u{2212}", theme.text_secondary),
        };
        let name = stage.stage_name.clone();
        // Each stage is: "icon name" + separator " › "
        let width = icon.len() + 1 + name.len();
        stage_spans.push((
            vec![
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(name, Style::default().fg(color)),
            ],
            width,
        ));
    }

    // Compute total width to decide if truncation is needed
    let sep = " \u{203A} ";
    let sep_width = 3; // " › "
    let name_prefix_width = workflow_name.len() + 3; // name + two spaces + space
    let total_stages_width: usize = stage_spans.iter().map(|(_, w)| *w).sum::<usize>()
        + stage_spans.len().saturating_sub(1) * sep_width;
    let total_width = name_prefix_width + total_stages_width;

    if total_width <= max_width as usize {
        // Fits — render all stages
        for (i, (s, _)) in stage_spans.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(
                    sep.to_string(),
                    Style::default()
                        .fg(theme.border_unfocused)
                        .add_modifier(Modifier::DIM),
                ));
            }
            spans.extend(s);
        }
    } else {
        // Truncate from left: find the current stage and show "… › current › remaining"
        let current_idx = stage_spans
            .iter()
            .position(|(s, _)| {
                s.first().is_some_and(|span| {
                    let content = span.content.as_ref();
                    content.contains('\u{25C9}') || content.contains('\u{23F8}')
                })
            })
            .unwrap_or(0);
        let start = current_idx.saturating_sub(0); // start from current
        spans.push(Span::styled(
            "\u{2026} ".to_string(),
            Style::default()
                .fg(theme.border_unfocused)
                .add_modifier(Modifier::DIM),
        ));
        for (i, (s, _)) in stage_spans.into_iter().enumerate().skip(start) {
            if i > start {
                spans.push(Span::styled(
                    sep.to_string(),
                    Style::default()
                        .fg(theme.border_unfocused)
                        .add_modifier(Modifier::DIM),
                ));
            }
            spans.extend(s);
        }
    }

    Some(Line::from(spans))
}

/// Build the approval gate banner shown above compose when workflow is waiting.
///
/// When the previous stage produced output (last assistant message before the gate),
/// show it inline so the user can review before approving.
fn workflow_approval_banner(
    thread_ctx: &SelectedThreadContext,
    theme: &Theme,
    width: u16,
) -> Option<Vec<Line<'static>>> {
    let run = thread_ctx.workflow_run.as_ref()?;
    if run.status != WorkflowRunStatus::WaitingApproval {
        return None;
    }
    let stage_name = run
        .current_stage
        .as_deref()
        .unwrap_or("unknown")
        .to_string();

    let mut lines = Vec::new();

    // Check for stage output: output_summary from the completed stage before this gate,
    // or fall back to the last assistant message in the conversation.
    let stage_output: Option<String> = thread_ctx
        .workflow_stages
        .iter()
        .rev()
        .find(|s| s.status == WorkflowStageStatus::Completed && s.output_summary.is_some())
        .and_then(|s| s.output_summary.clone())
        .or_else(|| {
            thread_ctx
                .messages
                .iter()
                .rev()
                .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
                .map(|m| m.content.clone())
        });

    // Stage divider
    let dashes: String = "\u{2504}".repeat(width.saturating_sub(2) as usize);
    lines.push(Line::from(Span::styled(
        format!(" {dashes}"),
        Style::default().fg(theme.status_paused),
    )));

    // Show inline review of previous stage output if available
    if let Some(output) = stage_output {
        lines.push(Line::from(Span::styled(
            format!(" \u{23F8} Stage: {stage_name} waiting for review"),
            Style::default()
                .fg(theme.status_paused)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Claude's output for this stage:",
            Style::default().fg(theme.text_secondary),
        )));

        // Bordered output box (top border)
        let box_width = width.saturating_sub(4) as usize;
        let top_border = format!(
            " \u{250C}{}\u{2510}",
            "\u{2500}".repeat(box_width.saturating_sub(2))
        );
        lines.push(Line::from(Span::styled(
            top_border,
            Style::default().fg(theme.border_unfocused),
        )));

        // Output content (limited to 8 lines for space)
        let max_preview_lines = 8;
        let trimmed = output.trim();
        for line in trimmed.lines().take(max_preview_lines) {
            let truncated = if line.len() > box_width.saturating_sub(4) {
                format!("{}...", &line[..box_width.saturating_sub(7)])
            } else {
                line.to_string()
            };
            let padded = format!(
                " \u{2502} {:<width$}\u{2502}",
                truncated,
                width = box_width.saturating_sub(4)
            );
            lines.push(Line::from(Span::styled(
                padded,
                Style::default().fg(theme.text_primary),
            )));
        }
        let remaining = trimmed.lines().count().saturating_sub(max_preview_lines);
        if remaining > 0 {
            let more_line = format!(
                " \u{2502} ...{remaining} more line(s){}\u{2502}",
                " ".repeat(
                    box_width.saturating_sub(4 + format!("...{remaining} more line(s)").len())
                )
            );
            lines.push(Line::from(Span::styled(
                more_line,
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::DIM),
            )));
        }

        // Bottom border
        let bottom_border = format!(
            " \u{2514}{}\u{2518}",
            "\u{2500}".repeat(box_width.saturating_sub(2))
        );
        lines.push(Line::from(Span::styled(
            bottom_border,
            Style::default().fg(theme.border_unfocused),
        )));
        lines.push(Line::from(""));
    }

    // Action hints
    lines.push(Line::from(vec![
        Span::styled(
            format!(" \u{23F8} Approval required: {stage_name} "),
            Style::default()
                .fg(theme.status_paused)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("\u{2014} ", Style::default().fg(theme.text_secondary)),
        Span::styled(
            "g",
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            ": approve and continue  \u{2502}  ",
            Style::default().fg(theme.text_secondary),
        ),
        Span::styled(
            "i",
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            ": compose feedback",
            Style::default().fg(theme.text_secondary),
        ),
    ]));

    Some(lines)
}

fn styled_plan_lines<'a>(
    selected_review_item: Option<&ReviewQueueItem>,
    thread_ctx: Option<&SelectedThreadContext>,
    theme: &Theme,
) -> Vec<Line<'a>> {
    let Some(thread_ctx) = thread_ctx else {
        let fallback = if let Some(review_item) = selected_review_item {
            format!(
                "Launch an AI review thread for PR #{} to capture a structured review plan, findings, and follow-up actions.",
                review_item.github_item.number
            )
        } else {
            "No workflow or knowledge context for the current selection.".to_string()
        };
        return vec![Line::from(fallback)];
    };

    let mut lines: Vec<Line<'a>> = Vec::new();

    // Stage pipeline visualization
    if !thread_ctx.workflow_stages.is_empty() {
        lines.push(Line::from(Span::styled(
            " Workflow Stages",
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        for stage in &thread_ctx.workflow_stages {
            let (icon, color) = match stage.status {
                WorkflowStageStatus::Completed => (" \u{2713}", theme.status_done), // ✓
                WorkflowStageStatus::Running => (" \u{25C9}", theme.status_working), // ◉
                WorkflowStageStatus::WaitingApproval => (" \u{23F8}", theme.status_paused), // ⏸
                WorkflowStageStatus::Pending => (" \u{25CB}", theme.status_pending), // ○
                WorkflowStageStatus::Failed => (" \u{2717}", theme.status_error),   // ✗
                WorkflowStageStatus::Skipped => (" \u{2212}", theme.text_secondary), // −
            };

            let provider_suffix = stage
                .provider_kind
                .as_ref()
                .map(|pk| format!(" via {pk}"))
                .unwrap_or_default();
            let status_label = stage.status.as_str();

            lines.push(Line::from(vec![
                Span::styled(icon, Style::default().fg(color)),
                Span::styled(
                    format!(" {} ", stage.stage_name),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("[{status_label}]"), Style::default().fg(color)),
                Span::styled(provider_suffix, Style::default().fg(theme.text_secondary)),
            ]));

            // Show prompt hint for running/pending stages
            if matches!(
                stage.status,
                WorkflowStageStatus::Running | WorkflowStageStatus::Pending
            ) && let Some(prompt) = stage.prompt.as_deref()
                && !prompt.is_empty()
            {
                let display = if prompt.len() > 60 {
                    format!("   /{}", &prompt[..57].trim_end())
                } else {
                    format!("   /{prompt}")
                };
                lines.push(Line::from(Span::styled(
                    display,
                    Style::default().fg(theme.text_secondary),
                )));
            }

            // Approval hint for waiting stages
            if stage.status == WorkflowStageStatus::WaitingApproval {
                lines.push(Line::from(Span::styled(
                    "   Press g to approve",
                    Style::default()
                        .fg(theme.status_paused)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        }

        lines.push(Line::from(""));
    } else if let Some(summary) = thread_ctx.workflow_summary() {
        lines.push(Line::from(Span::styled(
            " Workflow",
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(format!(" {summary}")));
        lines.push(Line::from(""));
    } else {
        lines.push(Line::from(Span::styled(
            " No workflow attached.",
            Style::default().fg(theme.text_secondary),
        )));
        lines.push(Line::from(""));
    }

    // Knowledge summary
    lines.push(Line::from(Span::styled(
        " Knowledge",
        Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(" {}", thread_ctx.knowledge_summary())));

    // Latest prompt
    if let Some(run) = &thread_ctx.latest_run
        && let Some(prompt) = run.prompt.as_deref()
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Latest Prompt",
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        )));
        for line in prompt.lines() {
            lines.push(Line::from(format!(" {line}")));
        }
    }

    lines
}

fn attachments_content(thread_ctx: Option<&SelectedThreadContext>) -> String {
    let Some(thread_ctx) = thread_ctx else {
        return "No attachments for the current selection.".to_string();
    };
    if thread_ctx.attachments.is_empty() {
        return "No attachments yet.".to_string();
    }
    let mut text = String::new();
    for attachment in &thread_ctx.attachments {
        let _ = writeln!(text, "{} ({})", attachment.file_name, attachment.mime_type);
        let _ = writeln!(text, "  {}", attachment.local_path);
    }
    text
}

fn draw_status_line(frame: &mut Frame, app: &App, area: Rect) {
    if app.input_mode == InputMode::LaunchThread
        && let Some(draft) = app.launch_thread_draft.as_ref()
    {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(
                    " launch: {} via {} ",
                    draft.source_label(),
                    draft.provider_kind
                ),
                Style::default()
                    .fg(app.theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            )),
            area,
        );
        return;
    }

    if app.input_mode == InputMode::ConfirmDelete {
        let prompt = if matches!(
            app.confirm_delete_kind,
            super::super::app::DeleteTarget::Session
        ) {
            format!(
                " Close session '{}' permanently? Worktree and changes will be deleted. ",
                app.confirm_target
            )
        } else {
            format!(" Delete '{}'? ", app.confirm_target)
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prompt, Style::default().fg(app.theme.status_error)),
                Span::styled(
                    "(y: confirm, Esc: cancel)",
                    Style::default().fg(app.theme.text_secondary),
                ),
            ])),
            area,
        );
        return;
    }

    if app.input_mode == InputMode::TaskFilter {
        let cursor = app.task_filter_cursor.min(app.task_filter.len());
        let (before, after) = app.task_filter.split_at(cursor);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" /", Style::default().fg(app.theme.accent_secondary)),
                Span::raw(before.to_string()),
                Span::styled("\u{2588}", Style::default().fg(app.theme.accent_secondary)),
                Span::raw(after.to_string()),
                Span::styled(
                    "  Enter:apply  Esc:clear",
                    Style::default().fg(app.theme.text_secondary),
                ),
            ])),
            area,
        );
        return;
    }

    if app.leader_pending {
        frame.render_widget(
            Paragraph::new(
                " leader: t tasks  b board  h threads  r reviews  a agents  s settings ",
            ),
            area,
        );
        return;
    }

    if let Some(line) = toast_line(app) {
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    let status = match app.workbench_view {
        WorkbenchView::MyTasks => format!(
            " {} assigned GitHub item(s), {} thread(s), {} active session(s) ",
            app.visible_my_task_count(),
            app.threads.len(),
            app.sessions
                .iter()
                .filter(|session| session.closed_at.is_none())
                .count()
        ),
        WorkbenchView::Threads => {
            let selected = app
                .selected_thread()
                .map_or_else(|| "none".to_string(), |thread| truncate(&thread.title, 36));
            if app.input_mode == InputMode::ThreadCompose {
                format!(" composing in: {selected} ")
            } else {
                format!(" {} thread(s), selected: {} ", app.threads.len(), selected)
            }
        }
        WorkbenchView::Reviews => format!(
            " reviews: {} authored, {} need review, queue: {} ",
            app.review_authored_items.len(),
            app.review_requested_items.len(),
            app.review_queue_tab.label()
        ),
        WorkbenchView::Agents => " provider and runtime overview ".to_string(),
        WorkbenchView::Settings => format!(
            " settings: {} ",
            SettingsSection::ALL
                .get(app.settings_section_index)
                .copied()
                .unwrap_or(SettingsSection::AiProviders)
                .label()
        ),
        WorkbenchView::SprintBoard => format!(
            " board: {} column(s), {} item(s), board: {}, sprint: {}, column: {} ",
            app.board_columns.len(),
            app.board_issues.iter().map(Vec::len).sum::<usize>(),
            app.board_project_title.as_deref().unwrap_or("none"),
            app.board_sprint_filter.as_deref().unwrap_or("all"),
            app.board_columns
                .get(app.board_column_index)
                .map_or("n/a", String::as_str)
        ),
    };
    let mut spans: Vec<Span<'_>> = Vec::new();

    if let Some(activity) = app.busy_indicator_label() {
        spans.push(Span::styled(
            format!(" {} {} ", spinner_char(), activity),
            Style::default()
                .fg(app.theme.spinner)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            "\u{2502}",
            Style::default().fg(app.theme.border_unfocused),
        ));
    }

    spans.push(Span::styled(
        status,
        Style::default()
            .fg(app.theme.text_secondary)
            .add_modifier(Modifier::BOLD),
    ));

    // Right-aligned model + effort info
    let model_short = app
        .config
        .claude
        .model
        .strip_prefix("claude-")
        .unwrap_or(&app.config.claude.model);
    let right_info = format!(" {} \u{00B7} {} ", model_short, app.config.claude.effort);
    let used_width: u16 = spans.iter().map(|s| s.width() as u16).sum();
    let right_width = right_info.len() as u16;
    let pad = area
        .width
        .saturating_sub(used_width)
        .saturating_sub(right_width);
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad as usize)));
        spans.push(Span::styled(
            right_info,
            Style::default().fg(app.theme.accent_secondary),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_hint_line(frame: &mut Frame, app: &App, area: Rect) {
    let show_inspector = workbench_uses_persistent_inspector(app.workbench_view);
    let hints = if app.input_mode == InputMode::LaunchThread {
        " Tab:next field  Shift+Tab:prev  h/l:provider  type:add context  Ctrl+V:paste image  Enter:launch  Esc:cancel "
    } else if app.input_mode == InputMode::ThreadProviderPicker {
        " j/k or h/l:choose provider  Enter:switch  Esc:cancel "
    } else if app.input_mode == InputMode::ProjectPicker {
        " j/k:choose repository  Enter:switch repo  Esc:cancel "
    } else if app.input_mode == InputMode::ThreadCompose {
        " 1:sidebar  2:main  3:inspect  [ / ]:resize  p:repo  type:message  Ctrl+V:paste image  Enter:send  Esc:keep draft  o:terminal  Tab:cycle focus "
    } else if app.workbench_view == WorkbenchView::SprintBoard
        && matches!(
            app.input_mode,
            InputMode::BoardView | InputMode::BoardFilter | InputMode::MilestoneFilter
        )
    {
        match app.focus {
            Focus::Projects => {
                " 1:sidebar  2:main  Tab:cycle focus  [ / ]:resize sidebar  p:repo  j/k:sidebar  Enter:open  "
            }
            Focus::Tasks => {
                " 1:sidebar  2:main  Tab:cycle focus  [ / ]:resize sidebar  p:repo  j/k:item  h/l:column  Enter/v:details  L:launch thread  o:open  /:filter  t:scope  g:board  m:sprint  b:back "
            }
            Focus::Inspector => {
                " 1:sidebar  2:main  Tab:cycle focus  [ / ]:resize sidebar  p:repo "
            }
        }
    } else if app.workbench_view == WorkbenchView::Threads && app.focus == Focus::Tasks {
        " SPC:view  1:sidebar  2:main  3:inspect  [ / ]:resize  p:repo  j/k:thread  type:message  m:provider  l/Enter:continue thread  o:terminal  n:new ad hoc thread  Tab:cycle focus "
    } else if app.workbench_view == WorkbenchView::Reviews && app.focus == Focus::Tasks {
        " SPC:view  1:sidebar  2:main  3:inspect  [ / ]:resize  p:repo  j/k:pr  t:queue  v:drawer  l:launch/focus thread  Enter:open/focus thread  o:open PR  Tab:cycle focus "
    } else if app.workbench_view == WorkbenchView::Reviews && app.focus == Focus::Inspector {
        if app.inspector_expanded {
            " 1-6:tab  j/k:scroll  f:collapse  v:drawer  Esc:collapse  Tab:cycle focus "
        } else {
            " 1-6:tab  j/k:scroll  f:expand  v:drawer  Tab:cycle focus "
        }
    } else if matches!(app.workbench_view, WorkbenchView::MyTasks) && app.focus == Focus::Tasks {
        " SPC:view  1:sidebar  2:main  [ / ]:resize sidebar  p:repo  j/k:navigate  Enter/v:details  l:launch/focus thread  N:new session  R:sync GitHub  Tab:cycle focus "
    } else if app.workbench_view == WorkbenchView::Settings && app.focus == Focus::Tasks {
        match SettingsSection::ALL
            .get(app.settings_section_index)
            .copied()
            .unwrap_or(SettingsSection::AiProviders)
        {
            SettingsSection::AiProviders => {
                " SPC:view  1:sidebar  2:main  [ / ]:resize sidebar  P:repo  j/k:section  Enter/m:model  e:effort  r:remote  u:auto-update  Tab:cycle focus "
            }
            SettingsSection::Permissions => {
                " SPC:view  1:sidebar  2:main  [ / ]:resize sidebar  P:repo  j/k:section  Enter/c:configure  Tab:cycle focus "
            }
            SettingsSection::GitHub => {
                if app.github_settings_show_advanced {
                    " SPC:view  1:sidebar  2:main  [ / ]:resize sidebar  P:repo  j/k:section  Enter/a:connect  f:refresh  o:install  y:sync  x:disconnect  g:default board  .:hide advanced  c/s/u/r/i:custom  Tab:cycle focus "
                } else {
                    " SPC:view  1:sidebar  2:main  [ / ]:resize sidebar  P:repo  j/k:section  Enter/a:connect  f:refresh  o:install  y:sync  x:disconnect  g:default board  .:advanced  Tab:cycle focus "
                }
            }
            SettingsSection::Layout => {
                " SPC:view  1:sidebar  2:main  [ / ]:resize sidebar  P:repo  j/k:section  Enter/p:preset  l:leader  Tab:cycle focus "
            }
            SettingsSection::Notifications => {
                " SPC:view  1:sidebar  2:main  [ / ]:resize sidebar  P:repo  j/k:section  Enter/e:voice  s:system  c:command  t:template  a/p/r/v/f:rules  Tab:cycle focus "
            }
            SettingsSection::ReviewLoop => {
                " SPC:view  1:sidebar  2:main  [ / ]:resize sidebar  P:repo  j/k:section  Enter/i:interval  p:prompt  Tab:cycle focus "
            }
            SettingsSection::Workflows => {
                " SPC:view  1:sidebar  2:main  [ / ]:resize sidebar  P:repo  j/k:section  J/K:workflow  n:new  e:edit  d:delete  Tab:cycle focus "
            }
            SettingsSection::Sandbox => {
                " SPC:view  1:sidebar  2:main  [ / ]:resize sidebar  P:repo  j/k:section  Enter/p:profile  s:path  a:attachments  Tab:cycle focus "
            }
            SettingsSection::General => {
                " SPC:view  1:sidebar  2:main  [ / ]:resize sidebar  P:repo  j/k:section  Enter/a:auto-push  r:rtk  Tab:cycle focus "
            }
        }
    } else if !show_inspector {
        match app.focus {
            Focus::Projects => {
                " SPC:view  1:sidebar  2:main  Tab:cycle focus  [ / ]:resize sidebar  p:repo  j/k:navigate  Enter:open "
            }
            Focus::Tasks => {
                " SPC:view  1:sidebar  2:main  Tab:cycle focus  [ / ]:resize sidebar  p:repo  j/k:navigate  Enter:activate "
            }
            Focus::Inspector => " SPC:view  1:sidebar  2:main  Tab:cycle focus ",
        }
    } else {
        " SPC:view  1:sidebar  2:main  3:inspect  [ / ]:resize  p:repo  j/k:navigate  Tab:cycle focus  Enter:activate  ?:help "
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            hints,
            Style::default().fg(app.theme.text_secondary),
        )),
        area,
    );
}

fn load_diff_preview(worktree_path: &str) -> String {
    // First try uncommitted changes (staged + unstaged)
    let output = Command::new("git")
        .args([
            "-C",
            worktree_path,
            "diff",
            "HEAD",
            "--find-renames",
            "--unified=3",
            "--no-ext-diff",
        ])
        .output();

    let uncommitted = match &output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => String::new(),
    };

    if !uncommitted.is_empty() {
        return trim_multiline(&uncommitted, 180);
    }

    // No uncommitted changes — show branch diff (all commits on this branch)
    let merge_base = Command::new("git")
        .args(["-C", worktree_path, "merge-base", "HEAD", "HEAD~20"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    // Try diff against the branch point (first parent that's on a different branch)
    let branch_diff = Command::new("git")
        .args([
            "-C",
            worktree_path,
            "log",
            "--oneline",
            "--no-merges",
            "-20",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    if branch_diff.is_empty() && merge_base.is_none() {
        return "No local diff. Worktree is clean.".to_string();
    }

    // Show the diff of all commits on this branch vs first commit's parent
    let commit_count = branch_diff.lines().count();
    let diff_output = Command::new("git")
        .args([
            "-C",
            worktree_path,
            "diff",
            &format!("HEAD~{commit_count}..HEAD"),
            "--find-renames",
            "--unified=3",
            "--no-ext-diff",
            "--stat",
        ])
        .output();

    match diff_output {
        Ok(output) if output.status.success() => {
            let diff = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if diff.is_empty() {
                "No local diff. Worktree is clean.".to_string()
            } else {
                let mut result = format!("Branch commits ({commit_count}):\n{branch_diff}\n\n");
                result.push_str(&diff);
                trim_multiline(&result, 180)
            }
        }
        _ => {
            if branch_diff.is_empty() {
                "No local diff. Worktree is clean.".to_string()
            } else {
                format!("Branch commits ({commit_count}):\n{branch_diff}")
            }
        }
    }
}

/// Full diff for the review drawer / expanded inspector (2000 line limit).
pub(super) fn load_remote_pr_diff_preview_full(
    store: &crate::store::Store,
    review_item: &ReviewQueueItem,
) -> String {
    load_remote_pr_diff_with_limit(store, review_item, 2000)
}

fn load_remote_pr_diff_with_limit(
    store: &crate::store::Store,
    review_item: &ReviewQueueItem,
    max_lines: usize,
) -> String {
    let repo = match store.get_github_repo(&review_item.github_item.repo_id) {
        Ok(repo) => repo,
        Err(error) => return format!("failed to resolve cached GitHub repo: {error}"),
    };

    let output = Command::new("gh")
        .args([
            "pr",
            "diff",
            &review_item.github_item.number.to_string(),
            "--repo",
            &repo.full_name,
        ])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let diff = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if diff.is_empty() {
                "GitHub returned no diff for this PR.".to_string()
            } else {
                trim_multiline(&diff, max_lines)
            }
        }
        Ok(output) => format!(
            "gh pr diff failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => format!("failed to run gh pr diff: {error}"),
    }
}

fn trim_multiline(text: &str, max_lines: usize) -> String {
    let mut lines = text.lines();
    let mut trimmed = lines
        .by_ref()
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n");
    if lines.next().is_some() {
        trimmed.push_str("\n\n... diff truncated ...");
    }
    trimmed
}

fn truncate(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text.chars().count() <= max_width {
        return text.to_string();
    }
    let take = max_width.saturating_sub(1);
    let mut out = text.chars().take(take).collect::<String>();
    out.push('~');
    out
}

/// Build renderable lines from JSONL conversation entries.
///
/// Each entry type gets a distinct visual treatment using the theme palette:
/// - User messages: accent left bar, bold first line, dim timestamp
/// - Assistant text: body text in normal color, dim separator with timestamp
/// - Tool use: individual lines with status icon (completed/in-progress)
/// - Tool result: bordered output preview (first 8 lines, rest collapsed)
/// - Thinking: hidden (too noisy)
/// - Turn end: flushes any remaining pending state
pub(super) fn build_jsonl_conversation_lines(
    entries: &[crate::conversation::ConversationEntry],
    theme: &Theme,
    _width: u16,
) -> Vec<Line<'static>> {
    use crate::conversation::ConversationEntry;

    /// Maximum visible output lines for a tool result box before collapsing.
    const TOOL_RESULT_MAX_LINES: usize = 8;

    // Pre-compute which tool_use_ids have a matching ToolResult so we can show
    // completed vs in-progress status icons.
    let completed_tool_ids: HashSet<&str> = entries
        .iter()
        .filter_map(|e| {
            if let ConversationEntry::ToolResult { tool_use_id, .. } = e {
                Some(tool_use_id.as_str())
            } else {
                None
            }
        })
        .collect();

    // Find the last ToolUse that has no matching result — this is "in-progress".
    let last_pending_tool_id: Option<&str> = entries.iter().rev().find_map(|e| {
        if let ConversationEntry::ToolUse { tool_use_id, .. } = e
            && !completed_tool_ids.contains(tool_use_id.as_str())
        {
            Some(tool_use_id.as_str())
        } else {
            None
        }
    });

    let mut lines: Vec<Line<'static>> = Vec::new();

    for entry in entries {
        match entry {
            ConversationEntry::UserMessage { timestamp, text } => {
                // Separator before user message
                if !lines.is_empty() {
                    lines.push(Line::from(""));
                }
                // First line with accent left bar + bold text
                lines.push(Line::from(vec![
                    Span::styled(
                        "\u{258e} ".to_string(),
                        Style::default().fg(theme.accent_primary),
                    ),
                    Span::styled(
                        text.lines().next().unwrap_or("").to_string(),
                        Style::default()
                            .fg(theme.text_primary)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                // Additional lines of user message with left bar
                for line in text.lines().skip(1) {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "\u{258e} ".to_string(),
                            Style::default().fg(theme.accent_primary),
                        ),
                        Span::styled(line.to_string(), Style::default().fg(theme.text_primary)),
                    ]));
                }
                // Timestamp on separate dim line
                lines.push(Line::from(Span::styled(
                    format!("  {}", compact_timestamp(timestamp)),
                    Style::default()
                        .fg(theme.text_secondary)
                        .add_modifier(Modifier::DIM),
                )));
            }

            ConversationEntry::AssistantText { timestamp, text } => {
                lines.push(Line::from(""));
                // Agent response text — no label, just the content
                for line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {line}"),
                        Style::default().fg(theme.text_primary),
                    )));
                }
                // Separator: "◇ Claude via Anthropic · timestamp"
                lines.push(Line::from(Span::styled(
                    format!(
                        "  \u{25c7} Claude via Anthropic \u{00b7} {}",
                        compact_timestamp(timestamp)
                    ),
                    Style::default()
                        .fg(theme.text_secondary)
                        .add_modifier(Modifier::DIM),
                )));
            }

            ConversationEntry::ToolUse {
                tool_name,
                description,
                tool_use_id,
                ..
            } => {
                let is_completed = completed_tool_ids.contains(tool_use_id.as_str());
                let is_in_progress =
                    last_pending_tool_id.is_some_and(|id| id == tool_use_id.as_str());

                let (icon_str, icon_color) = if is_completed {
                    ("\u{2713}".to_string(), theme.status_done) // check mark green
                } else if is_in_progress {
                    (spinner_char().to_string(), theme.accent_secondary) // spinner yellow
                } else {
                    ("\u{25cb}".to_string(), theme.text_secondary) // ○ dim
                };

                // Show each tool call on its own line with status icon
                lines.push(Line::from(vec![
                    Span::styled(format!("  {icon_str} "), Style::default().fg(icon_color)),
                    Span::styled(
                        tool_name.clone(),
                        Style::default()
                            .fg(theme.text_primary)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {description}"),
                        Style::default().fg(theme.text_secondary),
                    ),
                ]));
            }

            ConversationEntry::ToolResult {
                is_error,
                output_preview,
                ..
            } => {
                if output_preview.is_empty() {
                    continue;
                }

                let border_color = if *is_error {
                    theme.status_error
                } else {
                    theme.border_unfocused
                };
                let text_color = if *is_error {
                    theme.status_error
                } else {
                    theme.text_secondary
                };

                let output_lines: Vec<&str> = output_preview.lines().collect();
                let visible = output_lines.len().min(TOOL_RESULT_MAX_LINES);
                for line in &output_lines[..visible] {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "    \u{2502} ".to_string(),
                            Style::default().fg(border_color),
                        ),
                        Span::styled(
                            (*line).to_string(),
                            Style::default().fg(text_color).add_modifier(Modifier::DIM),
                        ),
                    ]));
                }
                let remaining = output_lines.len().saturating_sub(TOOL_RESULT_MAX_LINES);
                if remaining > 0 {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "    \u{2502} ".to_string(),
                            Style::default().fg(border_color),
                        ),
                        Span::styled(
                            format!("({remaining} more lines)"),
                            Style::default()
                                .fg(theme.text_secondary)
                                .add_modifier(Modifier::DIM),
                        ),
                    ]));
                }
            }

            // Thinking and TurnEnd produce no visual output.
            ConversationEntry::Thinking { .. } | ConversationEntry::TurnEnd { .. } => {}
        }
    }

    lines
}
