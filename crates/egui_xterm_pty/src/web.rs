//! Browser (`wasm32-unknown-unknown`) and transport-agnostic Web PTY session support.
//!
//! - [`WebPtyConfig`], [`WebPtyState`], and [`WebPtySession`] are compiled on **all targets**
//!   (desktop and `wasm32`) so downstream applications can build custom transports (WebSockets,
//!   WebTransport, WebRTC DataChannels, Web Workers, or IPC bridges) and test full Web PTY
//!   sessions deterministically on native targets.
//! - On `wasm32-unknown-unknown`, `WebPtyBackend` wraps [`WebPtySession`] with a live
//!   browser `web_sys::WebSocket` implementing [`TerminalInput`].

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};

use egui_xterm_core::{TerminalInput, TerminalIoError, TerminalSize};

use crate::RepaintCallback;
use crate::protocol::{
    ServerFrame, WebPtyProtocol, WireMessage, append_sanitized_osc_title, clamp_pty_size,
    validate_ws_url,
};

/// Default upper bound for buffered unread bytes received from a remote Web PTY (`4 MiB`).
pub const DEFAULT_MAX_RX_BUFFER_BYTES: usize = 4 * 1024 * 1024;

/// Default upper bound for bytes queued while a Web PTY connection is still handshaking (`256 KiB`).
pub const DEFAULT_MAX_PENDING_TX_BYTES: usize = 256 * 1024;

/// Default upper bound for undrained outgoing [`WireMessage`] frames queued in [`WebPtySession`].
const MAX_OUTGOING_FRAMES: usize = 1024;

/// Minimum allowed receive buffer capacity to avoid unusable configurations.
const MIN_RX_BUFFER_BYTES: usize = 4096;

/// Generic transport error text emitted when a browser `WebSocket` fires an `error` event without detail.
const GENERIC_WS_ERROR_MSG: &str = "WebSocket transport error";

type SharedRepaint = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;

/// Connection lifecycle state of a Web PTY session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebPtyState {
    /// WebSocket handshake is in progress; outgoing bytes are buffered and resizes are coalesced.
    Connecting,
    /// Connection is open and streaming terminal I/O.
    Open,
    /// Graceful close has been requested locally and awaits remote close completion.
    Closing,
    /// Connection has permanently terminated.
    Closed,
}

/// Configuration for connecting to a remote PTY gateway over WebSocket or another message transport.
///
/// # Examples
///
/// ```
/// use egui_xterm_pty::{TerminalSize, WebPtyConfig, WebPtyProtocol};
///
/// let cfg = WebPtyConfig::new("wss://terminal.example.com/ws")
///     .with_protocol(WebPtyProtocol::Ttyd)
///     .with_initial_size(TerminalSize::new(120, 40));
/// assert_eq!(cfg.url, "wss://terminal.example.com/ws");
/// assert_eq!(cfg.protocol, WebPtyProtocol::Ttyd);
/// assert_eq!(cfg.initial_size.cols, 120);
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct WebPtyConfig {
    /// WebSocket endpoint URL (`ws://...` or `wss://...`).
    pub url: String,
    /// Wire framing protocol used for input, output, resize, and control messages.
    pub protocol: WebPtyProtocol,
    /// Optional explicit WebSocket subprotocols. When empty, falls back to
    /// [`WebPtyProtocol::default_subprotocol`] (e.g. `"tty"` for [`WebPtyProtocol::Ttyd`]).
    pub subprotocols: Vec<String>,
    /// Optional authentication token transmitted during initial protocol handshake
    /// (used by [`WebPtyProtocol::Ttyd`]).
    pub auth_token: Option<String>,
    /// Initial character-grid and pixel dimensions advertised once the connection opens.
    pub initial_size: TerminalSize,
    /// Maximum unread incoming bytes retained before backpressure drops excess output.
    /// Values below `4096` are clamped up to `4096` (and `0` selects [`DEFAULT_MAX_RX_BUFFER_BYTES`]).
    pub max_rx_buffer_bytes: usize,
    /// Maximum outgoing bytes buffered while [`WebPtyState::Connecting`].
    /// `0` selects [`DEFAULT_MAX_PENDING_TX_BYTES`].
    pub max_pending_tx_bytes: usize,
}

impl fmt::Debug for WebPtyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted_token = self.auth_token.as_ref().map(|_| "<redacted>");
        f.debug_struct("WebPtyConfig")
            .field("url", &self.url)
            .field("protocol", &self.protocol)
            .field("subprotocols", &self.subprotocols)
            .field("auth_token", &redacted_token)
            .field("initial_size", &self.initial_size)
            .field("max_rx_buffer_bytes", &self.max_rx_buffer_bytes)
            .field("max_pending_tx_bytes", &self.max_pending_tx_bytes)
            .finish()
    }
}

impl WebPtyConfig {
    /// Creates a new [`WebPtyConfig`] targeting `url` with [`WebPtyProtocol::TaggedBinary`].
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            protocol: WebPtyProtocol::default(),
            subprotocols: Vec::new(),
            auth_token: None,
            initial_size: TerminalSize::default(),
            max_rx_buffer_bytes: DEFAULT_MAX_RX_BUFFER_BYTES,
            max_pending_tx_bytes: DEFAULT_MAX_PENDING_TX_BYTES,
        }
    }

    /// Selects the wire framing [`WebPtyProtocol`].
    #[must_use]
    pub const fn with_protocol(mut self, protocol: WebPtyProtocol) -> Self {
        self.protocol = protocol;
        self
    }

    /// Sets explicit WebSocket subprotocols, overriding [`WebPtyProtocol::default_subprotocol`].
    #[must_use]
    pub fn with_subprotocols(
        mut self,
        subprotocols: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.subprotocols = subprotocols.into_iter().map(Into::into).collect();
        self
    }

    /// Attaches an optional authentication token for protocol handshakes (such as `ttyd`).
    #[must_use]
    pub fn with_auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    /// Sets the initial terminal dimensions sent once the connection opens.
    #[must_use]
    pub const fn with_initial_size(mut self, initial_size: TerminalSize) -> Self {
        self.initial_size = initial_size;
        self
    }

    /// Sets the receive buffer byte cap.
    #[must_use]
    pub const fn with_max_rx_buffer_bytes(mut self, bytes: usize) -> Self {
        self.max_rx_buffer_bytes = bytes;
        self
    }

    /// Sets the pre-open outgoing buffer byte cap.
    #[must_use]
    pub const fn with_max_pending_tx_bytes(mut self, bytes: usize) -> Self {
        self.max_pending_tx_bytes = bytes;
        self
    }

    pub(crate) fn effective_max_rx_bytes(&self) -> usize {
        if self.max_rx_buffer_bytes == 0 {
            DEFAULT_MAX_RX_BUFFER_BYTES
        } else {
            self.max_rx_buffer_bytes.max(MIN_RX_BUFFER_BYTES)
        }
    }

    pub(crate) fn effective_max_pending_tx_bytes(&self) -> usize {
        if self.max_pending_tx_bytes == 0 {
            DEFAULT_MAX_PENDING_TX_BYTES
        } else {
            self.max_pending_tx_bytes
        }
    }
}

