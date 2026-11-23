//! Clean-room wire framing protocols for browser and remote Web PTY sessions.
//!
//! This module is compiled on both desktop and `wasm32-unknown-unknown` targets so a
//! browser client (`WebPtyBackend`) and a server gateway (wrapping desktop `PtyBackend`)
//! can share identical encoding, decoding, and URL validation logic.

use std::fmt::Write as _;

use egui_xterm_core::{TerminalIoError, TerminalSize};

/// Safe upper bound for PTY columns and rows (matches desktop ConPTY clamping).
pub(crate) const MAX_PTY_DIM: u16 = 4096;

/// Binary frame tag for terminal stdin/stdout byte streams in [`WebPtyProtocol::TaggedBinary`].
pub const TAG_DATA: u8 = 0x00;

/// Client-to-server binary frame tag for terminal resize notifications in
/// [`WebPtyProtocol::TaggedBinary`].
pub const TAG_CLIENT_RESIZE: u8 = 0x01;

/// Server-to-client binary frame tag carrying a big-endian `u32` process exit code in
/// [`WebPtyProtocol::TaggedBinary`].
pub const TAG_SERVER_EXIT: u8 = 0x01;

/// Server-to-client binary frame tag carrying a UTF-8 diagnostic error message in
/// [`WebPtyProtocol::TaggedBinary`].
pub const TAG_SERVER_ERROR: u8 = 0x02;

/// Clamps terminal grid dimensions to `1..=4096` while preserving pixel dimensions.
#[must_use]
pub const fn clamp_pty_size(size: TerminalSize) -> TerminalSize {
    TerminalSize {
        cols: if size.cols == 0 {
            1
        } else if size.cols > MAX_PTY_DIM {
            MAX_PTY_DIM
        } else {
            size.cols
        },
        rows: if size.rows == 0 {
            1
        } else if size.rows > MAX_PTY_DIM {
            MAX_PTY_DIM
        } else {
            size.rows
        },
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    }
}

/// Wire framing protocol used between a browser Web PTY client and a remote PTY gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum WebPtyProtocol {
    /// Compact clean-room binary framing supporting raw bytes, pixel-aware resizes,
    /// remote process exit codes, and structured error frames.
    ///
    /// - Client -> Server:
    ///   - `[0x00, bytes...]` — terminal input (`stdin`)
    ///   - `[0x01, cols_be: u16, rows_be: u16, pixel_width_be: u16, pixel_height_be: u16]` — resize
    /// - Server -> Client:
    ///   - `[0x00, bytes...]` — terminal output (`stdout`/`stderr`)
    ///   - `[0x01, exit_code_be: u32]` — child process exited with `exit_code`
    ///   - `[0x02, utf8_error...]` — remote transport or spawn error
    #[default]
    TaggedBinary,

    /// Wire protocol compatible with `ttyd` WebSocket servers (default subprotocol `"tty"`).
    ///
    /// - Initial handshake on open: JSON text frame
    ///   `{"AuthToken":"...","columns":C,"rows":R}`.
    /// - Client -> Server:
    ///   - `[b'0', bytes...]` — terminal input
    ///   - `[b'1', b"{\"columns\":C,\"rows\":R}"]` — resize
    /// - Server -> Client:
    ///   - `[b'0', bytes...]` — terminal output
    ///   - `[b'1', utf8_title...]` — window title update
    ///   - `[b'2', utf8_json...]` — server preferences payload
    Ttyd,

    /// Hybrid framing used by common `xterm.js` and custom WebSocket PTY bridges.
    ///
    /// - Client -> Server:
    ///   - Binary frame: raw terminal input bytes
    ///   - Text frame: JSON `{"type":"resize","cols":C,"rows":R,"pixel_width":PW,"pixel_height":PH}`
    /// - Server -> Client:
    ///   - Binary or text frame: raw terminal output bytes
    JsonResize,

    /// Unframed raw byte stream over WebSocket (no wire-level resize or exit signaling).
    Raw,
}

/// Transport-level WebSocket message frame (binary or UTF-8 text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireMessage {
    /// Binary WebSocket message payload (`ArrayBuffer` / `Uint8Array`).
    Binary(Vec<u8>),
    /// UTF-8 text WebSocket message payload.
    Text(String),
}

impl WireMessage {
    /// Returns the raw byte slice backing either a binary or UTF-8 text message.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Binary(bytes) => bytes.as_slice(),
            Self::Text(text) => text.as_bytes(),
        }
    }

    /// Returns `true` if the payload contains zero bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.as_bytes().is_empty()
    }

    /// Returns the byte length of the payload.
    #[must_use]
    pub fn len(&self) -> usize {
        self.as_bytes().len()
    }
}

/// Borrowed client-to-server logical PTY frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClientFrame<'a> {
    /// Raw terminal input bytes (keyboard, paste, mouse, or VT query replies).
    Input(&'a [u8]),
    /// Viewport grid and pixel resize notification.
    Resize(TerminalSize),
}

/// Owned client-to-server logical PTY frame produced by server-side decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum OwnedClientFrame {
    /// Raw terminal input bytes (keyboard, paste, mouse, or VT query replies).
    Input(Vec<u8>),
    /// Viewport grid and pixel resize notification.
    Resize(TerminalSize),
}

/// Logical server-to-client PTY frame decoded from an incoming WebSocket message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServerFrame {
    /// Raw VT/ANSI terminal output bytes from the remote PTY.
    Output(Vec<u8>),
    /// Remote child process exit code.
    Exit(u32),
    /// Remote server diagnostic or spawn error message.
    Error(String),
    /// Out-of-band window title update (emitted by [`WebPtyProtocol::Ttyd`]).
    Title(String),
    /// Out-of-band server preferences JSON (emitted by [`WebPtyProtocol::Ttyd`]).
    Preferences(String),
}

