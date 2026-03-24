//! The `Terminal` trait: a unified interface for local and remote terminal backends.
//!
//! Both `EmbeddedTerminal` (local PTY) and `RemoteTerminal` (session-host socket)
//! implement this trait so that `SessionTerminals` and the TUI rendering code can
//! work with either backend interchangeably.

use anyhow::Result;

/// Unified interface for terminal backends (local PTY or remote session-host).
///
/// Captures the public surface of `EmbeddedTerminal` so that `SessionTerminals`
/// and TUI rendering code can operate on any backend without knowing the
/// concrete type.
pub(crate) trait Terminal {
    /// Drain pending output from the backend and feed it to the vt100 parser.
    ///
    /// Processing is capped at a byte budget per call so the UI thread is never
    /// blocked for too long on a burst of output.
    fn process_output(&mut self);

    /// Like [`Terminal::process_output`] but drains the entire backlog without
    /// a byte budget. Used when switching to a session tab.
    fn process_output_full(&mut self);

    /// Send raw bytes (keystrokes) to the child process or remote host.
    fn send_bytes(&mut self, bytes: &[u8]) -> Result<()>;

    /// Resize the terminal to the given dimensions.
    fn resize(&mut self, rows: u16, cols: u16) -> Result<()>;

    /// Clear the screen buffer (erase display + home cursor).
    fn clear_screen(&mut self);

    /// Get the current terminal screen state for rendering.
    fn screen(&self) -> &vt100::Screen;

    /// Get the user's current scroll offset (0 = live screen, >0 = lines into history).
    fn scrollback(&self) -> usize;

    /// Whether mouse events should be forwarded to the PTY application.
    fn should_forward_mouse(&self) -> bool;

    /// The mouse protocol mode requested by the PTY application.
    fn mouse_protocol_mode(&self) -> vt100::MouseProtocolMode;

    /// The mouse protocol encoding requested by the PTY application.
    fn mouse_protocol_encoding(&self) -> vt100::MouseProtocolEncoding;

    /// Scroll up into history by `lines` rows.
    fn scroll_up(&mut self, lines: usize);

    /// Scroll down toward the live screen by `lines` rows.
    fn scroll_down(&mut self, lines: usize);

    /// Reset scrollback to the live screen (offset = 0).
    fn reset_scrollback(&mut self);

    /// Set the parser's scrollback to the user's scroll position for rendering.
    /// Must be paired with [`Terminal::restore_after_render`].
    fn prepare_for_render(&mut self);

    /// Restore the parser to the live screen after rendering.
    fn restore_after_render(&mut self);

    /// Whether the child process has exited (reader thread ended).
    fn exited(&self) -> bool;

    /// Request the backend to shut down gracefully. Default: no-op.
    fn request_shutdown(&mut self) {
        // Default no-op for local terminals (dropping the PTY handles is enough).
    }
}
