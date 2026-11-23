//! Clean-room RFC 6455 WebSocket server handshake and frame codec for `egui_xterm_web_gateway`.

use std::io::{self, Read, Write};

use egui_xterm_pty::WireMessage;

/// Maximum HTTP upgrade handshake size accepted from a connecting client (`16 KiB`).
const MAX_HANDSHAKE_BYTES: usize = 16 * 1024;

/// Maximum payload length permitted for a single WebSocket message (`4 MiB`).
pub const MAX_WS_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// RFC 6455 opcode: continuation frame.
pub const OP_CONTINUATION: u8 = 0x0;
/// RFC 6455 opcode: UTF-8 text frame.
pub const OP_TEXT: u8 = 0x1;
/// RFC 6455 opcode: binary frame.
pub const OP_BINARY: u8 = 0x2;
/// RFC 6455 opcode: connection close control frame.
pub const OP_CLOSE: u8 = 0x8;
/// RFC 6455 opcode: ping control frame.
pub const OP_PING: u8 = 0x9;
/// RFC 6455 opcode: pong control frame.
pub const OP_PONG: u8 = 0xA;

const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Metadata extracted during a successful RFC 6455 HTTP WebSocket upgrade handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeInfo {
    /// Request target path from the HTTP `GET` line (for example, `"/ws"`).
    pub path: String,
    /// Matched WebSocket subprotocol echoed in `Sec-WebSocket-Protocol`, if any.
    pub negotiated_subprotocol: Option<String>,
}

/// Decoded WebSocket frame or reassembled message received from a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsIncoming {
    /// Complete text or binary application message.
    Message(WireMessage),
    /// Control `Ping` frame whose opaque payload must be echoed in a `Pong`.
    Ping(Vec<u8>),
    /// Control `Pong` frame.
    Pong(Vec<u8>),
    /// Control `Close` frame carrying an optional status code and UTF-8 close reason.
    Close {
        /// Close status code (`1005` when the peer omitted a status code).
        code: u16,
        /// Optional UTF-8 close reason string.
        reason: String,
    },
}

/// Stateful RFC 6455 client-to-server frame reader supporting fragmentation and control interleaving.
#[derive(Debug, Default)]
pub struct WsFrameReader {
    frag_opcode: Option<u8>,
    frag_buf: Vec<u8>,
}

impl WsFrameReader {
    /// Creates a new [`WsFrameReader`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            frag_opcode: None,
            frag_buf: Vec::new(),
        }
    }

    /// Reads and unmasks the next WebSocket control frame or reassembled data message.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] on I/O failure, EOF, unmasked client frames, oversized payloads,
    /// malformed UTF-8 in text/close frames, or RFC 6455 framing violations.
    pub fn read_next<R: Read>(&mut self, reader: &mut R) -> io::Result<WsIncoming> {
        loop {
            let mut hdr = [0u8; 2];
            reader.read_exact(&mut hdr)?;
            let fin = (hdr[0] & 0x80) != 0;
            let rsv = hdr[0] & 0x70;
            if rsv != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unsupported WebSocket RSV bits",
                ));
            }
            let opcode = hdr[0] & 0x0F;
            let masked = (hdr[1] & 0x80) != 0;
            if !masked {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unmasked client WebSocket frame",
                ));
            }

            let len7 = hdr[1] & 0x7F;
            let payload_len = read_payload_len(reader, len7)?;

            let is_control = (opcode & 0x08) != 0;
            if is_control {
                if !fin || payload_len > 125 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid fragmented or oversized WebSocket control frame",
                    ));
                }
            } else if payload_len > MAX_WS_PAYLOAD_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WebSocket frame payload exceeds limit",
                ));
            }

            let mut mask = [0u8; 4];
            reader.read_exact(&mut mask)?;

            let mut payload = vec![0u8; payload_len];
            reader.read_exact(&mut payload)?;
            for (idx, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[idx & 3];
            }

            match opcode {
                OP_CLOSE => return decode_close_payload(&payload),
                OP_PING => return Ok(WsIncoming::Ping(payload)),
                OP_PONG => return Ok(WsIncoming::Pong(payload)),
                OP_TEXT | OP_BINARY => {
                    if self.frag_opcode.is_some() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "received new data frame while fragmented message is incomplete",
                        ));
                    }
                    if fin {
                        return Ok(WsIncoming::Message(build_wire_message(opcode, payload)?));
                    }
                    self.frag_opcode = Some(opcode);
                    self.frag_buf = payload;
                }
                OP_CONTINUATION => {
                    let Some(active_opcode) = self.frag_opcode else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unexpected WebSocket continuation frame",
                        ));
                    };
                    if self.frag_buf.len().saturating_add(payload.len()) > MAX_WS_PAYLOAD_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "fragmented WebSocket message exceeds limit",
                        ));
                    }
                    self.frag_buf.extend_from_slice(&payload);
                    if fin {
                        self.frag_opcode = None;
                        let full = std::mem::take(&mut self.frag_buf);
                        return Ok(WsIncoming::Message(build_wire_message(
                            active_opcode,
                            full,
                        )?));
                    }
                }
                other => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unsupported WebSocket opcode: 0x{other:02X}"),
                    ));
                }
            }
        }
    }
}

