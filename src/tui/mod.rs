//! Terminal UI built on ratatui.
//!
//! Manages the app state machine, crossterm event loop, and rendering
//! for the dashboard, session tabs, and overlay panels.

mod app;
mod event;
pub mod form;
pub mod keymap;
pub mod theme;
mod ui;

use std::io::stdout;

use anyhow::Result;
use crossterm::event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste};
use crossterm::execute;

use crate::store::Store;

/// Initialize file-based debug logging to `~/.claustre/tui.log`.
///
/// Must be called before `ratatui::init()` takes over stdout/stderr.
/// Uses the `tracing` crate — all `tracing::debug!`, `tracing::info!`,
/// `tracing::warn!`, `tracing::error!` calls write to this file.
fn init_logging() {
    use tracing_subscriber::{EnvFilter, fmt};

    let log_dir = dirs::home_dir()
        .map(|h| h.join(".claustre"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));

    let log_path = log_dir.join("tui.log");

    // Truncate the log on each TUI launch to keep it small
    let log_file = match std::fs::File::create(&log_path) {
        Ok(f) => f,
        Err(_) => return, // silently skip if we can't create the log file
    };

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("claustre=debug"));

    let subscriber = fmt::Subscriber::builder()
        .with_env_filter(filter)
        .with_writer(std::sync::Mutex::new(log_file))
        .with_ansi(false)
        .with_target(false)
        .with_thread_ids(false)
        .compact()
        .finish();

    // Ignore error if already initialized (e.g., tests)
    let _ = tracing::subscriber::set_global_default(subscriber);
}

pub fn run(store: Store) -> Result<()> {
    init_logging();

    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);

    let result = app::App::new(store).and_then(|mut app| app.run(&mut terminal));

    let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    result
}