impl WebPtyProtocol {
    /// Returns the default WebSocket subprotocol name for this framing mode, if any.
    #[must_use]
    pub const fn default_subprotocol(self) -> Option<&'static str> {
        match self {
            Self::Ttyd => Some("tty"),
            Self::TaggedBinary | Self::JsonResize | Self::Raw => None,
        }
    }

    /// Encodes the initial message sent immediately after the WebSocket transitions to open.
    ///
    /// For [`Self::Ttyd`], produces the JSON handshake carrying `auth_token` (or empty string)
    /// and `initial_size`. For [`Self::TaggedBinary`] and [`Self::JsonResize`], emits an
    /// initial resize frame. For [`Self::Raw`], returns `None`.
    #[must_use]
    pub fn encode_open_handshake(
        self,
        initial_size: TerminalSize,
        auth_token: Option<&str>,
    ) -> Option<WireMessage> {
        let clamped = clamp_pty_size(initial_size);
        match self {
            Self::Ttyd => {
                let token = auth_token.unwrap_or("");
                let mut json = String::with_capacity(token.len() + 48);
                json.push_str(r#"{"AuthToken":""#);
                escape_json_str_into(token, &mut json);
                if write!(
                    json,
                    r#"","columns":{},"rows":{}}}"#,
                    clamped.cols, clamped.rows
                )
                .is_err()
                {
                    // `fmt::Write` for `String` is infallible.
                }
                Some(WireMessage::Text(json))
            }
            Self::TaggedBinary | Self::JsonResize => self.encode_client_resize(clamped),
            Self::Raw => None,
        }
    }

    /// Encodes a [`ClientFrame`] into a [`WireMessage`].
    ///
    /// Returns `None` for empty input payloads or for [`Self::Raw`] resize frames.
    #[must_use]
    pub fn encode_client_frame(self, frame: ClientFrame<'_>) -> Option<WireMessage> {
        match frame {
            ClientFrame::Input(bytes) => self.encode_client_input(bytes),
            ClientFrame::Resize(size) => self.encode_client_resize(size),
        }
    }

    /// Encodes outgoing terminal input bytes into a [`WireMessage`].
    ///
    /// Returns `None` when `data` is empty.
    #[must_use]
    pub fn encode_client_input(self, data: &[u8]) -> Option<WireMessage> {
        if data.is_empty() {
            return None;
        }
        match self {
            Self::TaggedBinary => {
                let mut payload = Vec::with_capacity(1 + data.len());
                payload.push(TAG_DATA);
                payload.extend_from_slice(data);
                Some(WireMessage::Binary(payload))
            }
            Self::Ttyd => {
                let mut payload = Vec::with_capacity(1 + data.len());
                payload.push(b'0');
                payload.extend_from_slice(data);
                Some(WireMessage::Binary(payload))
            }
            Self::JsonResize | Self::Raw => Some(WireMessage::Binary(data.to_vec())),
        }
    }

    /// Encodes a terminal resize notification into a [`WireMessage`].
    ///
    /// Dimensions are clamped to `1..=4096`. Returns `None` for [`Self::Raw`].
    #[must_use]
    pub fn encode_client_resize(self, size: TerminalSize) -> Option<WireMessage> {
        let clamped = clamp_pty_size(size);
        match self {
            Self::TaggedBinary => {
                let mut payload = Vec::with_capacity(9);
                payload.push(TAG_CLIENT_RESIZE);
                payload.extend_from_slice(&clamped.cols.to_be_bytes());
                payload.extend_from_slice(&clamped.rows.to_be_bytes());
                payload.extend_from_slice(&clamped.pixel_width.to_be_bytes());
                payload.extend_from_slice(&clamped.pixel_height.to_be_bytes());
                Some(WireMessage::Binary(payload))
            }
            Self::Ttyd => {
                let json = format!(r#"{{"columns":{},"rows":{}}}"#, clamped.cols, clamped.rows);
                let mut payload = Vec::with_capacity(1 + json.len());
                payload.push(b'1');
                payload.extend_from_slice(json.as_bytes());
                Some(WireMessage::Binary(payload))
            }
            Self::JsonResize => {
                let json = format!(
                    r#"{{"type":"resize","cols":{},"rows":{},"pixel_width":{},"pixel_height":{}}}"#,
                    clamped.cols, clamped.rows, clamped.pixel_width, clamped.pixel_height
                );
                Some(WireMessage::Text(json))
            }
            Self::Raw => None,
        }
    }

    /// Decodes a client-to-server [`WireMessage`] into an [`OwnedClientFrame`].
    ///
    /// Intended for server-side gateways bridging browser `WebPtyBackend` connections to a
    /// native `PtyBackend`.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalIoError::Message`] if the frame does not conform to `self`.
    pub fn decode_client_message(
        self,
        msg: &WireMessage,
    ) -> Result<Option<OwnedClientFrame>, TerminalIoError> {
        match self {
            Self::TaggedBinary => {
                let WireMessage::Binary(bytes) = msg else {
                    return Err(TerminalIoError::message(
                        "unexpected text WebSocket frame for TaggedBinary protocol",
                    ));
                };
                let Some((&tag, rest)) = bytes.split_first() else {
                    return Ok(None);
                };
                match tag {
                    TAG_DATA => {
                        if rest.is_empty() {
                            Ok(None)
                        } else {
                            Ok(Some(OwnedClientFrame::Input(rest.to_vec())))
                        }
                    }
                    TAG_CLIENT_RESIZE => {
                        if rest.len() != 8 {
                            return Err(TerminalIoError::message(format!(
                                "invalid TaggedBinary resize payload length: expected 8, got {}",
                                rest.len()
                            )));
                        }
                        let cols = u16::from_be_bytes([rest[0], rest[1]]);
                        let rows = u16::from_be_bytes([rest[2], rest[3]]);
                        let pixel_width = u16::from_be_bytes([rest[4], rest[5]]);
                        let pixel_height = u16::from_be_bytes([rest[6], rest[7]]);
                        Ok(Some(OwnedClientFrame::Resize(clamp_pty_size(
                            TerminalSize::with_pixels(cols, rows, pixel_width, pixel_height),
                        ))))
                    }
                    other => Err(TerminalIoError::message(format!(
                        "unknown TaggedBinary client frame tag: 0x{other:02X}"
                    ))),
                }
            }
            Self::Ttyd => {
                let bytes = msg.as_bytes();
                let Some((&first, rest)) = bytes.split_first() else {
                    return Ok(None);
                };
                match first {
                    b'0' => {
                        if rest.is_empty() {
                            Ok(None)
                        } else {
                            Ok(Some(OwnedClientFrame::Input(rest.to_vec())))
                        }
                    }
                    b'1' => {
                        let json = std::str::from_utf8(rest).map_err(|_| {
                            TerminalIoError::message("invalid UTF-8 in ttyd resize frame")
                        })?;
                        let size = parse_ttyd_resize_json(json)?;
                        Ok(Some(OwnedClientFrame::Resize(size)))
                    }
                    b'2' | b'3' => {
                        // `ttyd` client flow-control commands (`PAUSE` / `RESUME`).
                        Ok(None)
                    }
                    b'{' => {
                        let json = std::str::from_utf8(bytes).map_err(|_| {
                            TerminalIoError::message("invalid UTF-8 in ttyd handshake frame")
                        })?;
                        let size = parse_ttyd_resize_json(json)?;
                        Ok(Some(OwnedClientFrame::Resize(size)))
                    }
                    other => Err(TerminalIoError::message(format!(
                        "unknown ttyd client command byte: 0x{other:02X}"
                    ))),
                }
            }
            Self::JsonResize => match msg {
                WireMessage::Binary(bytes) => {
                    if bytes.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(OwnedClientFrame::Input(bytes.clone())))
                    }
                }
                WireMessage::Text(text) => {
                    if text.is_empty() {
                        return Ok(None);
                    }
                    if let Some(size) = try_parse_json_resize(text) {
                        Ok(Some(OwnedClientFrame::Resize(size)))
                    } else {
                        Ok(Some(OwnedClientFrame::Input(text.as_bytes().to_vec())))
                    }
                }
            },
            Self::Raw => {
                let bytes = msg.as_bytes();
                if bytes.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(OwnedClientFrame::Input(bytes.to_vec())))
                }
            }
        }
    }

    /// Encodes a [`ServerFrame`] into a [`WireMessage`] for transmission to a browser client.
    ///
    /// Returns `None` if `frame` is empty output or unsupported by `self` (for example,
    /// [`ServerFrame::Exit`] under [`Self::Raw`]).
    #[must_use]
    pub fn encode_server_frame(self, frame: &ServerFrame) -> Option<WireMessage> {
        match self {
            Self::TaggedBinary => match frame {
                ServerFrame::Output(bytes) => {
                    if bytes.is_empty() {
                        return None;
                    }
                    let mut payload = Vec::with_capacity(1 + bytes.len());
                    payload.push(TAG_DATA);
                    payload.extend_from_slice(bytes);
                    Some(WireMessage::Binary(payload))
                }
                ServerFrame::Exit(code) => {
                    let mut payload = Vec::with_capacity(5);
                    payload.push(TAG_SERVER_EXIT);
                    payload.extend_from_slice(&code.to_be_bytes());
                    Some(WireMessage::Binary(payload))
                }
                ServerFrame::Error(msg) => {
                    let mut payload = Vec::with_capacity(1 + msg.len());
                    payload.push(TAG_SERVER_ERROR);
                    payload.extend_from_slice(msg.as_bytes());
                    Some(WireMessage::Binary(payload))
                }
                ServerFrame::Title(title) => {
                    let mut osc = Vec::with_capacity(1 + 4 + title.len() + 1);
                    osc.push(TAG_DATA);
                    append_sanitized_osc_title(title, &mut osc);
                    Some(WireMessage::Binary(osc))
                }
                ServerFrame::Preferences(_) => None,
            },
            Self::Ttyd => match frame {
                ServerFrame::Output(bytes) => {
                    if bytes.is_empty() {
                        return None;
                    }
                    let mut payload = Vec::with_capacity(1 + bytes.len());
                    payload.push(b'0');
                    payload.extend_from_slice(bytes);
                    Some(WireMessage::Binary(payload))
                }
                ServerFrame::Title(title) => {
                    let mut payload = Vec::with_capacity(1 + title.len());
                    payload.push(b'1');
                    payload.extend_from_slice(title.as_bytes());
                    Some(WireMessage::Binary(payload))
                }
                ServerFrame::Preferences(prefs) => {
                    let mut payload = Vec::with_capacity(1 + prefs.len());
                    payload.push(b'2');
                    payload.extend_from_slice(prefs.as_bytes());
                    Some(WireMessage::Binary(payload))
                }
                ServerFrame::Exit(_) | ServerFrame::Error(_) => None,
            },
            Self::JsonResize | Self::Raw => match frame {
                ServerFrame::Output(bytes) => {
                    if bytes.is_empty() {
                        None
                    } else {
                        Some(WireMessage::Binary(bytes.clone()))
                    }
                }
                ServerFrame::Title(title) => {
                    let mut osc = Vec::with_capacity(4 + title.len() + 1);
                    append_sanitized_osc_title(title, &mut osc);
                    Some(WireMessage::Binary(osc))
                }
                ServerFrame::Exit(_) | ServerFrame::Error(_) | ServerFrame::Preferences(_) => None,
            },
        }
    }

    /// Decodes a server-to-client [`WireMessage`] into a [`ServerFrame`].
    ///
    /// # Errors
    ///
    /// Returns [`TerminalIoError::Message`] if the incoming message violates the framing
    /// rules of `self`.
    pub fn decode_server_message(
        self,
        msg: &WireMessage,
    ) -> Result<Option<ServerFrame>, TerminalIoError> {
        match self {
            Self::TaggedBinary => {
                let WireMessage::Binary(bytes) = msg else {
                    return Err(TerminalIoError::message(
                        "unexpected text WebSocket frame for TaggedBinary protocol",
                    ));
                };
                let Some((&tag, rest)) = bytes.split_first() else {
                    return Ok(None);
                };
                match tag {
                    TAG_DATA => {
                        if rest.is_empty() {
                            Ok(None)
                        } else {
                            Ok(Some(ServerFrame::Output(rest.to_vec())))
                        }
                    }
                    TAG_SERVER_EXIT => {
                        if rest.len() != 4 {
                            return Err(TerminalIoError::message(format!(
                                "invalid TaggedBinary exit payload length: expected 4, got {}",
                                rest.len()
                            )));
                        }
                        let code = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
                        Ok(Some(ServerFrame::Exit(code)))
                    }
                    TAG_SERVER_ERROR => {
                        let text = String::from_utf8_lossy(rest).into_owned();
                        Ok(Some(ServerFrame::Error(text)))
                    }
                    other => Err(TerminalIoError::message(format!(
                        "unknown TaggedBinary server frame tag: 0x{other:02X}"
                    ))),
                }
            }
            Self::Ttyd => {
                let bytes = msg.as_bytes();
                let Some((&cmd, rest)) = bytes.split_first() else {
                    return Ok(None);
                };
                match cmd {
                    b'0' => {
                        if rest.is_empty() {
                            Ok(None)
                        } else {
                            Ok(Some(ServerFrame::Output(rest.to_vec())))
                        }
                    }
                    b'1' => Ok(Some(ServerFrame::Title(
                        String::from_utf8_lossy(rest).into_owned(),
                    ))),
                    b'2' => Ok(Some(ServerFrame::Preferences(
                        String::from_utf8_lossy(rest).into_owned(),
                    ))),
                    other => Err(TerminalIoError::message(format!(
                        "unknown ttyd server command byte: 0x{other:02X}"
                    ))),
                }
            }
            Self::JsonResize | Self::Raw => {
                let bytes = msg.as_bytes();
                if bytes.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(ServerFrame::Output(bytes.to_vec())))
                }
            }
        }
    }
}

