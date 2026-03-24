//! Remote terminal backend: connects to a session-host via Unix socket.
//!
//! `RemoteTerminal` implements the `Terminal` trait by proxying I/O over the
//! session-host wire protocol.  The local vt100 parser is fed output frames
//! from the host so `screen()` always reflects the current terminal state.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result};

use super::protocol::{ClientMessage, HostMessage};
use super::terminal_trait::Terminal;
use super::{PROCESS_BYTE_BUDGET, SCROLL_DOWN_ACCEL_DIVISOR, SCROLLBACK_LINES};

/// Output received from the session-host reader thread.
enum RemoteOutput {
    /// Raw PTY output bytes to feed to the local parser.
    Data(Vec<u8>),
    /// The child process on the host exited with the given code.
    Exited(i32),
}

/// Header size: 1-byte type + 4-byte payload length.
const HEADER_LEN: usize = 5;

/// Maximum payload size we accept from the host (16 MB).
const MAX_PAYLOAD_SIZE: usize = 16 * 1024 * 1024;

/// A terminal backed by a session-host Unix socket connection.
///
/// Implements the same scrollback architecture as `EmbeddedTerminal`: the
/// parser is always at scrollback 0 outside the render phase, and
/// `scroll_offset` is pure arithmetic tracked locally.
pub(crate) struct RemoteTerminal {
    /// Socket for sending `ClientMessage`s to the host.
    stream: UnixStream,
    /// Receiver for host output (from the reader thread).
    output_rx: mpsc::Receiver<RemoteOutput>,
    /// Local vt100 parser — fed with host output for rendering.
    parser: vt100::Parser,
    /// Whether the host reported child exit.
    has_exited: bool,
    /// User-controlled scroll position (0 = live screen).
    scroll_offset: usize,
    /// Maximum scrollback lines currently available.
    available_scrollback: usize,
    /// Session identifier (for logging / diagnostics).
    #[expect(dead_code, reason = "retained for diagnostic logging")]
    session_id: String,
    /// Whether to send Shutdown on drop.
    shutdown_on_drop: bool,
}

