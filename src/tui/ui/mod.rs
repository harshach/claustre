mod board;
mod forms;
mod overlays;
mod session;
mod tab_bar;
mod usage;
mod workbench;

pub use tab_bar::compute_tab_layout;
pub(crate) use workbench::{
    compute_workbench_layout, normalize_workbench_widths, workbench_uses_persistent_inspector,
};

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::{SystemTime, UNIX_EPOCH};

use super::app::{App, InputMode};

use forms::{draw_new_project_panel, draw_task_form_panel};
use overlays::{
    draw_board_issue_drawer, draw_command_palette, draw_configure_wizard,
    draw_github_installation_overlay, draw_github_issue_form, draw_help_overlay,
    draw_launch_thread_overlay, draw_my_task_drawer, draw_project_picker_overlay,
    draw_review_drawer, draw_settings_edit_overlay, draw_settings_picker_overlay,
    draw_skill_add_overlay, draw_skill_panel, draw_skill_search_overlay, draw_subtask_panel,
    draw_task_details_panel, draw_thread_provider_picker_overlay,
};
use session::draw_session_tab;
use tab_bar::draw_tab_bar;
use workbench::{draw_active, draw_active_in_area};

/// If a toast is active, return a styled `Line` for it; otherwise `None`.
pub(crate) fn toast_line(app: &App) -> Option<Line<'static>> {
    let msg = app.toast_message.as_ref()?;
    let style = app.theme.toast_style(app.toast_style);
    let prefix = match app.toast_style {
        crate::tui::app::ToastStyle::Success => " OKAY! ",
        crate::tui::app::ToastStyle::Error => " ERR!  ",
        crate::tui::app::ToastStyle::Info => " INFO  ",
    };
    // Pad to fill the full terminal width
    let text = format!("{prefix} {msg}");
    let pad = app
        .last_terminal_area
        .width
        .saturating_sub(text.len() as u16);
    let padded = format!("{text}{}", " ".repeat(pad as usize));
    Some(Line::from(Span::styled(padded, style)))
}

pub(crate) fn spinner_char() -> char {
    const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let frame_index = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            ((duration.as_millis() / 100) as usize) % FRAMES.len()
        });
    FRAMES[frame_index]
}

fn draw_loading_screen(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let theme = &app.theme;
    let bg = theme.main_surface();

    // Fill background
    frame.render_widget(Paragraph::new("").style(bg), area);

    // Center vertically
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(area);

    let spinner = spinner_char();
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "claustre",
                Style::default()
                    .fg(theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    format!("{spinner} "),
                    Style::default().fg(theme.accent_secondary),
                ),
                Span::styled("Loading...", Style::default().fg(theme.text_secondary)),
            ]),
        ])
        .alignment(Alignment::Center)
        .style(bg),
        vertical[1],
    );
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    // Loading screen on startup
    if app.loading {
        draw_loading_screen(frame, app);
        return;
    }

    // If on a session tab, render the terminal view
    if app.active_tab > 0 {
        draw_session_tab(frame, app);
        return;
    }

    // Tab bar (only show if there are session tabs)
    if app.tabs.len() > 1 {
        let size = frame.area();
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(size);
        draw_tab_bar(frame, app, outer[0]);
        // Render the dashboard in the remaining area
        let sub_frame_area = outer[1];
        draw_active_in_area(frame, app, sub_frame_area);
    } else {
        draw_active(frame, app);
    }

    // Floating panel overlays
    match app.input_mode {
        InputMode::CommandPalette => draw_command_palette(frame, app),
        InputMode::LaunchThread => draw_launch_thread_overlay(frame, app),
        InputMode::SettingsEdit => draw_settings_edit_overlay(frame, app),
        InputMode::SettingsPicker => draw_settings_picker_overlay(frame, app),
        InputMode::GitHubInstallationPicker => draw_github_installation_overlay(frame, app),
        InputMode::ProjectPicker => draw_project_picker_overlay(frame, app),
        InputMode::ThreadProviderPicker => draw_thread_provider_picker_overlay(frame, app),
        InputMode::NewTask => draw_task_form_panel(frame, app, " New Task "),
        InputMode::EditTask => draw_task_form_panel(frame, app, " Edit Task "),
        InputMode::NewProject => draw_new_project_panel(frame, app),
        InputMode::HelpOverlay => draw_help_overlay(frame, app),
        InputMode::TaskDetails => draw_task_details_panel(frame, app),
        InputMode::SubtaskPanel => draw_subtask_panel(frame, app),
        InputMode::SkillPanel => draw_skill_panel(frame, app),
        InputMode::SkillSearch | InputMode::SkillAdd => {
            draw_skill_panel(frame, app);
            if app.input_mode == InputMode::SkillSearch {
                draw_skill_search_overlay(frame, app);
            } else {
                draw_skill_add_overlay(frame, app);
            }
        }
        InputMode::ConfigureWizard => draw_configure_wizard(frame, app),
        InputMode::CreateGitHubIssue | InputMode::EditGitHubIssue => {
            draw_github_issue_form(frame, app);
        }
        InputMode::BoardIssueDrawer | InputMode::FieldPicker => {
            draw_board_issue_drawer(frame, app);
        }
        InputMode::CommentCompose => {
            // Render the drawer that the compose bar belongs to
            if app.comment_compose_return_mode == InputMode::ReviewDrawer {
                draw_review_drawer(frame, app);
            } else if app.comment_compose_return_mode == InputMode::MyTaskDrawer {
                draw_my_task_drawer(frame, app);
            } else {
                draw_board_issue_drawer(frame, app);
            }
        }
        InputMode::MyTaskDrawer => {
            draw_my_task_drawer(frame, app);
        }
        InputMode::ReviewDrawer => {
            draw_review_drawer(frame, app);
        }
        _ => {}
    }
}
