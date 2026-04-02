//! `ScreenView` trait: abstract read-only access to terminal screen state.
//!
//! Decouples all rendering, selection, and detection code from a specific
//! terminal parser (vt100 today, libghostty later).  Every consumer of
//! terminal screen data should go through `ScreenView` instead of
//! `vt100::Screen` directly.

// ── Color & mouse protocol types ──

/// Terminal cell color, parser-agnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermColor {
    /// The terminal's default foreground/background.
    Default,
    /// A 256-color palette index (0–255).
    Idx(u8),
    /// 24-bit true color.
    Rgb(u8, u8, u8),
}

/// Mouse protocol tracking mode requested by the PTY application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    /// No mouse tracking enabled.
    None,
    /// Report button press events only (mode 9).
    Press,
    /// Report button press and release events (mode 1000).
    PressRelease,
    /// Report button-event tracking / motion while pressed (mode 1002).
    ButtonMotion,
    /// Report all motion events (mode 1003).
    AnyMotion,
}

/// Mouse protocol encoding requested by the PTY application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEncoding {
    /// Default X10 encoding.
    Default,
    /// UTF-8 extended encoding (mode 1005).
    Utf8,
    /// SGR extended encoding (mode 1006).
    Sgr,
}

// ── Cell view ──

/// Snapshot of a single terminal cell's visible properties.
///
/// Cheap to construct — `contents` is typically 0–2 chars.
pub struct CellView {
    pub contents: String,
    pub fg: TermColor,
    pub bg: TermColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

// ── ScreenView trait ──

/// Abstract read-only view of a terminal screen.
///
/// Implemented by parser-specific wrappers (`Vt100ScreenView`, and later
/// a Ghostty wrapper).  All rendering, selection, and screen-content
/// detection code should depend on this trait, not on a concrete parser.
pub trait ScreenView {
    /// Terminal dimensions as (rows, cols).
    fn size(&self) -> (u16, u16);

    /// Get the cell at the given (row, col) position.
    fn cell(&self, row: u16, col: u16) -> Option<CellView>;

    /// Full screen contents as a string (rows joined by newlines).
    fn contents(&self) -> String;

    /// Extract text between two positions (inclusive).
    fn contents_between(&self, r1: u16, c1: u16, r2: u16, c2: u16) -> String;

    /// Cursor position as (row, col).
    fn cursor_position(&self) -> (u16, u16);

    /// Whether the cursor is currently hidden.
    fn hide_cursor(&self) -> bool;

    /// Whether the terminal is in alternate screen mode.
    fn alternate_screen(&self) -> bool;

    /// Current scrollback offset (0 = live screen).
    fn scrollback(&self) -> usize;

    /// Mouse protocol tracking mode set by the PTY application.
    fn mouse_protocol_mode(&self) -> MouseMode;

    /// Mouse protocol encoding set by the PTY application.
    fn mouse_protocol_encoding(&self) -> MouseEncoding;

    /// Emit ANSI bytes that reconstruct the full screen state.
    ///
    /// Used by session-host for snapshots sent to connecting clients.
    fn state_formatted(&self) -> Vec<u8>;
}

// ── vt100 wrapper ──

/// Zero-cost wrapper around `&vt100::Screen` implementing `ScreenView`.
pub struct Vt100ScreenView<'a>(pub &'a vt100::Screen);

impl ScreenView for Vt100ScreenView<'_> {
    fn size(&self) -> (u16, u16) {
        self.0.size()
    }

    fn cell(&self, row: u16, col: u16) -> Option<CellView> {
        let c = self.0.cell(row, col)?;
        Some(CellView {
            contents: c.contents(),
            fg: vt100_color(c.fgcolor()),
            bg: vt100_color(c.bgcolor()),
            bold: c.bold(),
            italic: c.italic(),
            underline: c.underline(),
            inverse: c.inverse(),
        })
    }

    fn contents(&self) -> String {
        self.0.contents()
    }

    fn contents_between(&self, r1: u16, c1: u16, r2: u16, c2: u16) -> String {
        self.0.contents_between(r1, c1, r2, c2)
    }

    fn cursor_position(&self) -> (u16, u16) {
        self.0.cursor_position()
    }

    fn hide_cursor(&self) -> bool {
        self.0.hide_cursor()
    }

    fn alternate_screen(&self) -> bool {
        self.0.alternate_screen()
    }

    fn scrollback(&self) -> usize {
        self.0.scrollback()
    }

    fn mouse_protocol_mode(&self) -> MouseMode {
        vt100_mouse_mode(self.0.mouse_protocol_mode())
    }

    fn mouse_protocol_encoding(&self) -> MouseEncoding {
        vt100_mouse_encoding(self.0.mouse_protocol_encoding())
    }

    fn state_formatted(&self) -> Vec<u8> {
        self.0.state_formatted()
    }
}