fn read_payload_len<R: Read>(reader: &mut R, len7: u8) -> io::Result<usize> {
    match len7 {
        0..=125 => Ok(usize::from(len7)),
        126 => {
            let mut ext = [0u8; 2];
            reader.read_exact(&mut ext)?;
            Ok(usize::from(u16::from_be_bytes(ext)))
        }
        _ => {
            let mut ext = [0u8; 8];
            reader.read_exact(&mut ext)?;
            let len64 = u64::from_be_bytes(ext);
            if (len64 >> 63) != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WebSocket 64-bit length has invalid MSB set",
                ));
            }
            usize::try_from(len64).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WebSocket frame length exceeds addressable memory",
                )
            })
        }
    }
}

fn build_wire_message(opcode: u8, payload: Vec<u8>) -> io::Result<WireMessage> {
    if opcode == OP_TEXT {
        let text = String::from_utf8(payload).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "WebSocket text frame is not valid UTF-8",
            )
        })?;
        Ok(WireMessage::Text(text))
    } else {
        Ok(WireMessage::Binary(payload))
    }
}

fn decode_close_payload(payload: &[u8]) -> io::Result<WsIncoming> {
    if payload.len() == 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid 1-byte WebSocket close payload",
        ));
    }
    if payload.is_empty() {
        return Ok(WsIncoming::Close {
            code: 1005,
            reason: String::new(),
        });
    }
    let code = u16::from_be_bytes([payload[0], payload[1]]);
    let reason = std::str::from_utf8(&payload[2..])
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "WebSocket close reason is not valid UTF-8",
            )
        })?
        .to_owned();
    Ok(WsIncoming::Close { code, reason })
}

/// Computes standard FIPS 180-4 SHA-1 over `input`.
#[must_use]
pub fn sha1(input: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];

    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(input.len() + 72);
    padded.extend_from_slice(input);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0x00);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for &chunk in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (idx, &word_bytes) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[idx] = u32::from_be_bytes(word_bytes);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (idx, word) in state.iter().enumerate() {
        out[idx * 4..idx * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Encodes `bytes` using standard RFC 4648 base64 with `=` padding.
#[must_use]
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);

        out.push(char::from(BASE64_ALPHABET[usize::from(b0 >> 2)]));
        out.push(char::from(
            BASE64_ALPHABET[usize::from(((b0 & 0x03) << 4) | (b1 >> 4))],
        ));
        if chunk.len() > 1 {
            out.push(char::from(
                BASE64_ALPHABET[usize::from(((b1 & 0x0F) << 2) | (b2 >> 6))],
            ));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char::from(BASE64_ALPHABET[usize::from(b2 & 0x3F)]));
        } else {
            out.push('=');
        }
    }
    out
}