impl RemoteTerminal {
    /// Connect to a running session-host and synchronize terminal state.
    ///
    /// 1. Resolves the socket path via `config::session_socket_path`.
    /// 2. Connects and reads the initial `Snapshot` message.
    /// 3. Spawns a reader thread for subsequent `HostMessage`s.
    /// 4. Sends an initial `Resize` to match the requested dimensions.
    pub fn connect(session_id: &str, rows: u16, cols: u16) -> Result<Self> {
        let socket_path = crate::config::session_socket_path(session_id)?;
        let stream = UnixStream::connect(&socket_path).with_context(|| {
            format!(
                "failed to connect to session-host socket at {}",
                socket_path.display()
            )
        })?;

        // Clone stream for the reader thread before we set up the writer.
        let reader_stream = stream
            .try_clone()
            .context("failed to clone socket for reader thread")?;

        // Read the initial Snapshot from the socket (blocking).
        let snapshot_bytes = read_initial_snapshot(&stream)?;

        // Initialize parser and feed snapshot.
        let mut parser = vt100::Parser::new(rows, cols, SCROLLBACK_LINES);
        parser.process(&snapshot_bytes);

        // Spawn reader thread for subsequent messages.
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            reader_thread(reader_stream, tx);
        });

        // Build the terminal before sending resize (we need a &mut for the write).
        let mut term = Self {
            stream,
            output_rx: rx,
            parser,
            has_exited: false,
            scroll_offset: 0,
            available_scrollback: 0,
            session_id: session_id.to_string(),
            shutdown_on_drop: false,
        };

        // Send initial resize so the host PTY matches our dimensions.
        term.send_client_message(&ClientMessage::Resize { cols, rows })?;

        Ok(term)
    }

    /// Send a `ClientMessage` to the session-host.
    fn send_client_message(&mut self, msg: &ClientMessage) -> Result<()> {
        self.stream
            .write_all(&msg.encode())
            .context("failed to write client message to session-host")?;
        self.stream
            .flush()
            .context("failed to flush client message")?;
        Ok(())
    }

    /// Inner implementation for `process_output` with optional byte budget.
    fn process_output_inner(&mut self, budget: Option<usize>) {
        // Force parser to live screen so auto-increment never fires.
        self.parser.set_scrollback(0);

        let mut bytes_processed: usize = 0;
        loop {
            match self.output_rx.try_recv() {
                Ok(RemoteOutput::Data(bytes)) => {
                    bytes_processed += bytes.len();
                    self.parser.process(&bytes);
                    if let Some(limit) = budget
                        && bytes_processed >= limit
                    {
                        break;
                    }
                }
                Ok(RemoteOutput::Exited(_code)) => {
                    self.has_exited = true;
                    // Reset terminal modes (same cleanup as EmbeddedTerminal).
                    self.parser.process(
                        concat!(
                            "\x1b[?1000l",
                            "\x1b[?1002l",
                            "\x1b[?1003l",
                            "\x1b[?1006l",
                            "\x1b[?2004l",
                            "\x1b[?1049l",
                        )
                        .as_bytes(),
                    );
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.has_exited = true;
                    self.parser.process(
                        concat!(
                            "\x1b[?1000l",
                            "\x1b[?1002l",
                            "\x1b[?1003l",
                            "\x1b[?1006l",
                            "\x1b[?2004l",
                            "\x1b[?1049l",
                        )
                        .as_bytes(),
                    );
                    break;
                }
            }
        }

        // Handle alternate screen (same logic as EmbeddedTerminal).
        if self.parser.screen().alternate_screen() {
            self.scroll_offset = 0;
            self.available_scrollback = 0;
            self.parser.set_scrollback(0);
            return;
        }

        // Query available scrollback and clamp.
        self.parser.set_scrollback(usize::MAX);
        self.available_scrollback = self.parser.screen().scrollback();
        self.parser.set_scrollback(0);
        self.scroll_offset = self.scroll_offset.min(self.available_scrollback);
    }
}

impl Terminal for RemoteTerminal {
    fn process_output(&mut self) {
        self.process_output_inner(Some(PROCESS_BYTE_BUDGET));
    }

    fn process_output_full(&mut self) {
        self.process_output_inner(None);
    }

    fn send_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.send_client_message(&ClientMessage::Input(bytes.to_vec()))
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.send_client_message(&ClientMessage::Resize { cols, rows })?;
        self.parser.set_size(rows, cols);
        self.scroll_offset = 0;
        self.parser.set_scrollback(0);
        Ok(())
    }

    fn clear_screen(&mut self) {
        self.parser.process(b"\x1b[2J\x1b[H");
    }

    fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    fn scrollback(&self) -> usize {
        self.scroll_offset
    }

    fn should_forward_mouse(&self) -> bool {
        !self.has_exited && self.mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    fn mouse_protocol_mode(&self) -> vt100::MouseProtocolMode {
        self.parser.screen().mouse_protocol_mode()
    }

    fn mouse_protocol_encoding(&self) -> vt100::MouseProtocolEncoding {
        self.parser.screen().mouse_protocol_encoding()
    }

    fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = (self.scroll_offset + lines).min(self.available_scrollback);
    }

    fn scroll_down(&mut self, lines: usize) {
        let effective = lines.max(self.scroll_offset / SCROLL_DOWN_ACCEL_DIVISOR);
        let new_offset = self.scroll_offset.saturating_sub(effective);
        let snap_zone = usize::from(self.parser.screen().size().0);
        if new_offset <= snap_zone {
            self.scroll_offset = 0;
        } else {
            self.scroll_offset = new_offset;
        }
    }

    fn reset_scrollback(&mut self) {
        self.scroll_offset = 0;
    }

    fn prepare_for_render(&mut self) {
        self.parser.set_scrollback(self.scroll_offset);
    }

    fn restore_after_render(&mut self) {
        self.parser.set_scrollback(0);
    }

    fn exited(&self) -> bool {
        self.has_exited
    }

    fn request_shutdown(&mut self) {
        let _ = self.send_client_message(&ClientMessage::Shutdown);
    }
}