/// Transport-agnostic Web PTY session state machine implementing [`TerminalInput`].
///
/// Manages URL validation, wire-protocol encoding/decoding, pre-open input buffering and
/// resize coalescing, bounded receive buffering, remote exit codes, non-fatal error reporting,
/// and strict EOF drain semantics.
///
/// On `wasm32-unknown-unknown`, `WebPtyBackend` drives [`WebPtySession`] automatically over
/// `web_sys::WebSocket`. On any target (desktop or web), consumers can also pair
/// [`WebPtySession`] directly with custom message transports (WebTransport, WebRTC
/// DataChannels, Web Workers, IPC, or test harnesses).
pub struct WebPtySession {
    url: String,
    protocol: WebPtyProtocol,
    auth_token: Option<String>,
    state: WebPtyState,
    last_size: TerminalSize,
    max_rx_bytes: usize,
    max_pending_tx_bytes: usize,
    rx_buf: VecDeque<u8>,
    pending_tx: VecDeque<Vec<u8>>,
    pending_tx_bytes: usize,
    outgoing: VecDeque<WireMessage>,
    pending_error: Option<String>,
    exit_code: Option<u32>,
    repaint: SharedRepaint,
}

const _: fn() = || {
    const fn assert_send<T: Send>() {}
    assert_send::<WebPtySession>();
};

impl fmt::Debug for WebPtySession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebPtySession")
            .field("url", &self.url)
            .field("protocol", &self.protocol)
            .field("state", &self.state)
            .field("last_size", &self.last_size)
            .field("rx_len", &self.rx_buf.len())
            .field("pending_tx_bytes", &self.pending_tx_bytes)
            .field("outgoing_len", &self.outgoing.len())
            .field("exit_code", &self.exit_code)
            .finish_non_exhaustive()
    }
}

impl WebPtySession {
    /// Creates a new [`WebPtySession`] in [`WebPtyState::Connecting`] after validating `config.url`.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalIoError::Message`] if `config.url` is not a valid `ws://` or `wss://` URL.
    pub fn new(config: WebPtyConfig) -> Result<Self, TerminalIoError> {
        Self::new_inner(config, None)
    }

    /// Creates a new [`WebPtySession`] with a [`RepaintCallback`] invoked whenever incoming
    /// terminal data, connection state transitions, or errors arrive.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalIoError::Message`] if `config.url` is not a valid `ws://` or `wss://` URL.
    pub fn with_repaint(
        config: WebPtyConfig,
        repaint: RepaintCallback,
    ) -> Result<Self, TerminalIoError> {
        Self::new_inner(config, Some(repaint))
    }

    fn new_inner(
        config: WebPtyConfig,
        repaint: Option<RepaintCallback>,
    ) -> Result<Self, TerminalIoError> {
        validate_ws_url(&config.url)?;
        let max_rx_bytes = config.effective_max_rx_bytes();
        let max_pending_tx_bytes = config.effective_max_pending_tx_bytes();
        let last_size = clamp_pty_size(config.initial_size);

        Ok(Self {
            url: config.url,
            protocol: config.protocol,
            auth_token: config.auth_token,
            state: WebPtyState::Connecting,
            last_size,
            max_rx_bytes,
            max_pending_tx_bytes,
            rx_buf: VecDeque::new(),
            pending_tx: VecDeque::new(),
            pending_tx_bytes: 0,
            outgoing: VecDeque::new(),
            pending_error: None,
            exit_code: None,
            repaint: Arc::new(Mutex::new(repaint.map(Arc::from))),
        })
    }

    /// Replaces or clears the [`RepaintCallback`].
    pub fn set_repaint_callback(&self, repaint: Option<RepaintCallback>) {
        let mut guard = match self.repaint.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = repaint.map(Arc::from);
    }

    /// Returns the configured endpoint URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns the active wire framing protocol.
    #[must_use]
    pub const fn protocol(&self) -> WebPtyProtocol {
        self.protocol
    }

    /// Returns the current connection lifecycle state.
    #[must_use]
    pub const fn state(&self) -> WebPtyState {
        self.state
    }

    /// Returns the latest clamped grid and pixel dimensions.
    #[must_use]
    pub const fn current_size(&self) -> TerminalSize {
        self.last_size
    }

    /// Non-blocking check for a remote child process exit code reported via the wire protocol.
    ///
    /// # Errors
    ///
    /// Never returns an error in the current implementation; returns `Result` for parity with
    /// desktop `PtyBackend::try_wait`.
    pub fn try_wait(&mut self) -> Result<Option<u32>, TerminalIoError> {
        Ok(self.exit_code)
    }

    /// Transitions the session from [`WebPtyState::Connecting`] to [`WebPtyState::Open`],
    /// returning the initial handshake and any queued pre-open outgoing frames in send order.
    pub fn mark_open(&mut self) -> Vec<WireMessage> {
        let (transitioned, frames) = self.mark_open_without_notify();
        if transitioned {
            self.invoke_repaint();
        }
        frames
    }

    /// Marks the session as [`WebPtyState::Closing`] (when still connecting or open).
    pub fn mark_closing(&mut self) {
        if matches!(self.state, WebPtyState::Connecting | WebPtyState::Open) {
            self.state = WebPtyState::Closing;
            self.pending_tx.clear();
            self.pending_tx_bytes = 0;
            self.outgoing.clear();
        }
    }