/// Computes the RFC 6455 `Sec-WebSocket-Accept` header value for `sec_websocket_key`.
#[must_use]
pub fn compute_accept_key(sec_websocket_key: &str) -> String {
    let trimmed = sec_websocket_key.trim();
    let mut combined = Vec::with_capacity(trimmed.len() + WS_GUID.len());
    combined.extend_from_slice(trimmed.as_bytes());
    combined.extend_from_slice(WS_GUID);
    base64_encode(&sha1(&combined))
}

/// Reads an HTTP/1.1 WebSocket upgrade request and writes the `101 Switching Protocols` response.
///
/// Reads byte-by-byte until `\r\n\r\n` so trailing WebSocket frame bytes sent immediately by the
/// client remain untouched in `stream`.
///
/// # Errors
///
/// Returns [`io::Error`] if reading fails, the request is not a valid WebSocket upgrade (after
/// replying with `426 Upgrade Required`), or writing the response fails.
pub fn accept_handshake<S: Read + Write>(
    stream: &mut S,
    expected_subprotocol: Option<&str>,
) -> io::Result<HandshakeInfo> {
    let mut buf = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    loop {
        if buf.len() >= MAX_HANDSHAKE_BYTES {
            drop(write_upgrade_required(stream));
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WebSocket HTTP upgrade header exceeded 16 KiB",
            ));
        }
        let n = stream.read(&mut byte)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before HTTP WebSocket upgrade completed",
            ));
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }

    let Ok(header_text) = std::str::from_utf8(&buf) else {
        drop(write_upgrade_required(stream));
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP upgrade request is not valid UTF-8",
        ));
    };

    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut req_parts = request_line.split_whitespace();
    let method = req_parts.next().unwrap_or("");
    let path = req_parts.next().unwrap_or("");
    let version = req_parts.next().unwrap_or("");
    let valid_request_line =
        method.eq_ignore_ascii_case("GET") && !path.is_empty() && version.starts_with("HTTP/1.");

    let mut has_upgrade_websocket = false;
    let mut sec_key: Option<&str> = None;
    let mut client_subprotocols: Vec<&str> = Vec::new();

    for line in lines {
        if line.is_empty() {
            break;
        }
        let Some((raw_name, raw_val)) = line.split_once(':') else {
            continue;
        };
        let name = raw_name.trim();
        let val = raw_val.trim();
        if name.eq_ignore_ascii_case("upgrade") {
            if val
                .split(',')
                .any(|tok| tok.trim().eq_ignore_ascii_case("websocket"))
            {
                has_upgrade_websocket = true;
            }
        } else if name.eq_ignore_ascii_case("sec-websocket-key") {
            if !val.is_empty() {
                sec_key = Some(val);
            }
        } else if name.eq_ignore_ascii_case("sec-websocket-protocol") {
            for proto in val.split(',') {
                let p = proto.trim();
                if !p.is_empty() {
                    client_subprotocols.push(p);
                }
            }
        }
    }

    let Some(key) = sec_key else {
        drop(write_upgrade_required(stream));
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing Sec-WebSocket-Key header",
        ));
    };
    if !valid_request_line || !has_upgrade_websocket {
        drop(write_upgrade_required(stream));
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid WebSocket upgrade request",
        ));
    }

    let negotiated_subprotocol = expected_subprotocol.and_then(|expected| {
        client_subprotocols
            .iter()
            .any(|p| p.eq_ignore_ascii_case(expected))
            .then(|| expected.to_owned())
    });

    let accept = compute_accept_key(key);
    let mut response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n"
    );
    if let Some(proto) = &negotiated_subprotocol {
        response.push_str("Sec-WebSocket-Protocol: ");
        response.push_str(proto);
        response.push_str("\r\n");
    }
    response.push_str("\r\n");
    stream.write_all(response.as_bytes())?;
    stream.flush()?;

    Ok(HandshakeInfo {
        path: path.to_owned(),
        negotiated_subprotocol,
    })
}

fn write_upgrade_required<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.write_all(
        b"HTTP/1.1 426 Upgrade Required\r\n\
          Content-Type: text/plain\r\n\
          Connection: close\r\n\r\n\
          WebSocket upgrade required.\r\n",
    )?;
    writer.flush()
}

