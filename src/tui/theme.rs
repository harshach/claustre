use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

use crate::store::{CiStatus, ClaudeStatus, TaskStatus, ThreadStatus};

/// Semantic colour theme for the entire TUI.
///
/// Every colour used by the renderer is stored here so the user can
/// override any of them via `[theme]` in `config.toml`.
#[derive(Debug, Clone)]
pub struct Theme {
    // ── Borders ───────────────────────────────────────────────
    pub border_focused: Color,
    pub border_unfocused: Color,

    // ── Text ──────────────────────────────────────────────────
    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_accent: Color,

    // ── Task status ───────────────────────────────────────────
    pub status_draft: Color,
    pub status_pending: Color,
    pub status_working: Color,
    pub status_interrupted: Color,
    pub status_in_review: Color,
    pub status_conflict: Color,
    pub status_ci_failed: Color,
    pub status_ci_running: Color,
    pub status_ci_passed: Color,
    pub status_done: Color,
    pub status_error: Color,
    pub status_paused: Color,
    /// Style for the waiting override (Claude asked a question via `AskUserQuestion`).
    pub status_waiting: Color,

    // ── Accents ───────────────────────────────────────────────
    pub accent_primary: Color,
    pub accent_secondary: Color,
    pub accent_tertiary: Color,

    // ── Toast ─────────────────────────────────────────────────
    pub toast_info: Color,
    pub toast_success: Color,
    pub toast_error: Color,

    // ── Usage bars ────────────────────────────────────────────
    pub usage_low: Color,
    pub usage_medium: Color,
    pub usage_high: Color,

    // ── Forms ─────────────────────────────────────────────────
    pub form_border_task: Color,
    pub form_border_project: Color,
    pub form_highlight: Color,
    pub form_dim: Color,

    // ── Tabs ──────────────────────────────────────────────────
    pub tab_active: Color,
    pub tab_inactive: Color,

    // ── Misc ──────────────────────────────────────────────────
    pub selection_indicator: Color,
    pub pr_link: Color,
    pub spinner: Color,
    pub rate_limit_warning: Color,
}

impl Default for Theme {
    fn default() -> Self {
        // Modern palette inspired by Charmbracelet Crush.
        // Cool blues and purples dominate; warm tones are soft peach/coral
        // instead of saturated yellow.
        Self {
            border_focused: Color::Rgb(138, 108, 255), // soft violet
            border_unfocused: Color::Rgb(55, 60, 82),  // slate

            text_primary: Color::Rgb(230, 225, 245), // cool white-lavender
            text_secondary: Color::Rgb(140, 148, 178), // muted slate
            text_accent: Color::Rgb(120, 200, 255),  // sky blue

            status_draft: Color::Rgb(120, 200, 255), // sky blue
            status_pending: Color::Rgb(110, 116, 148), // dim slate
            status_working: Color::Rgb(80, 220, 200), // teal-mint
            status_interrupted: Color::Rgb(240, 130, 210), // soft pink
            status_in_review: Color::Rgb(255, 180, 128), // warm peach
            status_conflict: Color::Rgb(255, 150, 90), // coral-orange
            status_ci_failed: Color::Rgb(255, 110, 120), // soft red
            status_ci_running: Color::Rgb(180, 160, 255), // lavender
            status_ci_passed: Color::Rgb(80, 220, 200), // teal-mint
            status_done: Color::Rgb(130, 170, 255),  // periwinkle
            status_error: Color::Rgb(255, 100, 100), // red
            status_paused: Color::Rgb(255, 180, 128), // warm peach
            status_waiting: Color::Rgb(120, 200, 255), // sky blue

            accent_primary: Color::Rgb(120, 200, 255), // sky blue
            accent_secondary: Color::Rgb(180, 160, 255), // lavender
            accent_tertiary: Color::Rgb(220, 120, 255), // orchid

            toast_info: Color::Rgb(120, 200, 255),   // sky blue
            toast_success: Color::Rgb(80, 220, 200), // teal-mint
            toast_error: Color::Rgb(255, 100, 100),  // red

            usage_low: Color::Rgb(80, 220, 200),     // teal-mint
            usage_medium: Color::Rgb(255, 180, 128), // peach
            usage_high: Color::Rgb(255, 100, 100),   // red

            form_border_task: Color::Rgb(180, 160, 255), // lavender
            form_border_project: Color::Rgb(220, 120, 255), // orchid
            form_highlight: Color::Rgb(120, 200, 255),   // sky blue
            form_dim: Color::Rgb(80, 85, 110),           // dark slate

            tab_active: Color::Rgb(138, 108, 255), // soft violet
            tab_inactive: Color::Rgb(105, 112, 140), // muted slate

            selection_indicator: Color::Rgb(120, 200, 255), // sky blue
            pr_link: Color::Rgb(220, 120, 255),             // orchid
            spinner: Color::Rgb(180, 160, 255),             // lavender
            rate_limit_warning: Color::Rgb(255, 100, 100),  // red
        }
    }
}