    /// Feeds an incoming transport [`WireMessage`] from the remote endpoint into the session.
    pub fn push_incoming(&mut self, msg: WireMessage) {
        if self.push_incoming_without_notify(msg) {
            self.invoke_repaint();
        }
    }

    /// Records a non-fatal transport error to be surfaced on the next [`TerminalInput::poll_read`].
    pub fn push_error(&mut self, msg: impl Into<String>) {
        if self.push_error_without_notify(msg.into()) {
            self.invoke_repaint();
        }
    }

    /// Transitions the session to [`WebPtyState::Closed`] after the underlying transport closes.
    ///
    /// Abnormal closes (`!was_clean` and close code outside normal `1000`/`1001`) record a
    /// descriptive error message so the UI can inform the user why the connection dropped.
    pub fn mark_closed(&mut self, code: u16, reason: &str, was_clean: bool) {
        if self.mark_closed_without_notify(code, reason, was_clean) {
            self.invoke_repaint();
        }
    }

    /// Drains all encoded outgoing [`WireMessage`] frames queued since the previous drain.
    pub fn drain_outgoing(&mut self) -> Vec<WireMessage> {
        self.outgoing.drain(..).collect()
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) const fn max_rx_bytes(&self) -> usize {
        self.max_rx_bytes
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) const fn set_last_size(&mut self, size: TerminalSize) {
        self.last_size = size;
    }

    pub(crate) fn repaint_clone(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let guard = match self.repaint.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.clone()
    }

    fn invoke_repaint(&self) {
        if let Some(cb) = self.repaint_clone() {
            cb();
        }
    }

    pub(crate) fn mark_open_without_notify(&mut self) -> (bool, Vec<WireMessage>) {
        if self.state != WebPtyState::Connecting {
            return (false, Vec::new());
        }
        self.state = WebPtyState::Open;
        let mut frames = Vec::with_capacity(1 + self.pending_tx.len());
        if let Some(handshake) = self
            .protocol
            .encode_open_handshake(self.last_size, self.auth_token.as_deref())
        {
            frames.push(handshake);
        }
        while let Some(chunk) = self.pending_tx.pop_front() {
            if let Some(encoded) = self.protocol.encode_client_input(&chunk) {
                frames.push(encoded);
            }
        }
        self.pending_tx_bytes = 0;
        (true, frames)
    }

    pub(crate) fn push_incoming_without_notify(&mut self, msg: WireMessage) -> bool {
        if self.state == WebPtyState::Closed {
            return false;
        }
        match self.protocol.decode_server_message(&msg) {
            Ok(Some(ServerFrame::Output(bytes))) => {
                self.append_rx_bytes(&bytes);
                true
            }
            Ok(Some(ServerFrame::Exit(code))) => {
                self.exit_code = Some(code);
                true
            }
            Ok(Some(ServerFrame::Error(err_msg))) => {
                let text = if err_msg.is_empty() {
                    "remote Web PTY reported an error".to_owned()
                } else {
                    err_msg
                };
                self.record_error(text);
                true
            }
            Ok(Some(ServerFrame::Title(title))) => {
                let mut osc = Vec::with_capacity(title.len() + 6);
                append_sanitized_osc_title(&title, &mut osc);
                self.append_rx_atomic(&osc);
                true
            }
            Ok(Some(ServerFrame::Preferences(_))) | Ok(None) => false,
            Err(err) => {
                self.record_error(err.to_string());
                true
            }
        }
    }

    pub(crate) fn push_error_without_notify(&mut self, msg: String) -> bool {
        if self.state == WebPtyState::Closed {
            return false;
        }
        self.record_error(msg);
        true
    }

    pub(crate) fn mark_closed_without_notify(
        &mut self,
        code: u16,
        reason: &str,
        was_clean: bool,
    ) -> bool {
        if self.state == WebPtyState::Closed {
            return false;
        }
        let was_closing_locally = self.state == WebPtyState::Closing;
        self.state = WebPtyState::Closed;
        self.pending_tx.clear();
        self.pending_tx_bytes = 0;
        self.outgoing.clear();

        let is_normal = was_closing_locally || (was_clean && matches!(code, 1000 | 1001));
        if !is_normal {
            let trimmed_reason = reason.trim();
            let close_msg = if trimmed_reason.is_empty() {
                format!("WebSocket closed abnormally (code {code})")
            } else {
                format!("WebSocket closed (code {code}): {trimmed_reason}")
            };
            match &self.pending_error {
                None => self.pending_error = Some(close_msg),
                Some(existing) if existing == GENERIC_WS_ERROR_MSG => {
                    self.pending_error = Some(close_msg);
                }
                Some(_) => {}
            }
        }
        true
    }

    fn record_error(&mut self, msg: String) {
        if self.pending_error.is_none() {
            self.pending_error = Some(msg);
        }
    }

    fn append_rx_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let avail = self.max_rx_bytes.saturating_sub(self.rx_buf.len());
        let to_take = bytes.len().min(avail);
        if to_take > 0 {
            self.rx_buf.extend(&bytes[..to_take]);
        }
        if to_take < bytes.len() {
            self.record_error(
                "Web PTY receive buffer limit exceeded; dropped incoming bytes".to_owned(),
            );
        }
    }

    /// Appends a synthesized control sequence (such as `OSC 0 ; title BEL`) atomically so receive
    /// buffer backpressure never injects an unterminated escape sequence into the VT stream.
    fn append_rx_atomic(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let avail = self.max_rx_bytes.saturating_sub(self.rx_buf.len());
        if bytes.len() <= avail {
            self.rx_buf.extend(bytes);
        } else {
            self.record_error(
                "Web PTY receive buffer limit exceeded; dropped incoming control frame".to_owned(),
            );
        }
    }

    fn push_outgoing_frame(&mut self, msg: WireMessage) -> Result<(), TerminalIoError> {
        if self.outgoing.len() >= MAX_OUTGOING_FRAMES {
            return Err(TerminalIoError::message("Web PTY outgoing queue is full"));
        }
        self.outgoing.push_back(msg);
        Ok(())
    }
}

impl TerminalInput for WebPtySession {
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
        if buf.is_empty() {
            return if self.is_closed() {
                Err(TerminalIoError::Closed)
            } else {
                Ok(0)
            };
        }