/// Writes a single unmasked server-to-client WebSocket frame with `FIN=1`.
///
/// # Errors
///
/// Returns [`io::Error`] if writing or flushing fails.
pub fn write_ws_frame<W: Write>(writer: &mut W, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut header = [0u8; 10];
    header[0] = 0x80 | (opcode & 0x0F);
    let hdr_len = if payload.len() <= 125 {
        header[1] = u8::try_from(payload.len()).unwrap_or(125);
        2
    } else if let Ok(len16) = u16::try_from(payload.len()) {
        header[1] = 126;
        header[2..4].copy_from_slice(&len16.to_be_bytes());
        4
    } else {
        header[1] = 127;
        let len64 = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        header[2..10].copy_from_slice(&len64.to_be_bytes());
        10
    };
    writer.write_all(&header[..hdr_len])?;
    if !payload.is_empty() {
        writer.write_all(payload)?;
    }
    writer.flush()
}

/// Writes an application [`WireMessage`] as an unmasked server-to-client WebSocket frame.
///
/// # Errors
///
/// Returns [`io::Error`] if writing or flushing fails.
pub fn write_ws_message<W: Write>(writer: &mut W, msg: &WireMessage) -> io::Result<()> {
    match msg {
        WireMessage::Binary(bytes) => write_ws_frame(writer, OP_BINARY, bytes),
        WireMessage::Text(text) => write_ws_frame(writer, OP_TEXT, text.as_bytes()),
    }
}

/// Writes a `Pong` control frame echoing at most 125 bytes from `payload`.
///
/// # Errors
///
/// Returns [`io::Error`] if writing or flushing fails.
pub fn write_ws_pong<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
    let clamped = &payload[..payload.len().min(125)];
    write_ws_frame(writer, OP_PONG, clamped)
}

/// Writes a `Close` control frame carrying `code` and a UTF-8 `reason` clamped within 125 bytes.
///
/// # Errors
///
/// Returns [`io::Error`] if writing or flushing fails.
pub fn write_ws_close<W: Write>(writer: &mut W, code: u16, reason: &str) -> io::Result<()> {
    let mut end = reason.len().min(123);
    while end > 0 && !reason.is_char_boundary(end) {
        end -= 1;
    }
    let mut payload = Vec::with_capacity(2 + end);
    payload.extend_from_slice(&code.to_be_bytes());
    payload.extend_from_slice(&reason.as_bytes()[..end]);
    write_ws_frame(writer, OP_CLOSE, &payload)
}