/// Validates that `url` is a syntactically well-formed `ws://` or `wss://` endpoint URL
/// conforming to RFC 6455 §3 (no fragment identifiers, whitespace, or control characters).
///
/// # Errors
///
/// Returns [`TerminalIoError::Message`] if `url` is empty, uses an unsupported scheme,
/// contains whitespace, control characters, or a `#fragment`, or lacks a valid host authority.
///
/// # Examples
///
/// ```
/// use egui_xterm_pty::validate_ws_url;
///
/// assert!(validate_ws_url("wss://terminal.example.com/pty").is_ok());
/// assert!(validate_ws_url("ws://127.0.0.1:7681/ws").is_ok());
/// assert!(validate_ws_url("https://example.com").is_err());
/// assert!(validate_ws_url("wss://example.com/pty#frag").is_err());
/// assert!(validate_ws_url("ws://").is_err());
/// ```
pub fn validate_ws_url(url: &str) -> Result<(), TerminalIoError> {
    if url.is_empty() {
        return Err(TerminalIoError::message(
            "Web PTY WebSocket URL must not be empty",
        ));
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(TerminalIoError::message(
            "Web PTY WebSocket URL must not contain whitespace or control characters",
        ));
    }
    if url.contains('#') {
        return Err(TerminalIoError::message(
            "Web PTY WebSocket URL must not contain a fragment identifier (#)",
        ));
    }

    let rest = if url.len() >= 5 && url[..5].eq_ignore_ascii_case("ws://") {
        &url[5..]
    } else if url.len() >= 6 && url[..6].eq_ignore_ascii_case("wss://") {
        &url[6..]
    } else {
        return Err(TerminalIoError::message(
            "Web PTY WebSocket URL must start with ws:// or wss://",
        ));
    };

    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() {
        return Err(TerminalIoError::message(
            "Web PTY WebSocket URL must include a host",
        ));
    }

    // Strip optional userinfo (`user@host`), rejecting ambiguous multiple `@` delimiters.
    let host_port = match authority.split_once('@') {
        Some((_, after_at)) => {
            if after_at.contains('@') {
                return Err(TerminalIoError::message(
                    "Web PTY WebSocket URL authority contains multiple '@' delimiters",
                ));
            }
            after_at
        }
        None => authority,
    };
    if host_port.is_empty() {
        return Err(TerminalIoError::message(
            "Web PTY WebSocket URL must include a host",
        ));
    }

    if let Some(bracketed) = host_port.strip_prefix('[') {
        let Some((ipv6_host, after_bracket)) = bracketed.split_once(']') else {
            return Err(TerminalIoError::message(
                "Web PTY WebSocket URL has an unclosed IPv6 host bracket",
            ));
        };
        if ipv6_host.is_empty() || ipv6_host.contains('[') {
            return Err(TerminalIoError::message(
                "Web PTY WebSocket URL has an invalid IPv6 host",
            ));
        }
        if !after_bracket.is_empty() {
            validate_port_suffix(after_bracket)?;
        }
    } else {
        if host_port.contains(['[', ']']) {
            return Err(TerminalIoError::message(
                "Web PTY WebSocket URL host contains unexpected brackets",
            ));
        }
        if let Some((host, port)) = host_port.split_once(':') {
            if host.is_empty() {
                return Err(TerminalIoError::message(
                    "Web PTY WebSocket URL must include a host before the port",
                ));
            }
            validate_port_digits(port)?;
        }
    }

    Ok(())
}