        if !self.rx_buf.is_empty() {
            let (first, second) = self.rx_buf.as_slices();
            let n1 = first.len().min(buf.len());
            buf[..n1].copy_from_slice(&first[..n1]);

            let rem = buf.len() - n1;
            let n2 = if rem > 0 {
                let count = second.len().min(rem);
                buf[n1..n1 + count].copy_from_slice(&second[..count]);
                count
            } else {
                0
            };

            let total = n1 + n2;
            self.rx_buf.drain(..total);
            return Ok(total);
        }

        if let Some(err_msg) = self.pending_error.take() {
            return Err(TerminalIoError::Message(err_msg));
        }

        if self.state == WebPtyState::Closed {
            Err(TerminalIoError::Closed)
        } else {
            Ok(0)
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
        match self.state {
            WebPtyState::Closing | WebPtyState::Closed => Err(TerminalIoError::Closed),
            WebPtyState::Connecting => {
                if bytes.is_empty() {
                    return Ok(());
                }
                let next_total = self.pending_tx_bytes.saturating_add(bytes.len());
                if next_total > self.max_pending_tx_bytes {
                    return Err(TerminalIoError::message(
                        "Web PTY pre-open send buffer is full",
                    ));
                }
                self.pending_tx.push_back(bytes.to_vec());
                self.pending_tx_bytes = next_total;
                Ok(())
            }
            WebPtyState::Open => {
                if let Some(msg) = self.protocol.encode_client_input(bytes) {
                    self.push_outgoing_frame(msg)?;
                }
                Ok(())
            }
        }
    }

    fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalIoError> {
        match self.state {
            WebPtyState::Closing | WebPtyState::Closed => Err(TerminalIoError::Closed),
            WebPtyState::Connecting => {
                self.last_size = clamp_pty_size(size);
                Ok(())
            }
            WebPtyState::Open => {
                let clamped = clamp_pty_size(size);
                if clamped == self.last_size {
                    return Ok(());
                }
                // `Ttyd` only transmits `(cols, rows)`; skip redundant frames on pixel-only changes.
                let redundant_wire_resize = self.protocol == WebPtyProtocol::Ttyd
                    && clamped.cols == self.last_size.cols
                    && clamped.rows == self.last_size.rows;
                if !redundant_wire_resize
                    && let Some(msg) = self.protocol.encode_client_resize(clamped)
                {
                    self.push_outgoing_frame(msg)?;
                }
                self.last_size = clamped;
                Ok(())
            }
        }
    }

    fn is_closed(&self) -> bool {
        self.state == WebPtyState::Closed && self.rx_buf.is_empty() && self.pending_error.is_none()
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_backend {
    use std::cell::RefCell;
    use std::fmt;
    use std::rc::Rc;

    use egui_xterm_core::{TerminalInput, TerminalIoError, TerminalSize};
    use js_sys::{Array, ArrayBuffer, Error as JsError, Uint8Array};
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::{JsCast, JsValue};
    use web_sys::{BinaryType, CloseEvent, ErrorEvent, Event, MessageEvent, WebSocket};

    use super::{GENERIC_WS_ERROR_MSG, WebPtyConfig, WebPtySession, WebPtyState};
    use crate::RepaintCallback;
    use crate::protocol::{TAG_DATA, WebPtyProtocol, WireMessage};

    /// Browser WebSocket PTY backend for `wasm32-unknown-unknown` implementing [`TerminalInput`].
    ///
    /// Connects to a remote PTY gateway (such as a `ttyd` server or a custom Rust gateway using
    /// [`crate::protocol`]), buffers pre-open keystrokes and coalesces pre-open resizes until
    /// `onopen` fires, invokes an optional [`RepaintCallback`] on incoming data/state changes,
    /// and cleanly detaches all JS event listeners on [`Drop`].
    pub struct WebPtyBackend {
        ws: WebSocket,
        session: Rc<RefCell<WebPtySession>>,
        _onopen: Closure<dyn FnMut(Event)>,
        _onmessage: Closure<dyn FnMut(MessageEvent)>,
        _onerror: Closure<dyn FnMut(Event)>,
        _onclose: Closure<dyn FnMut(CloseEvent)>,
    }

    impl fmt::Debug for WebPtyBackend {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let mut st = f.debug_struct("WebPtyBackend");
            if let Ok(session) = self.session.try_borrow() {
                st.field("url", &session.url())
                    .field("protocol", &session.protocol())
                    .field("state", &session.state())
                    .field("last_size", &session.current_size());
            } else {
                st.field("session", &"<borrowed>");
            }
            st.field("ws_ready_state", &self.ws.ready_state())
                .finish_non_exhaustive()
        }
    }

    impl WebPtyBackend {
        /// Opens a browser WebSocket PTY connection without a repaint callback.
        ///
        /// # Errors
        ///
        /// Returns [`TerminalIoError::Message`] if `config.url` fails validation or the browser
        /// rejects creating the `WebSocket` (for example, mixed-content `ws://` from an `https://`
        /// page or an invalid subprotocol).
        pub fn connect(config: WebPtyConfig) -> Result<Self, TerminalIoError> {
            Self::connect_inner(config, None)
        }

        /// Opens a browser WebSocket PTY connection and registers a [`RepaintCallback`] invoked
        /// whenever data arrives or the connection state changes.
        ///
        /// # Errors
        ///
        /// Returns [`TerminalIoError::Message`] if `config.url` fails validation or the browser
        /// rejects creating the `WebSocket`.
        pub fn connect_with_repaint(
            config: WebPtyConfig,
            repaint: RepaintCallback,
        ) -> Result<Self, TerminalIoError> {
            Self::connect_inner(config, Some(repaint))
        }

        fn connect_inner(
            config: WebPtyConfig,
            repaint: Option<RepaintCallback>,
        ) -> Result<Self, TerminalIoError> {
            let url = config.url.clone();
            let subprotocols = config.subprotocols.clone();
            let default_subprotocol = config.protocol.default_subprotocol();

            let session_inner = match repaint {
                Some(cb) => WebPtySession::with_repaint(config, cb)?,
                None => WebPtySession::new(config)?,
            };

            let ws = open_websocket(&url, &subprotocols, default_subprotocol)?;
            ws.set_binary_type(BinaryType::Arraybuffer);

            let session = Rc::new(RefCell::new(session_inner));

            // Install `onopen`: transitions Connecting -> Open, flushes handshake & queued bytes.
            let open_session = Rc::clone(&session);
            let open_ws = ws.clone();
            let onopen = Closure::<dyn FnMut(Event)>::new(move |_ev: Event| {
                let (transitioned, frames, repaint_cb) = {
                    let mut sess = open_session.borrow_mut();
                    let (transitioned, frames) = sess.mark_open_without_notify();
                    let cb = sess.repaint_clone();
                    (transitioned, frames, cb)
                };
                for frame in frames {
                    if let Err(err) = send_wire_message(&open_ws, &frame) {
                        if !err.is_closed() {
                            open_session
                                .borrow_mut()
                                .push_error_without_notify(err.to_string());
                        }
                        break;
                    }
                }
                if transitioned && let Some(cb) = repaint_cb {
                    cb();
                }
            });
            ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));

            // Install `onmessage`: decodes bounded ArrayBuffer / Uint8Array / String frames.
            let msg_session = Rc::clone(&session);
            let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |ev: MessageEvent| {
                let (max_frame_bytes, proto) = {
                    let sess = msg_session.borrow();
                    (sess.max_rx_bytes().saturating_add(16), sess.protocol())
                };
                let repaint_cb = match extract_ws_message(&ev, max_frame_bytes) {
                    Ok(ExtractedWsMessage::Complete(wire_msg)) => {
                        let mut sess = msg_session.borrow_mut();
                        if sess.push_incoming_without_notify(wire_msg) {
                            sess.repaint_clone()
                        } else {
                            None
                        }
                    }
                    Ok(ExtractedWsMessage::Truncated(wire_msg)) => {
                        let mut sess = msg_session.borrow_mut();
                        let mut notify = false;
                        if is_truncatable_data_frame(proto, &wire_msg) {
                            notify |= sess.push_incoming_without_notify(wire_msg);
                        }
                        notify |= sess.push_error_without_notify(
                            "Web PTY receive buffer limit exceeded; dropped incoming bytes"
                                .to_owned(),
                        );
                        if notify { sess.repaint_clone() } else { None }
                    }
                    Err(err_msg) => {
                        let mut sess = msg_session.borrow_mut();
                        if sess.push_error_without_notify(err_msg) {
                            sess.repaint_clone()
                        } else {
                            None
                        }
                    }
                };
                if let Some(cb) = repaint_cb {
                    cb();
                }
            });
            ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