impl Theme {
    /// Background style for the navigation/sidebar rail.
    pub fn sidebar_surface(&self) -> Style {
        Style::default().bg(Color::Rgb(22, 24, 38))
    }

    /// Background style for primary work surfaces.
    pub fn main_surface(&self) -> Style {
        Style::default().bg(Color::Rgb(18, 20, 32))
    }

    /// Background style for the detail inspector rail.
    pub fn inspector_surface(&self) -> Style {
        Style::default().bg(Color::Rgb(16, 18, 28))
    }

    /// Background style for elevated cards and overlays.
    pub fn overlay_surface(&self) -> Style {
        Style::default().bg(Color::Rgb(26, 24, 42))
    }

    /// Background style for nested cards inside panels.
    pub fn card_surface(&self) -> Style {
        Style::default().bg(Color::Rgb(28, 32, 46))
    }

    /// Filled style for selected rows and focus chips.
    pub fn selected_fill(&self) -> Style {
        Style::default()
            .fg(self.text_primary)
            .bg(Color::Rgb(50, 55, 80))
            .add_modifier(Modifier::BOLD)
    }

    /// Filled chip style used for tabs and compact badges.
    pub fn chip_style(&self, color: Color) -> Style {
        Style::default()
            .fg(self.text_primary)
            .bg(color)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for a focused panel border.
    pub fn focused_border(&self) -> Style {
        Style::default()
            .fg(self.border_focused)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for an unfocused panel border.
    pub fn unfocused_border(&self) -> Style {
        Style::default().fg(self.border_unfocused)
    }

    /// Map a `TaskStatus` to its display style (foreground colour).
    pub fn task_status_style(&self, status: TaskStatus) -> Style {
        let color = match status {
            TaskStatus::Draft => self.status_draft,
            TaskStatus::Pending => self.status_pending,
            TaskStatus::Working => self.status_working,
            TaskStatus::Interrupted => self.status_interrupted,
            TaskStatus::InReview => self.status_in_review,
            TaskStatus::Conflict => self.status_conflict,
            TaskStatus::CiFailed => self.status_ci_failed,
            TaskStatus::Done => self.status_done,
            TaskStatus::Error => self.status_error,
        };
        Style::default().fg(color)
    }

    /// Map a `CiStatus` to its display style (foreground colour).
    pub fn ci_status_style(&self, status: CiStatus) -> Style {
        let color = match status {
            CiStatus::Running => self.status_ci_running,
            CiStatus::Passed => self.status_ci_passed,
            CiStatus::Failed => self.status_ci_failed,
        };
        Style::default().fg(color)
    }

    /// Map a `ClaudeStatus` to its display style (foreground colour).
    pub fn claude_status_style(&self, status: ClaudeStatus) -> Style {
        let color = match status {
            ClaudeStatus::Working => self.status_working,
            ClaudeStatus::Interrupted => self.status_interrupted,
            ClaudeStatus::Error => self.status_error,
            ClaudeStatus::Done => self.status_done,
            ClaudeStatus::Idle => self.status_pending,
        };
        Style::default().fg(color)
    }

    /// Style for the paused override (detected from PTY screen).
    pub fn paused_style(&self) -> Style {
        Style::default().fg(self.status_paused)
    }

    /// Style for the waiting override (Claude asked a question, detected from PTY screen).
    pub fn waiting_style(&self) -> Style {
        Style::default().fg(self.status_waiting)
    }

    /// Map a `ThreadStatus` to its display style (foreground colour).
    pub fn thread_status_style(&self, status: ThreadStatus) -> Style {
        let color = match status {
            ThreadStatus::Draft => self.status_draft,
            ThreadStatus::RuntimePreparing => self.status_pending,
            ThreadStatus::Ready | ThreadStatus::Done => self.status_done,
            ThreadStatus::Running => self.status_working,
            ThreadStatus::WaitingUser => self.status_waiting,
            ThreadStatus::WaitingReview => self.status_in_review,
            ThreadStatus::Blocked => self.status_conflict,
            ThreadStatus::Error => self.status_error,
        };
        Style::default().fg(color)
    }

    /// Style for the active tab label.
    pub fn tab_active_style(&self) -> Style {
        Style::default()
            .fg(self.text_primary)
            .bg(self.tab_active)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for an inactive tab label.
    pub fn tab_inactive_style(&self) -> Style {
        Style::default().fg(self.tab_inactive)
    }

    /// Style for a toast notification.
    pub fn toast_style(&self, style: super::app::ToastStyle) -> Style {
        let color = match style {
            super::app::ToastStyle::Info => self.toast_info,
            super::app::ToastStyle::Success => self.toast_success,
            super::app::ToastStyle::Error => self.toast_error,
        };
        Style::default()
            .fg(Color::Rgb(21, 24, 35))
            .bg(color)
            .add_modifier(Modifier::BOLD)
    }

    /// Colour for a usage bar at the given percentage.
    pub fn usage_bar_color(&self, pct: f64) -> Color {
        if pct > 90.0 {
            self.usage_high
        } else if pct >= 70.0 {
            self.usage_medium
        } else {
            self.usage_low
        }
    }

    /// Build a pill-style `Style` for a GitHub label with an optional hex colour.
    ///
    /// When a colour is provided, auto-selects a contrasting foreground (black
    /// or white) based on relative luminance.
    pub fn label_pill_style(&self, hex_color: Option<&str>) -> Style {
        let Some(bg) = hex_color.and_then(parse_hex_color) else {
            return Style::default()
                .fg(self.text_primary)
                .bg(Color::Rgb(50, 55, 75))
                .add_modifier(Modifier::BOLD);
        };
        let fg = contrasting_fg(bg);
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD)
    }
}

/// Parse a hex colour string (e.g. `"d73a4a"` or `"#d73a4a"`) into an RGB colour.
pub fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

/// Pick black or white foreground based on background luminance (WCAG contrast).
fn contrasting_fg(bg: Color) -> Color {
    let Color::Rgb(r, g, b) = bg else {
        return Color::White;
    };
    // Relative luminance (sRGB linearised, simplified)
    let luminance = 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b);
    if luminance > 140.0 {
        Color::Rgb(30, 30, 30)
    } else {
        Color::Rgb(255, 255, 255)
    }
}

// ── Config deserialization ────────────────────────────────────────────

/// All-optional mirror of [`Theme`] for `config.toml` `[theme]` section.
///
/// Only `Some` fields override the default; everything else keeps its default.
#[derive(Debug, Default, Deserialize, Clone)]
pub struct ThemeConfig {
    pub border_focused: Option<String>,
    pub border_unfocused: Option<String>,