fn validate_port_suffix(suffix: &str) -> Result<(), TerminalIoError> {
    let Some(port) = suffix.strip_prefix(':') else {
        return Err(TerminalIoError::message(
            "Web PTY WebSocket URL has invalid characters after IPv6 host",
        ));
    };
    validate_port_digits(port)
}

fn validate_port_digits(port: &str) -> Result<(), TerminalIoError> {
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(TerminalIoError::message(
            "Web PTY WebSocket URL port must be numeric",
        ));
    }
    if port.parse::<u16>().is_err() {
        return Err(TerminalIoError::message(
            "Web PTY WebSocket URL port is out of range (0..=65535)",
        ));
    }
    Ok(())
}

/// Appends an `OSC 0 ; <sanitized_title> BEL` sequence while stripping C0/C1 control characters
/// so untrusted out-of-band titles cannot inject arbitrary escape sequences.
pub(crate) fn append_sanitized_osc_title(title: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b]0;");
    for ch in title.chars() {
        let code = ch as u32;
        if code >= 0x20 && code != 0x7F && !(0x80..=0x9F).contains(&code) {
            let mut buf = [0u8; 4];
            let encoded = ch.encode_utf8(&mut buf);
            out.extend_from_slice(encoded.as_bytes());
        }
    }
    out.push(0x07);
}