impl Drop for RemoteTerminal {
    fn drop(&mut self) {
        if self.shutdown_on_drop {
            self.request_shutdown();
        }
    }
}

// -- Private helpers ----------------------------------------------------------

/// Read the initial `Snapshot` message from the socket (blocking).
///
/// The session-host sends a `Snapshot` immediately on connection. We read
/// exactly one framed message and validate that it is a Snapshot.
fn read_initial_snapshot(stream: &UnixStream) -> Result<Vec<u8>> {
    use std::io::Read;

    // Read header (5 bytes).
    let mut header = [0u8; HEADER_LEN];
    let mut stream_reader = stream;
    stream_reader
        .read_exact(&mut header)
        .context("failed to read snapshot header from session-host")?;

    let payload_len = u32::from_le_bytes(
        header[1..5]
            .try_into()
            .context("snapshot header payload length corrupted")?,
    ) as usize;

    if payload_len > MAX_PAYLOAD_SIZE {
        anyhow::bail!("snapshot payload size {payload_len} exceeds limit {MAX_PAYLOAD_SIZE}");
    }

    // Build full frame and read payload.
    let mut frame = Vec::with_capacity(HEADER_LEN + payload_len);
    frame.extend_from_slice(&header);
    frame.resize(HEADER_LEN + payload_len, 0);
    if payload_len > 0 {
        stream_reader
            .read_exact(&mut frame[HEADER_LEN..])
            .context("failed to read snapshot payload from session-host")?;
    }

    let msg = HostMessage::decode(&frame).context("failed to decode initial snapshot")?;
    match msg {
        HostMessage::Snapshot(data) => Ok(data),
        other => anyhow::bail!(
            "expected Snapshot as first message, got {:?}",
            std::mem::discriminant(&other)
        ),
    }
}

/// Reader thread: continuously reads `HostMessage` frames from the socket
/// and sends parsed output to the mpsc channel.
#[expect(
    clippy::needless_pass_by_value,
    reason = "thread entry point must own the sender"
)]
fn reader_thread(stream: UnixStream, tx: mpsc::Sender<RemoteOutput>) {
    use std::io::Read;

    // Set non-blocking so we can detect disconnection without blocking forever.
    // We use a small poll loop instead of blocking reads so the thread can exit
    // promptly when the channel receiver is dropped.
    let _ = stream.set_nonblocking(true);
    let mut stream_reader = stream;
    let mut buf = Vec::new();

    loop {
        // Try to read available data.  Heap-allocated to avoid a 32 KB stack frame.
        let mut tmp = vec![0u8; 32_768];
        match stream_reader.read(&mut tmp) {
            Ok(0) => break, // EOF — host disconnected.
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No data available — process any buffered frames, then sleep briefly.
                if buf.is_empty() {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
            }
            Err(_) => break, // Read error — assume host is gone.
        }

        // Parse complete frames from the buffer.
        while buf.len() >= HEADER_LEN {
            let payload_len = u32::from_le_bytes(buf[1..5].try_into().unwrap_or([0; 4])) as usize;

            let frame_len = HEADER_LEN + payload_len;
            if buf.len() < frame_len {
                break; // Incomplete frame — wait for more data.
            }

            // Decode the frame.
            let frame = &buf[..frame_len];
            if let Ok(msg) = HostMessage::decode(frame) {
                let output = match msg {
                    HostMessage::Snapshot(data) | HostMessage::Output(data) => {
                        RemoteOutput::Data(data)
                    }
                    HostMessage::Exited(code) => RemoteOutput::Exited(code),
                };
                if tx.send(output).is_err() {
                    return; // Receiver dropped — terminal was deallocated.
                }
            }

            // Remove the consumed frame from the buffer.
            buf.drain(..frame_len);
        }
    }
}