    pub text_primary: Option<String>,
    pub text_secondary: Option<String>,
    pub text_accent: Option<String>,

    pub status_draft: Option<String>,
    pub status_pending: Option<String>,
    pub status_working: Option<String>,
    pub status_interrupted: Option<String>,
    pub status_in_review: Option<String>,
    pub status_conflict: Option<String>,
    pub status_ci_failed: Option<String>,
    pub status_ci_running: Option<String>,
    pub status_ci_passed: Option<String>,
    pub status_done: Option<String>,
    pub status_error: Option<String>,
    pub status_paused: Option<String>,
    pub status_waiting: Option<String>,

    pub accent_primary: Option<String>,
    pub accent_secondary: Option<String>,
    pub accent_tertiary: Option<String>,

    pub toast_info: Option<String>,
    pub toast_success: Option<String>,
    pub toast_error: Option<String>,

    pub usage_low: Option<String>,
    pub usage_medium: Option<String>,
    pub usage_high: Option<String>,

    pub form_border_task: Option<String>,
    pub form_border_project: Option<String>,
    pub form_highlight: Option<String>,
    pub form_dim: Option<String>,

    pub tab_active: Option<String>,
    pub tab_inactive: Option<String>,

    pub selection_indicator: Option<String>,
    pub pr_link: Option<String>,
    pub spinner: Option<String>,
    pub rate_limit_warning: Option<String>,
}

/// Parse a colour string into a ratatui `Color`.
///
/// Supports named colours (`"cyan"`, `"red"`, `"dark_gray"`, etc.) and
/// `"rgb(R,G,B)"` syntax.
fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    // Try rgb(R,G,B)
    if let Some(inner) = s.strip_prefix("rgb(").and_then(|r| r.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() == 3 {
            let r = parts[0].trim().parse::<u8>().ok()?;
            let g = parts[1].trim().parse::<u8>().ok()?;
            let b = parts[2].trim().parse::<u8>().ok()?;
            return Some(Color::Rgb(r, g, b));
        }
        return None;
    }