/// Escapes a string per RFC 8259 JSON string rules into `out`.
fn escape_json_str_into(input: &str, out: &mut String) {
    for ch in input.chars() {
        match ch {
            '"' => out.push_str(r#"\""#),
            '\\' => out.push_str(r"\\"),
            '\u{08}' => out.push_str(r"\b"),
            '\u{0C}' => out.push_str(r"\f"),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            c if (c as u32) < 0x20 => {
                if write!(out, r"\u{:04x}", c as u32).is_err() {
                    // `fmt::Write` for `String` is infallible.
                }
            }
            c => out.push(c),
        }
    }
}

#[derive(Default)]
struct JsonResizeFields {
    kind: Option<String>,
    cols: Option<u16>,
    columns: Option<u16>,
    rows: Option<u16>,
    pixel_width: Option<u16>,
    pixel_height: Option<u16>,
}

fn parse_ttyd_resize_json(json: &str) -> Result<TerminalSize, TerminalIoError> {
    let fields = parse_top_level_resize_object(json)
        .ok_or_else(|| TerminalIoError::message("invalid ttyd JSON resize payload"))?;
    let cols = fields
        .columns
        .or(fields.cols)
        .ok_or_else(|| TerminalIoError::message("ttyd resize JSON missing columns field"))?;
    let rows = fields
        .rows
        .ok_or_else(|| TerminalIoError::message("ttyd resize JSON missing rows field"))?;
    Ok(clamp_pty_size(TerminalSize::new(cols, rows)))
}

fn try_parse_json_resize(json: &str) -> Option<TerminalSize> {
    let fields = parse_top_level_resize_object(json)?;
    if let Some(kind) = &fields.kind
        && kind != "resize"
    {
        return None;
    }
    let cols = fields.cols.or(fields.columns)?;
    let rows = fields.rows?;
    let pixel_width = fields.pixel_width.unwrap_or(0);
    let pixel_height = fields.pixel_height.unwrap_or(0);
    Some(clamp_pty_size(TerminalSize::with_pixels(
        cols,
        rows,
        pixel_width,
        pixel_height,
    )))
}

/// Clean-room, iterative (non-recursive) RFC 8259 JSON object parser for resize payloads.
fn parse_top_level_resize_object(input: &str) -> Option<JsonResizeFields> {
    let bytes = input.as_bytes();
    let mut pos = skip_json_ws(bytes, 0);
    if bytes.get(pos) != Some(&b'{') {
        return None;
    }
    pos += 1;
    pos = skip_json_ws(bytes, pos);

    let mut fields = JsonResizeFields::default();
    if bytes.get(pos) == Some(&b'}') {
        pos = skip_json_ws(bytes, pos + 1);
        return (pos == bytes.len()).then_some(fields);
    }

    loop {
        let (key, next_pos) = parse_json_string(bytes, pos)?;
        pos = skip_json_ws(bytes, next_pos);
        if bytes.get(pos) != Some(&b':') {
            return None;
        }
        pos = skip_json_ws(bytes, pos + 1);

        match key.as_str() {
            "type" => {
                let (val, next) = parse_json_string(bytes, pos)?;
                fields.kind = Some(val);
                pos = next;
            }
            "cols" => {
                let (val, next) = parse_json_u16(bytes, pos)?;
                fields.cols = Some(val);
                pos = next;
            }
            "columns" => {
                let (val, next) = parse_json_u16(bytes, pos)?;
                fields.columns = Some(val);
                pos = next;
            }
            "rows" => {
                let (val, next) = parse_json_u16(bytes, pos)?;
                fields.rows = Some(val);
                pos = next;
            }
            "pixel_width" => {
                let (val, next) = parse_json_optional_u16(bytes, pos)?;
                fields.pixel_width = val;
                pos = next;
            }
            "pixel_height" => {
                let (val, next) = parse_json_optional_u16(bytes, pos)?;
                fields.pixel_height = val;
                pos = next;
            }
            _ => {
                pos = skip_json_value(bytes, pos)?;
            }
        }

        pos = skip_json_ws(bytes, pos);
        match bytes.get(pos) {
            Some(&b',') => {
                pos = skip_json_ws(bytes, pos + 1);
            }
            Some(&b'}') => {
                pos = skip_json_ws(bytes, pos + 1);
                return (pos == bytes.len()).then_some(fields);
            }
            _ => return None,
        }
    }
}

fn skip_json_ws(bytes: &[u8], mut pos: usize) -> usize {
    while let Some(&b) = bytes.get(pos) {
        if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
            pos += 1;
        } else {
            break;
        }
    }
    pos
}