            // Install `onerror`: surfaces non-fatal transport diagnostics.
            let err_session = Rc::clone(&session);
            let onerror = Closure::<dyn FnMut(Event)>::new(move |ev: Event| {
                let detail = ev
                    .dyn_ref::<ErrorEvent>()
                    .map(ErrorEvent::message)
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| GENERIC_WS_ERROR_MSG.to_owned());
                let repaint_cb = {
                    let mut sess = err_session.borrow_mut();
                    if sess.push_error_without_notify(detail) {
                        sess.repaint_clone()
                    } else {
                        None
                    }
                };
                if let Some(cb) = repaint_cb {
                    cb();
                }
            });
            ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));

            // Install `onclose`: marks Closed and preserves abnormal close diagnostics.
            let close_session = Rc::clone(&session);
            let onclose = Closure::<dyn FnMut(CloseEvent)>::new(move |ev: CloseEvent| {
                let repaint_cb = {
                    let mut sess = close_session.borrow_mut();
                    if sess.mark_closed_without_notify(ev.code(), &ev.reason(), ev.was_clean()) {
                        sess.repaint_clone()
                    } else {
                        None
                    }
                };
                if let Some(cb) = repaint_cb {
                    cb();
                }
            });
            ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));

            Ok(Self {
                ws,
                session,
                _onopen: onopen,
                _onmessage: onmessage,
                _onerror: onerror,
                _onclose: onclose,
            })
        }

        /// Replaces or clears the [`RepaintCallback`].
        pub fn set_repaint_callback(&self, repaint: Option<RepaintCallback>) {
            self.session.borrow().set_repaint_callback(repaint);
        }

        /// Returns the configured WebSocket URL.
        #[must_use]
        pub fn url(&self) -> String {
            self.session.borrow().url().to_owned()
        }

        /// Returns the active wire framing protocol.
        #[must_use]
        pub fn protocol(&self) -> WebPtyProtocol {
            self.session.borrow().protocol()
        }

        /// Returns the current connection lifecycle state.
        #[must_use]
        pub fn state(&self) -> WebPtyState {
            self.session.borrow().state()
        }

        /// Returns the latest grid and pixel dimensions applied to this session.
        #[must_use]
        pub fn current_size(&self) -> TerminalSize {
            self.session.borrow().current_size()
        }

        /// Non-blocking check for whether the remote process reported an exit code.
        ///
        /// # Errors
        ///
        /// Never returns an error in the current implementation; returns `Result` for parity with
        /// desktop `PtyBackend::try_wait`.
        pub fn try_wait(&mut self) -> Result<Option<u32>, TerminalIoError> {
            self.session.borrow_mut().try_wait()
        }

        /// Initiates a clean WebSocket close (`1000` normal closure).
        pub fn close(&mut self) {
            let ready = self.ws.ready_state();
            if ready == WebSocket::CONNECTING || ready == WebSocket::OPEN {
                self.session.borrow_mut().mark_closing();
                if self.ws.close_with_code(1000).is_err() {
                    self.session
                        .borrow_mut()
                        .mark_closed_without_notify(1000, "", true);
                }
            } else if ready == WebSocket::CLOSING {
                self.session.borrow_mut().mark_closing();
            } else {
                self.session
                    .borrow_mut()
                    .mark_closed_without_notify(1000, "", true);
            }
        }

        fn flush_frames(&self, frames: Vec<WireMessage>) -> Result<(), TerminalIoError> {
            for frame in frames {
                send_wire_message(&self.ws, &frame)?;
            }
            Ok(())
        }
    }

    impl TerminalInput for WebPtyBackend {
        fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
            self.session.borrow_mut().poll_read(buf)
        }

        fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
            let ready = self.ws.ready_state();
            if ready == WebSocket::CLOSING || ready == WebSocket::CLOSED {
                return Err(TerminalIoError::Closed);
            }
            let frames = {
                let mut sess = self.session.borrow_mut();
                sess.write_bytes(bytes)?;
                sess.drain_outgoing()
            };
            self.flush_frames(frames)
        }

        fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalIoError> {
            let ready = self.ws.ready_state();
            if ready == WebSocket::CLOSING || ready == WebSocket::CLOSED {
                return Err(TerminalIoError::Closed);
            }
            let (prev_size, frames) = {
                let mut sess = self.session.borrow_mut();
                let prev = sess.current_size();
                sess.resize(size)?;
                (prev, sess.drain_outgoing())
            };
            if let Err(err) = self.flush_frames(frames) {
                if !err.is_closed() {
                    self.session.borrow_mut().set_last_size(prev_size);
                }
                return Err(err);
            }
            Ok(())
        }

        fn is_closed(&self) -> bool {
            self.session.borrow().is_closed()
        }
    }

    impl Drop for WebPtyBackend {
        fn drop(&mut self) {
            // Detach all JS event listeners before dropping the Rust `Closure` handles.
            self.ws.set_onopen(None);
            self.ws.set_onmessage(None);
            self.ws.set_onerror(None);
            self.ws.set_onclose(None);
            let ready = self.ws.ready_state();
            if (ready == WebSocket::CONNECTING || ready == WebSocket::OPEN)
                && self.ws.close_with_code(1000).is_err()
            {
                // Best-effort graceful close during teardown.
            }
        }
    }

    fn open_websocket(
        url: &str,
        subprotocols: &[String],
        default_subprotocol: Option<&str>,
    ) -> Result<WebSocket, TerminalIoError> {
        let result = match subprotocols {
            [] => match default_subprotocol {
                Some(proto) => WebSocket::new_with_str(url, proto),
                None => WebSocket::new(url),
            },
            [single] => WebSocket::new_with_str(url, single),
            many => {
                let arr = Array::new();
                for proto in many {
                    arr.push(&JsValue::from_str(proto));
                }
                WebSocket::new_with_str_sequence(url, arr.as_ref())
            }
        };
        result.map_err(|err| {
            TerminalIoError::message(format!(
                "failed to open WebSocket ({url}): {}",
                js_error_to_string(err)
            ))
        })
    }

    fn send_wire_message(ws: &WebSocket, msg: &WireMessage) -> Result<(), TerminalIoError> {
        let ready = ws.ready_state();
        if ready == WebSocket::CLOSING || ready == WebSocket::CLOSED {
            return Err(TerminalIoError::Closed);
        }
        let send_res = match msg {
            WireMessage::Binary(bytes) => ws.send_with_u8_array(bytes),
            WireMessage::Text(text) => ws.send_with_str(text),
        };
        send_res.map_err(|err| {
            TerminalIoError::message(format!(
                "failed to send WebSocket PTY frame: {}",
                js_error_to_string(err)
            ))
        })
    }

    enum ExtractedWsMessage {
        Complete(WireMessage),
        Truncated(WireMessage),
    }

    fn is_truncatable_data_frame(protocol: WebPtyProtocol, msg: &WireMessage) -> bool {
        match protocol {
            WebPtyProtocol::TaggedBinary => {
                matches!(msg, WireMessage::Binary(b) if b.first() == Some(&TAG_DATA))
            }
            WebPtyProtocol::Ttyd => msg.as_bytes().first() == Some(&b'0'),
            WebPtyProtocol::JsonResize | WebPtyProtocol::Raw => true,
        }
    }

    fn extract_ws_message(
        ev: &MessageEvent,
        max_frame_bytes: usize,
    ) -> Result<ExtractedWsMessage, String> {
        let data = ev.data();
        if let Some(text) = data.as_string() {
            if text.len() <= max_frame_bytes {
                return Ok(ExtractedWsMessage::Complete(WireMessage::Text(text)));
            }
            let mut end = max_frame_bytes;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            return Ok(ExtractedWsMessage::Truncated(WireMessage::Text(
                text[..end].to_owned(),
            )));
        }
        let max_u32 = u32::try_from(max_frame_bytes).unwrap_or(u32::MAX);
        match data.dyn_into::<ArrayBuffer>() {
            Ok(ab) => {
                let full_len = ab.byte_length();
                if full_len <= max_u32 {
                    Ok(ExtractedWsMessage::Complete(WireMessage::Binary(
                        Uint8Array::new(&ab).to_vec(),
                    )))
                } else {
                    let view = Uint8Array::new_with_byte_offset_and_length(&ab, 0, max_u32);
                    Ok(ExtractedWsMessage::Truncated(WireMessage::Binary(
                        view.to_vec(),
                    )))
                }
            }
            Err(other) => match other.dyn_into::<Uint8Array>() {
                Ok(arr) => {
                    let full_len = arr.length();
                    if full_len <= max_u32 {
                        Ok(ExtractedWsMessage::Complete(WireMessage::Binary(
                            arr.to_vec(),
                        )))
                    } else {
                        Ok(ExtractedWsMessage::Truncated(WireMessage::Binary(
                            arr.subarray(0, max_u32).to_vec(),
                        )))
                    }
                }
                Err(_) => Err(
                    "received unsupported WebSocket message payload type (expected ArrayBuffer or text)"
                        .to_owned(),
                ),
            },
        }
    }

    fn js_error_to_string(err: JsValue) -> String {
        if let Some(s) = err.as_string() {
            return s;
        }
        if let Some(js_err) = err.dyn_ref::<JsError>() {
            let msg: String = js_err.message().into();
            if !msg.is_empty() {
                return msg;
            }
            return js_err.to_string().into();
        }
        format!("{err:?}")
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm_backend::WebPtyBackend;

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::protocol::OwnedClientFrame;

    #[test]
    fn session_rejects_invalid_url() {
        let err = WebPtySession::new(WebPtyConfig::new("http://localhost:8080")).unwrap_err();
        assert!(!err.is_closed());
        assert!(err.to_string().contains("ws://"));
    }

    #[test]
    fn session_queues_pre_open_writes_and_coalesces_resizes() {
        let repaints = Arc::new(AtomicUsize::new(0));
        let r_clone = Arc::clone(&repaints);
        let cfg = WebPtyConfig::new("wss://example.com/pty")
            .with_protocol(WebPtyProtocol::TaggedBinary)
            .with_initial_size(TerminalSize::new(80, 24));
        let mut session = WebPtySession::with_repaint(
            cfg,
            Box::new(move || {
                r_clone.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .unwrap();

        assert_eq!(session.state(), WebPtyState::Connecting);
        assert!(!session.is_closed());

        // Pre-open writes and resizes must succeed without emitting outgoing frames yet.
        session.resize(TerminalSize::new(100, 30)).unwrap();
        session
            .resize(TerminalSize::with_pixels(120, 40, 960, 640))
            .unwrap();
        session.write_bytes(b"echo ").unwrap();
        session.write_bytes(b"hi\r").unwrap();
        assert!(session.drain_outgoing().is_empty());
        assert_eq!(
            session.current_size(),
            TerminalSize::with_pixels(120, 40, 960, 640)
        );

        // Transitioning to Open emits one coalesced initial resize + both queued writes in order.
        let open_frames = session.mark_open();
        assert_eq!(session.state(), WebPtyState::Open);
        assert_eq!(repaints.load(Ordering::Relaxed), 1);
        assert_eq!(open_frames.len(), 3);
        assert_eq!(
            WebPtyProtocol::TaggedBinary
                .decode_client_message(&open_frames[0])
                .unwrap(),
            Some(OwnedClientFrame::Resize(TerminalSize::with_pixels(
                120, 40, 960, 640
            )))
        );
        assert_eq!(
            WebPtyProtocol::TaggedBinary
                .decode_client_message(&open_frames[1])
                .unwrap(),
            Some(OwnedClientFrame::Input(b"echo ".to_vec()))
        );
        assert_eq!(
            WebPtyProtocol::TaggedBinary
                .decode_client_message(&open_frames[2])
                .unwrap(),
            Some(OwnedClientFrame::Input(b"hi\r".to_vec()))
        );

        // Redundant resize with same dimensions is a no-op.
        session
            .resize(TerminalSize::with_pixels(120, 40, 960, 640))
            .unwrap();
        assert!(session.drain_outgoing().is_empty());

        // Post-open writes and resizes emit immediately into outgoing.
        session.write_bytes(b"ls\r").unwrap();
        session.resize(TerminalSize::new(132, 43)).unwrap();
        let out = session.drain_outgoing();
        assert_eq!(out.len(), 2);
        assert_eq!(
            WebPtyProtocol::TaggedBinary
                .decode_client_message(&out[0])
                .unwrap(),
            Some(OwnedClientFrame::Input(b"ls\r".to_vec()))
        );
        assert_eq!(
            WebPtyProtocol::TaggedBinary
                .decode_client_message(&out[1])
                .unwrap(),
            Some(OwnedClientFrame::Resize(TerminalSize::new(132, 43)))
        );
    }

    #[test]
    fn session_enforces_pre_open_tx_cap_and_rx_cap() {
        let cfg = WebPtyConfig::new("ws://127.0.0.1:7681/ws")
            .with_protocol(WebPtyProtocol::Raw)
            .with_max_pending_tx_bytes(8)
            .with_max_rx_buffer_bytes(4096);
        let mut session = WebPtySession::new(cfg).unwrap();

        session.write_bytes(b"123456").unwrap();
        let err = session.write_bytes(b"789").unwrap_err();
        assert!(!err.is_closed());
        assert!(err.to_string().contains("pre-open"));

        let open_frames = session.mark_open();
        assert_eq!(open_frames, vec![WireMessage::Binary(b"123456".to_vec())]);

        // Overflowing rx cap keeps the first 4096 bytes and surfaces a non-fatal error afterward.
        let huge = vec![b'A'; 5000];
        session.push_incoming(WireMessage::Binary(huge));
        let mut read_buf = vec![0u8; 4096];
        assert_eq!(session.poll_read(&mut read_buf).unwrap(), 4096);
        assert!(read_buf.iter().all(|&b| b == b'A'));

        let overflow_err = session.poll_read(&mut read_buf).unwrap_err();
        assert!(!overflow_err.is_closed());
        assert!(overflow_err.to_string().contains("receive buffer"));
    }

    #[test]
    fn session_drains_buffered_data_before_error_and_eof_and_records_exit_code() {
        let mut session = WebPtySession::new(
            WebPtyConfig::new("wss://example.com/pty").with_protocol(WebPtyProtocol::TaggedBinary),
        )
        .unwrap();
        let _ = session.mark_open();

        let output_msg = WebPtyProtocol::TaggedBinary
            .encode_server_frame(&ServerFrame::Output(b"final output".to_vec()))
            .unwrap();
        let exit_msg = WebPtyProtocol::TaggedBinary
            .encode_server_frame(&ServerFrame::Exit(7))
            .unwrap();
        session.push_incoming(output_msg);
        session.push_incoming(exit_msg);
        session.mark_closed(1006, "", false);

        // Must NOT report closed while buffered bytes and error remain.
        assert!(!session.is_closed());
        assert_eq!(session.try_wait().unwrap(), Some(7));

        let mut buf = [0u8; 6];
        assert_eq!(session.poll_read(&mut buf).unwrap(), 6);
        assert_eq!(&buf, b"final ");
        assert!(!session.is_closed());

        assert_eq!(session.poll_read(&mut buf).unwrap(), 6);
        assert_eq!(&buf, b"output");
        assert!(!session.is_closed());

        // Next poll surfaces abnormal close diagnostic once.
        let err = session.poll_read(&mut buf).unwrap_err();
        assert!(
            matches!(err, TerminalIoError::Message(ref s) if s.contains("1006")),
            "expected abnormal close 1006 error, got {err:?}"
        );

        // Now drained and error consumed -> permanently closed.
        assert!(session.is_closed());
        assert_eq!(session.poll_read(&mut buf), Err(TerminalIoError::Closed));
        assert_eq!(session.poll_read(&mut []), Err(TerminalIoError::Closed));
        assert_eq!(session.write_bytes(b"x"), Err(TerminalIoError::Closed));
        assert_eq!(
            session.resize(TerminalSize::new(90, 30)),
            Err(TerminalIoError::Closed)
        );
    }

    #[test]
    fn session_ttyd_title_synthesizes_sanitized_osc0_and_ignores_prefs() {
        let mut session = WebPtySession::new(
            WebPtyConfig::new("ws://localhost:7681/ws")
                .with_protocol(WebPtyProtocol::Ttyd)
                .with_auth_token("tok"),
        )
        .unwrap();
        let open_frames = session.mark_open();
        assert_eq!(open_frames.len(), 1);
        assert_eq!(
            open_frames[0],
            WireMessage::Text(r#"{"AuthToken":"tok","columns":80,"rows":24}"#.to_owned())
        );

        session.push_incoming(WireMessage::Binary(b"1zsh\x1b]52;c;bad\x07".to_vec()));
        session.push_incoming(WireMessage::Binary(b"2{\"fontSize\":14}".to_vec()));
        session.push_incoming(WireMessage::Binary(b"0prompt$ ".to_vec()));

        let mut out = [0u8; 128];
        let n = session.poll_read(&mut out).unwrap();
        assert_eq!(&out[..n], b"\x1b]0;zsh]52;c;bad\x07prompt$ ");
    }

    #[test]
    fn session_clean_close_produces_closed_without_spurious_error() {
        let mut session = WebPtySession::new(WebPtyConfig::new("wss://example.com/pty")).unwrap();
        let _ = session.mark_open();
        session.mark_closed(1000, "normal shutdown", true);
        assert!(session.is_closed());
        let mut buf = [0u8; 16];
        assert_eq!(session.poll_read(&mut buf), Err(TerminalIoError::Closed));
    }

    #[test]
    fn session_abnormal_close_refines_generic_onerror_but_keeps_specific_error() {
        let mut s1 = WebPtySession::new(WebPtyConfig::new("wss://example.com/pty")).unwrap();
        s1.push_error(GENERIC_WS_ERROR_MSG);
        s1.mark_closed(1006, "", false);
        let mut buf = [0u8; 8];
        let err1 = s1.poll_read(&mut buf).unwrap_err();
        assert!(err1.to_string().contains("1006"));

        let mut s2 = WebPtySession::new(WebPtyConfig::new("wss://example.com/pty")).unwrap();
        let _ = s2.mark_open();
        let srv_err = WebPtyProtocol::TaggedBinary
            .encode_server_frame(&ServerFrame::Error("auth rejected".to_owned()))
            .unwrap();
        s2.push_incoming(srv_err);
        s2.mark_closed(1008, "policy violation", false);
        let err2 = s2.poll_read(&mut buf).unwrap_err();
        assert_eq!(err2.to_string(), "auth rejected");
    }

    #[test]
    fn config_debug_redacts_auth_token() {
        let cfg = WebPtyConfig::new("wss://example.com/ws").with_auth_token("super-secret-jwt");
        let dbg = format!("{cfg:?}");
        assert!(
            !dbg.contains("super-secret-jwt"),
            "Debug output must redact auth_token: {dbg}"
        );
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn ttyd_title_overflow_drops_atomically_without_unterminated_osc() {
        let cfg = WebPtyConfig::new("ws://localhost:7681/ws")
            .with_protocol(WebPtyProtocol::Ttyd)
            .with_max_rx_buffer_bytes(4096);
        let mut session = WebPtySession::new(cfg).unwrap();
        let _ = session.mark_open();

        // Fill rx_buf to leave only 3 bytes free (less than `\x1b]0;shell\x07`).
        let mut filler = vec![b'0'];
        filler.extend(std::iter::repeat_n(b'X', 4093));
        session.push_incoming(WireMessage::Binary(filler));

        // Incoming title must be dropped atomically rather than injecting `\x1b]0` without BEL.
        session.push_incoming(WireMessage::Binary(b"1shell".to_vec()));

        let mut buf = vec![0u8; 4096];
        let n = session.poll_read(&mut buf).unwrap();
        assert_eq!(n, 4093);
        assert!(buf[..n].iter().all(|&b| b == b'X'));
        assert!(
            !buf[..n].contains(&0x1B),
            "must never inject truncated ESC sequence"
        );

        let err = session.poll_read(&mut buf).unwrap_err();
        assert!(err.to_string().contains("receive buffer"));

        // Subsequent normal output arrives cleanly with no unterminated OSC dangling in stream.
        session.push_incoming(WireMessage::Binary(b"0after".to_vec()));
        let n2 = session.poll_read(&mut buf).unwrap();
        assert_eq!(&buf[..n2], b"after");
    }

    #[test]
    fn closed_state_is_terminal_and_idempotent_and_ttyd_dedups_pixel_only_resize() {
        let repaints = Arc::new(AtomicUsize::new(0));
        let r_clone = Arc::clone(&repaints);
        let cfg = WebPtyConfig::new("ws://localhost:7681/ws")
            .with_protocol(WebPtyProtocol::Ttyd)
            .with_initial_size(TerminalSize::with_pixels(80, 24, 640, 384));
        let mut session = WebPtySession::with_repaint(
            cfg,
            Box::new(move || {
                r_clone.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .unwrap();

        let _ = session.mark_open();
        assert_eq!(repaints.load(Ordering::Relaxed), 1);
        // Second mark_open is a no-op and must not fire a spurious repaint.
        assert!(session.mark_open().is_empty());
        assert_eq!(repaints.load(Ordering::Relaxed), 1);

        // Pixel-only resize under Ttyd updates current_size without emitting a duplicate frame.
        session
            .resize(TerminalSize::with_pixels(80, 24, 960, 576))
            .unwrap();
        assert!(session.drain_outgoing().is_empty());
        assert_eq!(
            session.current_size(),
            TerminalSize::with_pixels(80, 24, 960, 576)
        );

        // Local close followed by abnormal remote close code must remain clean & idempotent.
        session.mark_closing();
        session.mark_closed(1006, "", false);
        assert!(session.is_closed());
        let repaints_at_close = repaints.load(Ordering::Relaxed);

        // Duplicate mark_closed or post-close push_incoming must not add errors or resurrect stream.
        session.mark_closed(1006, "late", false);
        session.push_incoming(WireMessage::Binary(b"0ghost".to_vec()));
        session.push_error("late error");
        assert!(session.is_closed());
        assert_eq!(repaints.load(Ordering::Relaxed), repaints_at_close);
        let mut buf = [0u8; 16];
        assert_eq!(session.poll_read(&mut buf), Err(TerminalIoError::Closed));
    }
}