    // Named colours (case-insensitive, with underscore tolerance)
    let lower = s.to_lowercase().replace('-', "_");
    match lower.as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "gray" | "grey" => Some(Color::Gray),
        "dark_gray" | "dark_grey" | "darkgray" | "darkgrey" => Some(Color::DarkGray),
        "light_red" | "lightred" => Some(Color::LightRed),
        "light_green" | "lightgreen" => Some(Color::LightGreen),
        "light_yellow" | "lightyellow" => Some(Color::LightYellow),
        "light_blue" | "lightblue" => Some(Color::LightBlue),
        "light_magenta" | "lightmagenta" => Some(Color::LightMagenta),
        "light_cyan" | "lightcyan" => Some(Color::LightCyan),
        "white" => Some(Color::White),
        _ => None,
    }
}

/// Apply an optional config field: if the string parses to a valid colour,
/// overwrite `target`.
fn apply(target: &mut Color, source: Option<&String>) {
    if let Some(s) = source
        && let Some(color) = parse_color(s)
    {
        *target = color;
    }
}

impl ThemeConfig {
    /// Build a `Theme` starting from defaults, overriding any fields that were
    /// set in the config file.
    pub fn build(&self) -> Theme {
        let mut t = Theme::default();

        apply(&mut t.border_focused, self.border_focused.as_ref());
        apply(&mut t.border_unfocused, self.border_unfocused.as_ref());
        apply(&mut t.text_primary, self.text_primary.as_ref());
        apply(&mut t.text_secondary, self.text_secondary.as_ref());
        apply(&mut t.text_accent, self.text_accent.as_ref());
        apply(&mut t.status_draft, self.status_draft.as_ref());
        apply(&mut t.status_pending, self.status_pending.as_ref());
        apply(&mut t.status_working, self.status_working.as_ref());
        apply(&mut t.status_interrupted, self.status_interrupted.as_ref());
        apply(&mut t.status_in_review, self.status_in_review.as_ref());
        apply(&mut t.status_conflict, self.status_conflict.as_ref());
        apply(&mut t.status_ci_failed, self.status_ci_failed.as_ref());
        apply(&mut t.status_ci_running, self.status_ci_running.as_ref());
        apply(&mut t.status_ci_passed, self.status_ci_passed.as_ref());
        apply(&mut t.status_done, self.status_done.as_ref());
        apply(&mut t.status_error, self.status_error.as_ref());
        apply(&mut t.status_paused, self.status_paused.as_ref());
        apply(&mut t.status_waiting, self.status_waiting.as_ref());
        apply(&mut t.accent_primary, self.accent_primary.as_ref());
        apply(&mut t.accent_secondary, self.accent_secondary.as_ref());
        apply(&mut t.accent_tertiary, self.accent_tertiary.as_ref());
        apply(&mut t.toast_info, self.toast_info.as_ref());
        apply(&mut t.toast_success, self.toast_success.as_ref());
        apply(&mut t.toast_error, self.toast_error.as_ref());
        apply(&mut t.usage_low, self.usage_low.as_ref());
        apply(&mut t.usage_medium, self.usage_medium.as_ref());
        apply(&mut t.usage_high, self.usage_high.as_ref());
        apply(&mut t.form_border_task, self.form_border_task.as_ref());
        apply(
            &mut t.form_border_project,
            self.form_border_project.as_ref(),
        );
        apply(&mut t.form_highlight, self.form_highlight.as_ref());
        apply(&mut t.form_dim, self.form_dim.as_ref());
        apply(&mut t.tab_active, self.tab_active.as_ref());
        apply(&mut t.tab_inactive, self.tab_inactive.as_ref());
        apply(
            &mut t.selection_indicator,
            self.selection_indicator.as_ref(),
        );
        apply(&mut t.pr_link, self.pr_link.as_ref());
        apply(&mut t.spinner, self.spinner.as_ref());
        apply(&mut t.rate_limit_warning, self.rate_limit_warning.as_ref());

        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_has_expected_colors() {
        let t = Theme::default();
        assert_eq!(t.border_focused, Color::Rgb(138, 108, 255));
        assert_eq!(t.status_conflict, Color::Rgb(255, 150, 90));
        assert_eq!(t.text_primary, Color::Rgb(230, 225, 245));
        assert_eq!(t.sidebar_surface().bg, Some(Color::Rgb(22, 24, 38)));
        assert_eq!(t.overlay_surface().bg, Some(Color::Rgb(26, 24, 42)));
    }

    #[test]
    fn parse_named_colors() {
        assert_eq!(parse_color("cyan"), Some(Color::Cyan));
        assert_eq!(parse_color("dark_gray"), Some(Color::DarkGray));
        assert_eq!(parse_color("DarkGray"), Some(Color::DarkGray));
        assert_eq!(parse_color("light_red"), Some(Color::LightRed));
        assert_eq!(parse_color("white"), Some(Color::White));
        assert_eq!(parse_color("nope"), None);
    }

    #[test]
    fn parse_rgb_color() {
        assert_eq!(
            parse_color("rgb(255, 165, 0)"),
            Some(Color::Rgb(255, 165, 0))
        );
        assert_eq!(parse_color("rgb(0,0,0)"), Some(Color::Rgb(0, 0, 0)));
        assert_eq!(parse_color("rgb(256,0,0)"), None); // overflow
        assert_eq!(parse_color("rgb(1,2)"), None); // too few
    }

    #[test]
    fn theme_config_overrides() {
        let cfg = ThemeConfig {
            border_focused: Some("red".into()),
            status_conflict: Some("rgb(100,200,50)".into()),
            ..Default::default()
        };
        let t = cfg.build();
        assert_eq!(t.border_focused, Color::Red);
        assert_eq!(t.status_conflict, Color::Rgb(100, 200, 50));
        // Non-overridden field keeps default
        assert_eq!(t.text_primary, Color::Rgb(230, 225, 245));
    }

    #[test]
    fn task_status_style_maps_correctly() {
        let t = Theme::default();
        assert_eq!(
            t.task_status_style(TaskStatus::Working),
            Style::default().fg(t.status_working)
        );
        assert_eq!(
            t.task_status_style(TaskStatus::Error),
            Style::default().fg(t.status_error)
        );
    }

    #[test]
    fn claude_status_style_maps_correctly() {
        let t = Theme::default();
        assert_eq!(
            t.claude_status_style(ClaudeStatus::Working),
            Style::default().fg(t.status_working)
        );
        assert_eq!(
            t.claude_status_style(ClaudeStatus::Done),
            Style::default().fg(t.status_done)
        );
    }

    #[test]
    fn thread_status_style_maps_correctly() {
        let t = Theme::default();
        assert_eq!(
            t.thread_status_style(ThreadStatus::Running),
            Style::default().fg(t.status_working)
        );
        assert_eq!(
            t.thread_status_style(ThreadStatus::WaitingReview),
            Style::default().fg(t.status_in_review)
        );
    }

    #[test]
    fn usage_bar_color_thresholds() {
        let t = Theme::default();
        assert_eq!(t.usage_bar_color(50.0), t.usage_low);
        assert_eq!(t.usage_bar_color(75.0), t.usage_medium);
        assert_eq!(t.usage_bar_color(95.0), t.usage_high);
    }

    #[test]
    fn focused_and_unfocused_border_styles() {
        let t = Theme::default();
        assert_eq!(
            t.focused_border(),
            Style::default()
                .fg(t.border_focused)
                .add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            t.unfocused_border(),
            Style::default().fg(t.border_unfocused)
        );
    }

    #[test]
    fn tab_styles() {
        let t = Theme::default();
        let active = t.tab_active_style();
        assert_eq!(active.fg, Some(t.text_primary));
        assert_eq!(active.bg, Some(t.tab_active));
        assert!(active.add_modifier.contains(Modifier::BOLD));
        assert!(!active.add_modifier.contains(Modifier::REVERSED));

        let inactive = t.tab_inactive_style();
        assert_eq!(inactive.fg, Some(t.tab_inactive));
    }

    #[test]
    fn paused_style_uses_paused_color() {
        let t = Theme::default();
        assert_eq!(t.paused_style(), Style::default().fg(t.status_paused));
    }

    #[test]
    fn waiting_style_uses_waiting_color() {
        let t = Theme::default();
        assert_eq!(t.waiting_style(), Style::default().fg(t.status_waiting));
    }

    #[test]
    fn toast_styles() {
        use crate::tui::app::ToastStyle;
        let t = Theme::default();

        let info = t.toast_style(ToastStyle::Info);
        assert_eq!(info.bg, Some(t.toast_info));
        assert!(info.add_modifier.contains(Modifier::BOLD));

        let success = t.toast_style(ToastStyle::Success);
        assert_eq!(success.bg, Some(t.toast_success));

        let error = t.toast_style(ToastStyle::Error);
        assert_eq!(error.bg, Some(t.toast_error));
    }

    #[test]
    fn all_task_statuses_have_styles() {
        let t = Theme::default();
        let statuses = [
            TaskStatus::Draft,
            TaskStatus::Pending,
            TaskStatus::Working,
            TaskStatus::Interrupted,
            TaskStatus::InReview,
            TaskStatus::Conflict,
            TaskStatus::CiFailed,
            TaskStatus::Done,
            TaskStatus::Error,
        ];
        for status in statuses {
            let style = t.task_status_style(status);
            assert!(style.fg.is_some(), "missing style for {status:?}");
        }
    }

    #[test]
    fn all_claude_statuses_have_styles() {
        let t = Theme::default();
        let statuses = [
            ClaudeStatus::Working,
            ClaudeStatus::Interrupted,
            ClaudeStatus::Error,
            ClaudeStatus::Done,
            ClaudeStatus::Idle,
        ];
        for status in statuses {
            let style = t.claude_status_style(status);
            assert!(style.fg.is_some(), "missing style for {status:?}");
        }
    }

    #[test]
    fn usage_bar_color_boundary_values() {
        let t = Theme::default();
        // Exactly at thresholds
        assert_eq!(t.usage_bar_color(70.0), t.usage_medium);
        assert_eq!(t.usage_bar_color(90.0), t.usage_medium);
        assert_eq!(t.usage_bar_color(90.1), t.usage_high);
        assert_eq!(t.usage_bar_color(0.0), t.usage_low);
        assert_eq!(t.usage_bar_color(69.9), t.usage_low);
    }
}