fn parse_json_string(bytes: &[u8], pos: usize) -> Option<(String, usize)> {
    if bytes.get(pos) != Some(&b'"') {
        return None;
    }
    let mut i = pos + 1;
    let mut out = String::new();
    while let Some(&b) = bytes.get(i) {
        match b {
            b'"' => return Some((out, i + 1)),
            b'\\' => {
                i += 1;
                let esc = *bytes.get(i)?;
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{08}'),
                    b'f' => out.push('\u{0C}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let code = parse_hex4(bytes, i + 1)?;
                        i += 4;
                        if (0xD800..=0xDBFF).contains(&code) {
                            // Combine valid UTF-16 surrogate pair (`\uD800..\uDBFF\uDC00..\uDFFF`).
                            if bytes.get(i + 1) == Some(&b'\\')
                                && bytes.get(i + 2) == Some(&b'u')
                                && let Some(low) = parse_hex4(bytes, i + 3)
                                && (0xDC00..=0xDFFF).contains(&low)
                            {
                                let combined = 0x10000
                                    + ((u32::from(code - 0xD800) << 10) | u32::from(low - 0xDC00));
                                out.push(
                                    char::from_u32(combined).unwrap_or(char::REPLACEMENT_CHARACTER),
                                );
                                i += 6;
                            } else {
                                out.push(char::REPLACEMENT_CHARACTER);
                            }
                        } else if let Some(ch) = char::from_u32(u32::from(code)) {
                            out.push(ch);
                        } else {
                            out.push(char::REPLACEMENT_CHARACTER);
                        }
                    }
                    _ => return None,
                }
                i += 1;
            }
            0x00..=0x1F => return None,
            _ => {
                // Consume valid UTF-8 chunk up to next quote, backslash, or control byte.
                let start = i;
                while let Some(&ch_b) = bytes.get(i) {
                    if ch_b == b'"' || ch_b == b'\\' || ch_b < 0x20 {
                        break;
                    }
                    i += 1;
                }
                let slice = std::str::from_utf8(bytes.get(start..i)?).ok()?;
                out.push_str(slice);
            }
        }
    }
    None
}

fn parse_hex4(bytes: &[u8], pos: usize) -> Option<u16> {
    let hex_slice = bytes.get(pos..pos + 4)?;
    let hex_str = std::str::from_utf8(hex_slice).ok()?;
    u16::from_str_radix(hex_str, 16).ok()
}

fn parse_json_optional_u16(bytes: &[u8], pos: usize) -> Option<(Option<u16>, usize)> {
    if bytes.get(pos..pos + 4) == Some(b"null") {
        return Some((None, pos + 4));
    }
    let (val, next) = parse_json_u16(bytes, pos)?;
    Some((Some(val), next))
}

fn parse_json_u16(bytes: &[u8], pos: usize) -> Option<(u16, usize)> {
    let start = pos;
    let mut i = pos;
    while let Some(&b) = bytes.get(i) {
        if b.is_ascii_digit() {
            i += 1;
        } else {
            break;
        }
    }
    if i == start {
        return None;
    }
    let digits = &bytes[start..i];
    if digits.len() > 1 && digits[0] == b'0' {
        return None;
    }
    let mut val = 0u16;
    for &d in digits {
        val = val.saturating_mul(10).saturating_add(u16::from(d - b'0'));
    }
    Some((val, i))
}

