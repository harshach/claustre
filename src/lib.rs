//! Claustre shared library.
//!
//! Re-exports all modules so they can be used by both the CLI binary
//! and the Tauri desktop app.

pub mod config;
pub mod configure;
pub mod conversation;
pub mod github;
pub mod github_app;
pub mod knowledge;
pub mod pty;
pub mod runtime;
pub mod scanner;
pub mod session;
pub mod session_host;
pub mod session_update;
pub mod skills;
pub mod store;
pub mod sync;
pub mod threads;
pub mod tui;
pub mod update;
pub mod workflows;