#[cfg(test)]
pub(crate) fn write_masked_client_frame<W: Write>(
    writer: &mut W,
    fin: bool,
    opcode: u8,
    mask: [u8; 4],
    payload: &[u8],
) -> io::Result<()> {
    let mut header = [0u8; 14];
    header[0] = if fin { 0x80 } else { 0x00 } | (opcode & 0x0F);
    let hdr_len = if payload.len() <= 125 {
        header[1] = 0x80 | u8::try_from(payload.len()).unwrap_or(125);
        header[2..6].copy_from_slice(&mask);
        6
    } else if let Ok(len16) = u16::try_from(payload.len()) {
        header[1] = 0x80 | 126;
        header[2..4].copy_from_slice(&len16.to_be_bytes());
        header[4..8].copy_from_slice(&mask);
        8
    } else {
        header[1] = 0x80 | 127;
        let len64 = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        header[2..10].copy_from_slice(&len64.to_be_bytes());
        header[10..14].copy_from_slice(&mask);
        14
    };
    writer.write_all(&header[..hdr_len])?;
    if !payload.is_empty() {
        let mut masked_payload = payload.to_vec();
        for (idx, b) in masked_payload.iter_mut().enumerate() {
            *b ^= mask[idx & 3];
        }
        writer.write_all(&masked_payload)?;
    }
    writer.flush()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    struct MemStream {
        rx: Cursor<Vec<u8>>,
        tx: Vec<u8>,
    }

    impl MemStream {
        fn from_bytes(bytes: &[u8]) -> Self {
            Self {
                rx: Cursor::new(bytes.to_vec()),
                tx: Vec::new(),
            }
        }
    }

    impl Read for MemStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.rx.read(buf)
        }
    }

    impl Write for MemStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.tx.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn sha1_and_accept_key_match_rfc6455_vector() {
        assert_eq!(
            compute_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn handshake_accepts_upgrade_and_echoes_matching_subprotocol() {
        let req = b"GET /ws HTTP/1.1\r\n\
                    Host: 127.0.0.1:7681\r\n\
                    Upgrade: websocket\r\n\
                    Connection: keep-alive, Upgrade\r\n\
                    Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                    Sec-WebSocket-Protocol: custom, tty\r\n\r\nTRAILING";
        let mut stream = MemStream::from_bytes(req);
        let info = accept_handshake(&mut stream, Some("tty")).expect("handshake succeeds");
        assert_eq!(info.path, "/ws");
        assert_eq!(info.negotiated_subprotocol.as_deref(), Some("tty"));

        let resp_str = std::str::from_utf8(&stream.tx).expect("valid utf8 response");
        assert!(resp_str.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
        assert!(resp_str.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"));
        assert!(resp_str.contains("Sec-WebSocket-Protocol: tty\r\n"));

        // Verify trailing bytes after `\r\n\r\n` were NOT consumed by the handshake reader.
        let mut rest = [0u8; 8];
        stream.read_exact(&mut rest).expect("trailing bytes remain");
        assert_eq!(&rest, b"TRAILING");

        // Plain HTTP GET without WebSocket upgrade returns 426 and InvalidData error.
        let mut bad_stream = MemStream::from_bytes(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let err = accept_handshake(&mut bad_stream, None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let bad_resp = std::str::from_utf8(&bad_stream.tx).expect("utf8 426 response");
        assert!(bad_resp.starts_with("HTTP/1.1 426 Upgrade Required\r\n"));
    }

    #[test]
    fn frame_codec_roundtrips_masked_extended_fragmented_and_control_frames() {
        let mut wire = Vec::new();
        let mask = [0x12, 0x34, 0x56, 0x78];

        // 1. Small 7-bit binary frame.
        write_masked_client_frame(&mut wire, true, OP_BINARY, mask, b"\x00hello").unwrap();

        // 2. Extended 16-bit (>125 bytes) binary frame.
        let large = vec![0xAB; 300];
        write_masked_client_frame(&mut wire, true, OP_BINARY, mask, &large).unwrap();

        // 3. Fragmented UTF-8 text frame with an interleaved Ping control frame.
        write_masked_client_frame(&mut wire, false, OP_TEXT, mask, b"hel").unwrap();
        write_masked_client_frame(&mut wire, true, OP_PING, mask, b"hb").unwrap();
        write_masked_client_frame(&mut wire, true, OP_CONTINUATION, mask, b"lo!").unwrap();

        // 4. Close frame with status code and reason.
        let mut close_payload = 1000u16.to_be_bytes().to_vec();
        close_payload.extend_from_slice(b"bye");
        write_masked_client_frame(&mut wire, true, OP_CLOSE, mask, &close_payload).unwrap();

        let mut cursor = Cursor::new(wire);
        let mut reader = WsFrameReader::new();

        assert_eq!(
            reader.read_next(&mut cursor).unwrap(),
            WsIncoming::Message(WireMessage::Binary(b"\x00hello".to_vec()))
        );
        assert_eq!(
            reader.read_next(&mut cursor).unwrap(),
            WsIncoming::Message(WireMessage::Binary(large))
        );
        assert_eq!(
            reader.read_next(&mut cursor).unwrap(),
            WsIncoming::Ping(b"hb".to_vec())
        );
        assert_eq!(
            reader.read_next(&mut cursor).unwrap(),
            WsIncoming::Message(WireMessage::Text("hello!".to_owned()))
        );
        assert_eq!(
            reader.read_next(&mut cursor).unwrap(),
            WsIncoming::Close {
                code: 1000,
                reason: "bye".to_owned(),
            }
        );

        // Unmasked client frame must be rejected per RFC 6455 §5.1.
        let unmasked = vec![0x82, 0x01, 0x00];
        let mut bad_cursor = Cursor::new(unmasked);
        let err = reader.read_next(&mut bad_cursor).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