/// Skips any JSON value iteratively without recursion so hostile nesting cannot overflow the stack.
fn skip_json_value(bytes: &[u8], pos: usize) -> Option<usize> {
    let first = *bytes.get(pos)?;
    match first {
        b'"' => {
            let (_, next) = parse_json_string(bytes, pos)?;
            Some(next)
        }
        b'{' | b'[' => {
            let mut expected_closers: Vec<u8> = Vec::with_capacity(8);
            let mut i = pos;
            while let Some(&b) = bytes.get(i) {
                match b {
                    b'"' => {
                        let (_, next) = parse_json_string(bytes, i)?;
                        i = next;
                        continue;
                    }
                    b'{' => expected_closers.push(b'}'),
                    b'[' => expected_closers.push(b']'),
                    b'}' | b']' => {
                        if expected_closers.pop() != Some(b) {
                            return None;
                        }
                        if expected_closers.is_empty() {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        b't' => (bytes.get(pos..pos + 4) == Some(b"true")).then_some(pos + 4),
        b'f' => (bytes.get(pos..pos + 5) == Some(b"false")).then_some(pos + 5),
        b'n' => (bytes.get(pos..pos + 4) == Some(b"null")).then_some(pos + 4),
        b'-' | b'0'..=b'9' => {
            let mut i = pos;
            if bytes.get(i) == Some(&b'-') {
                i += 1;
            }
            let int_start = i;
            if bytes.get(i) == Some(&b'0') {
                i += 1;
                if bytes.get(i).is_some_and(u8::is_ascii_digit) {
                    return None;
                }
            } else {
                while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
            }
            if i == int_start {
                return None;
            }
            if bytes.get(i) == Some(&b'.') {
                i += 1;
                let frac_start = i;
                while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                if i == frac_start {
                    return None;
                }
            }
            if matches!(bytes.get(i), Some(b'e' | b'E')) {
                i += 1;
                if matches!(bytes.get(i), Some(b'+' | b'-')) {
                    i += 1;
                }
                let exp_start = i;
                while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                if i == exp_start {
                    return None;
                }
            }
            Some(i)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_ws_url_accepts_valid_and_rejects_invalid() {
        for valid in [
            "ws://localhost:7681/ws",
            "wss://terminal.example.com/pty?session=42",
            "WS://127.0.0.1:8080",
            "WSS://[::1]:8443/ws",
            "wss://user@example.com/pty",
        ] {
            assert!(
                validate_ws_url(valid).is_ok(),
                "expected valid URL: {valid}"
            );
        }

        for invalid in [
            "",
            "http://localhost:7681/ws",
            "https://example.com/ws",
            "javascript:alert(1)",
            "wss://terminal.example.com/pty?session=42#frag",
            "ws://localhost:7681/ws#section",
            "ws://",
            "wss:///path",
            "ws://:8080",
            "ws://[::1/ws",
            "ws://[]/ws",
            "ws://[[::1]]/ws",
            "ws://[::1]foo",
            "ws://foo]bar/ws",
            "ws://foo[bar/ws",
            "ws://a@b@example.com/ws",
            "ws://localhost:99999/ws",
            "ws://localhost:abc/ws",
            "ws://localhost:7681/ws with space",
            "ws://localhost:7681/ws\u{00A0}nbsp",
            "ws://localhost:7681/ws\n",
            "ws://user@/pty",
        ] {
            assert!(
                validate_ws_url(invalid).is_err(),
                "expected invalid URL to fail: {invalid:?}"
            );
        }
    }

    #[test]
    fn tagged_binary_roundtrip_client_and_server() {
        let proto = WebPtyProtocol::TaggedBinary;
        assert_eq!(proto.default_subprotocol(), None);
        assert_eq!(proto.encode_client_input(b""), None);

        let input_msg = proto.encode_client_input(b"ls -la\r").unwrap();
        assert_eq!(
            proto.decode_client_message(&input_msg).unwrap(),
            Some(OwnedClientFrame::Input(b"ls -la\r".to_vec()))
        );

        let size = TerminalSize::with_pixels(120, 40, 960, 640);
        let resize_msg = proto.encode_client_resize(size).unwrap();
        assert_eq!(resize_msg.len(), 9);
        assert_eq!(
            proto.decode_client_message(&resize_msg).unwrap(),
            Some(OwnedClientFrame::Resize(size))
        );

        let out_msg = proto
            .encode_server_frame(&ServerFrame::Output(b"\x1b[32mok\x1b[0m".to_vec()))
            .unwrap();
        assert_eq!(
            proto.decode_server_message(&out_msg).unwrap(),
            Some(ServerFrame::Output(b"\x1b[32mok\x1b[0m".to_vec()))
        );

        let exit_msg = proto.encode_server_frame(&ServerFrame::Exit(42)).unwrap();
        assert_eq!(
            proto.decode_server_message(&exit_msg).unwrap(),
            Some(ServerFrame::Exit(42))
        );

        let err_msg = proto
            .encode_server_frame(&ServerFrame::Error("spawn failed".to_owned()))
            .unwrap();
        assert_eq!(
            proto.decode_server_message(&err_msg).unwrap(),
            Some(ServerFrame::Error("spawn failed".to_owned()))
        );

        // Malformed frames must return clean errors, never panic.
        assert!(
            proto
                .decode_server_message(&WireMessage::Text("bad".to_owned()))
                .is_err()
        );
        assert!(
            proto
                .decode_server_message(&WireMessage::Binary(vec![TAG_SERVER_EXIT, 0, 1]))
                .is_err()
        );
        assert!(
            proto
                .decode_server_message(&WireMessage::Binary(vec![0xFF]))
                .is_err()
        );
        assert!(
            proto
                .decode_client_message(&WireMessage::Binary(vec![TAG_CLIENT_RESIZE, 1, 2]))
                .is_err()
        );
        assert_eq!(
            proto
                .decode_server_message(&WireMessage::Binary(Vec::new()))
                .unwrap(),
            None
        );
    }

    #[test]
    fn ttyd_handshake_escaping_and_frame_roundtrip() {
        let proto = WebPtyProtocol::Ttyd;
        assert_eq!(proto.default_subprotocol(), Some("tty"));

        let handshake = proto
            .encode_open_handshake(TerminalSize::new(100, 35), Some("tok\"en\\line\n\x01"))
            .unwrap();
        let WireMessage::Text(ref json) = handshake else {
            panic!("expected text handshake");
        };
        assert!(json.contains(r#"\"en\\line\n\u0001"#));
        assert_eq!(
            proto.decode_client_message(&handshake).unwrap(),
            Some(OwnedClientFrame::Resize(TerminalSize::new(100, 35)))
        );

        let input_msg = proto.encode_client_input(b"echo hi\r").unwrap();
        assert_eq!(
            proto.decode_client_message(&input_msg).unwrap(),
            Some(OwnedClientFrame::Input(b"echo hi\r".to_vec()))
        );

        let resize_msg = proto
            .encode_client_resize(TerminalSize::new(132, 43))
            .unwrap();
        assert_eq!(
            proto.decode_client_message(&resize_msg).unwrap(),
            Some(OwnedClientFrame::Resize(TerminalSize::new(132, 43)))
        );

        let out_msg = proto
            .encode_server_frame(&ServerFrame::Output(b"hello".to_vec()))
            .unwrap();
        assert_eq!(
            proto.decode_server_message(&out_msg).unwrap(),
            Some(ServerFrame::Output(b"hello".to_vec()))
        );

        let title_msg = proto
            .encode_server_frame(&ServerFrame::Title("bash".to_owned()))
            .unwrap();
        assert_eq!(
            proto.decode_server_message(&title_msg).unwrap(),
            Some(ServerFrame::Title("bash".to_owned()))
        );

        let prefs_msg = proto
            .encode_server_frame(&ServerFrame::Preferences(r#"{"fontSize":14}"#.to_owned()))
            .unwrap();
        assert_eq!(
            proto.decode_server_message(&prefs_msg).unwrap(),
            Some(ServerFrame::Preferences(r#"{"fontSize":14}"#.to_owned()))
        );

        // `ttyd` client PAUSE ('2') and RESUME ('3') flow-control frames decode cleanly as None.
        assert_eq!(
            proto
                .decode_client_message(&WireMessage::Binary(vec![b'2']))
                .unwrap(),
            None
        );
        assert_eq!(
            proto
                .decode_client_message(&WireMessage::Binary(vec![b'3']))
                .unwrap(),
            None
        );
    }

    #[test]
    fn json_resize_and_raw_protocols_roundtrip_and_handle_edge_cases() {
        let json_proto = WebPtyProtocol::JsonResize;
        let size = TerminalSize::with_pixels(90, 28, 720, 448);
        let resize_msg = json_proto.encode_client_resize(size).unwrap();
        assert_eq!(
            json_proto.decode_client_message(&resize_msg).unwrap(),
            Some(OwnedClientFrame::Resize(size))
        );

        // Text with non-resize type falls back to Input bytes.
        let other_json = WireMessage::Text(r#"{"type":"ping","cols":80,"rows":24}"#.to_owned());
        assert!(matches!(
            json_proto.decode_client_message(&other_json).unwrap(),
            Some(OwnedClientFrame::Input(_))
        ));

        // Plain text input works too.
        let plain_text = WireMessage::Text("ls\r".to_owned());
        assert_eq!(
            json_proto.decode_client_message(&plain_text).unwrap(),
            Some(OwnedClientFrame::Input(b"ls\r".to_vec()))
        );

        // Deeply nested extra fields must not stack overflow.
        let deep = format!(
            r#"{{"extra":{}{},"cols":80,"rows":24}}"#,
            "[".repeat(2000),
            "]".repeat(2000)
        );
        assert_eq!(
            json_proto
                .decode_client_message(&WireMessage::Text(deep))
                .unwrap(),
            Some(OwnedClientFrame::Resize(TerminalSize::new(80, 24)))
        );

        // Ignored fields with escaped quotes/braces and `\uXXXX` escapes parse cleanly.
        let escaped = WireMessage::Text(
            r#"{"type":"res\u0069ze","note":"brace } \"quote\"","cols":110,"rows":32}"#.to_owned(),
        );
        assert_eq!(
            json_proto.decode_client_message(&escaped).unwrap(),
            Some(OwnedClientFrame::Resize(TerminalSize::new(110, 32)))
        );

        // Null optional pixel fields and oversized positive dimensions clamp cleanly instead of
        // leaking JSON into stdin.
        let null_px = WireMessage::Text(
            r#"{"type":"resize","cols":70000,"rows":24,"pixel_width":null,"pixel_height":null}"#
                .to_owned(),
        );
        assert_eq!(
            json_proto.decode_client_message(&null_px).unwrap(),
            Some(OwnedClientFrame::Resize(TerminalSize::new(MAX_PTY_DIM, 24)))
        );

        // Mismatched nested delimiters and leading-zero numbers must be rejected as invalid JSON
        // (and therefore not treated as valid resize objects).
        assert!(parse_top_level_resize_object(r#"{"extra":{],"cols":80,"rows":24}"#).is_none());
        assert!(parse_top_level_resize_object(r#"{"extra":[},"cols":80,"rows":24}"#).is_none());
        assert!(parse_top_level_resize_object(r#"{"extra":01,"cols":80,"rows":24}"#).is_none());
        assert!(parse_top_level_resize_object(r#"{"extra":-01,"cols":80,"rows":24}"#).is_none());

        // UTF-16 surrogate pairs decode to the full Unicode scalar value per RFC 8259.
        let (decoded_emoji, _) = parse_json_string(br#""\uD83D\uDE00""#, 0).unwrap();
        assert_eq!(decoded_emoji, "😀");

        let raw = WebPtyProtocol::Raw;
        assert_eq!(raw.encode_client_resize(size), None);
        assert_eq!(raw.encode_open_handshake(size, None), None);
        let raw_in = raw.encode_client_input(b"abc").unwrap();
        assert_eq!(
            raw.decode_client_message(&raw_in).unwrap(),
            Some(OwnedClientFrame::Input(b"abc".to_vec()))
        );
        assert_eq!(
            raw.decode_server_message(&WireMessage::Text("hi".to_owned()))
                .unwrap(),
            Some(ServerFrame::Output(b"hi".to_vec()))
        );
    }

    #[test]
    fn sanitized_osc_title_strips_escape_injection() {
        let mut out = Vec::new();
        append_sanitized_osc_title("safe\x07\x1b]52;c;evil\x07\u{009B}31m title", &mut out);
        assert_eq!(out, b"\x1b]0;safe]52;c;evil31m title\x07");
    }

    #[test]
    fn clamp_pty_size_enforces_bounds() {
        assert_eq!(
            clamp_pty_size(TerminalSize::with_pixels(0, 0, 100, 200)),
            TerminalSize::with_pixels(1, 1, 100, 200)
        );
        assert_eq!(
            clamp_pty_size(TerminalSize::new(u16::MAX, u16::MAX)),
            TerminalSize::new(MAX_PTY_DIM, MAX_PTY_DIM)
        );
    }
}