// ── Conversion helpers ──

fn vt100_color(c: vt100::Color) -> TermColor {
    match c {
        vt100::Color::Default => TermColor::Default,
        vt100::Color::Idx(i) => TermColor::Idx(i),
        vt100::Color::Rgb(r, g, b) => TermColor::Rgb(r, g, b),
    }
}

fn vt100_mouse_mode(m: vt100::MouseProtocolMode) -> MouseMode {
    match m {
        vt100::MouseProtocolMode::None => MouseMode::None,
        vt100::MouseProtocolMode::Press => MouseMode::Press,
        vt100::MouseProtocolMode::PressRelease => MouseMode::PressRelease,
        vt100::MouseProtocolMode::ButtonMotion => MouseMode::ButtonMotion,
        vt100::MouseProtocolMode::AnyMotion => MouseMode::AnyMotion,
    }
}

fn vt100_mouse_encoding(e: vt100::MouseProtocolEncoding) -> MouseEncoding {
    match e {
        vt100::MouseProtocolEncoding::Default => MouseEncoding::Default,
        vt100::MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
        vt100::MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
    }
}

// ── ratatui integration ──

/// Convert a `TermColor` to a `ratatui::style::Color`.
impl From<TermColor> for ratatui::style::Color {
    fn from(c: TermColor) -> Self {
        match c {
            TermColor::Default => Self::Reset,
            TermColor::Idx(i) => Self::Indexed(i),
            TermColor::Rgb(r, g, b) => Self::Rgb(r, g, b),
        }
    }
}

/// Borrowed parser-agnostic terminal screen view.
///
/// This keeps `Terminal::screen_view()` independent from a concrete parser
/// type while still remaining allocation-free and object-safe.
pub enum ScreenViewRef<'a> {
    Vt100(Vt100ScreenView<'a>),
}

impl ScreenView for ScreenViewRef<'_> {
    fn size(&self) -> (u16, u16) {
        match self {
            Self::Vt100(view) => view.size(),
        }
    }

    fn cell(&self, row: u16, col: u16) -> Option<CellView> {
        match self {
            Self::Vt100(view) => view.cell(row, col),
        }
    }

    fn contents(&self) -> String {
        match self {
            Self::Vt100(view) => view.contents(),
        }
    }

    fn contents_between(&self, r1: u16, c1: u16, r2: u16, c2: u16) -> String {
        match self {
            Self::Vt100(view) => view.contents_between(r1, c1, r2, c2),
        }
    }

    fn cursor_position(&self) -> (u16, u16) {
        match self {
            Self::Vt100(view) => view.cursor_position(),
        }
    }

    fn hide_cursor(&self) -> bool {
        match self {
            Self::Vt100(view) => view.hide_cursor(),
        }
    }

    fn alternate_screen(&self) -> bool {
        match self {
            Self::Vt100(view) => view.alternate_screen(),
        }
    }

    fn scrollback(&self) -> usize {
        match self {
            Self::Vt100(view) => view.scrollback(),
        }
    }

    fn mouse_protocol_mode(&self) -> MouseMode {
        match self {
            Self::Vt100(view) => view.mouse_protocol_mode(),
        }
    }

    fn mouse_protocol_encoding(&self) -> MouseEncoding {
        match self {
            Self::Vt100(view) => view.mouse_protocol_encoding(),
        }
    }

    fn state_formatted(&self) -> Vec<u8> {
        match self {
            Self::Vt100(view) => view.state_formatted(),
        }
    }
}
