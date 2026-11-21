//! DCS (`ESC P ... ST`) handlers for `XTGETTCAP` and `DECRQSS`.

use std::fmt::Write as _;

use crate::cell::{CellFlags, UnderlineStyle};
use crate::color::PackedColor;
use crate::parser::CsiParams;
use crate::term::cursor::CursorAttrs;
use crate::term::modes::{CursorShape, CursorStyle};

/// Maximum buffered DCS payload bytes to bound memory against malformed streams.
pub(crate) const MAX_DCS_BYTES: usize = 4096;

/// Active DCS sequence kind being accumulated between `hook` and `unhook`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DcsKind {
    /// `XTGETTCAP`: `DCS + q <hex-encoded cap names> ST`.
    Xtgettcap,
    /// `DECRQSS`: `DCS $ q <Pt> ST`.
    Decrqss,
}

/// Accumulated DCS parser state on [`Terminal`](super::Terminal).
#[derive(Debug, Default)]
pub(crate) struct DcsState {
    pub kind: Option<DcsKind>,
    pub payload: Vec<u8>,
    pub overflowed: bool,
}

impl DcsState {
    pub fn start(&mut self, _params: &CsiParams, intermediates: &[u8], action: u8) {
        self.payload.clear();
        self.overflowed = false;
        self.kind = match (intermediates, action) {
            (b"+", b'q') => Some(DcsKind::Xtgettcap),
            (b"$", b'q') => Some(DcsKind::Decrqss),
            _ => None,
        };
    }

    pub fn push(&mut self, byte: u8) {
        if self.kind.is_none() || self.overflowed {
            return;
        }
        if self.payload.len() >= MAX_DCS_BYTES {
            self.overflowed = true;
            self.payload.clear();
            return;
        }
        self.payload.push(byte);
    }

    pub fn take(&mut self) -> Option<(DcsKind, Vec<u8>)> {
        let kind = self.kind.take()?;
        if self.overflowed {
            self.overflowed = false;
            self.payload.clear();
            return None;
        }
        let payload = std::mem::take(&mut self.payload);
        Some((kind, payload))
    }
}

fn decode_hex(hex: &[u8]) -> Option<Vec<u8>> {
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let (pairs, _) = hex.as_chunks::<2>();
    for &[hi, lo] in pairs {
        let h = hex_nibble(hi)?;
        let l = hex_nibble(lo)?;
        out.push((h << 4) | l);
    }
    Some(out)
}

const fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn push_hex_encoded(out: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0F)]));
    }
}

fn lookup_termcap(name: &[u8]) -> Option<&'static [u8]> {
    match name {
        b"TN" | b"name" => Some(b"egui_xterm"),
        b"RGB" => Some(b"8/8/8"),
        b"Tc" | b"Su" => Some(b"1"),
        b"colors" | b"Co" => Some(b"256"),
        b"Smulx" => Some(b"\x1b[4:%p1%dm"),
        b"Setulc" => Some(b"\x1b[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m"),
        b"Ms" => Some(b"\x1b]52;%p1%s;%p2%s\x07"),
        b"Ss" => Some(b"\x1b[%p1%d q"),
        b"Se" => Some(b"\x1b[2 q"),
        _ => None,
    }
}

/// Handles `XTGETTCAP` (`DCS + q <hex-caps> ST`) and appends responses to `outgoing`.
pub(crate) fn handle_xtgettcap(payload: &[u8], outgoing: &mut Vec<u8>) {
    for hex_cap in payload.split(|&b| b == b';') {
        if hex_cap.is_empty() {
            continue;
        }
        let Ok(hex_str) = std::str::from_utf8(hex_cap) else {
            continue;
        };
        if let Some(cap_name) = decode_hex(hex_cap)
            && let Some(val) = lookup_termcap(&cap_name)
        {
            let mut reply = String::with_capacity(16 + hex_str.len() + val.len() * 2);
            reply.push_str("\x1bP1+r");
            reply.push_str(hex_str);
            reply.push('=');
            push_hex_encoded(&mut reply, val);
            reply.push_str("\x1b\\");
            outgoing.extend_from_slice(reply.as_bytes());
        } else {
            let mut reply = String::with_capacity(12 + hex_str.len());
            reply.push_str("\x1bP0+r");
            reply.push_str(hex_str);
            reply.push_str("\x1b\\");
            outgoing.extend_from_slice(reply.as_bytes());
        }
    }
}

/// Snapshot of terminal state needed to answer `DECRQSS`.
pub(crate) struct DecrqssState<'a> {
    pub attrs: &'a CursorAttrs,
    pub scroll_top: usize,
    pub scroll_bottom: usize,
    pub scroll_left: usize,
    pub scroll_right: usize,
    pub left_right_margin_mode: bool,
    pub cursor_style: CursorStyle,
}

fn format_sgr_state(attrs: &CursorAttrs) -> String {
    let mut out = String::from("0");
    let flags = attrs.flags;
    if flags.contains(CellFlags::BOLD) {
        out.push_str(";1");
    }
    if flags.contains(CellFlags::DIM) {
        out.push_str(";2");
    }
    if flags.contains(CellFlags::ITALIC) {
        out.push_str(";3");
    }
    match flags.underline_style() {
        UnderlineStyle::None => {}
        UnderlineStyle::Single => out.push_str(";4"),
        UnderlineStyle::Double => out.push_str(";4:2"),
        UnderlineStyle::Curly => out.push_str(";4:3"),
        UnderlineStyle::Dotted => out.push_str(";4:4"),
        UnderlineStyle::Dashed => out.push_str(";4:5"),
    }
    if flags.contains(CellFlags::BLINK_SLOW) {
        out.push_str(";5");
    }
    if flags.contains(CellFlags::BLINK_RAPID) {
        out.push_str(";6");
    }
    if flags.contains(CellFlags::INVERSE) {
        out.push_str(";7");
    }
    if flags.contains(CellFlags::HIDDEN) {
        out.push_str(";8");
    }
    if flags.contains(CellFlags::STRIKETHROUGH) {
        out.push_str(";9");
    }
    if flags.contains(CellFlags::OVERLINE) {
        out.push_str(";53");
    }
    match attrs.fg {
        PackedColor::Default => {}
        PackedColor::Indexed(idx) => {
            let _ = write!(out, ";38;5;{idx}");
        }
        PackedColor::Rgb(r, g, b) => {
            let _ = write!(out, ";38;2;{r};{g};{b}");
        }
    }
    match attrs.bg {
        PackedColor::Default => {}
        PackedColor::Indexed(idx) => {
            let _ = write!(out, ";48;5;{idx}");
        }
        PackedColor::Rgb(r, g, b) => {
            let _ = write!(out, ";48;2;{r};{g};{b}");
        }
    }
    if let Some(ul) = attrs.underline_color {
        match ul {
            PackedColor::Default => {}
            PackedColor::Indexed(idx) => {
                let _ = write!(out, ";58;5;{idx}");
            }
            PackedColor::Rgb(r, g, b) => {
                let _ = write!(out, ";58;2;{r};{g};{b}");
            }
        }
    }
    out
}

/// Handles `DECRQSS` (`DCS $ q <Pt> ST`) and appends the response to `outgoing`.
pub(crate) fn handle_decrqss(payload: &[u8], state: &DecrqssState<'_>, outgoing: &mut Vec<u8>) {
    match payload {
        b"m" => {
            let sgr = format_sgr_state(state.attrs);
            let mut reply = String::with_capacity(12 + sgr.len());
            let _ = write!(reply, "\x1bP1$r{sgr}m\x1b\\");
            outgoing.extend_from_slice(reply.as_bytes());
        }
        b"r" => {
            let top = state.scroll_top + 1;
            let bottom = state.scroll_bottom + 1;
            let mut reply = String::with_capacity(24);
            let _ = write!(reply, "\x1bP1$r{top};{bottom}r\x1b\\");
            outgoing.extend_from_slice(reply.as_bytes());
        }
        b"s" if state.left_right_margin_mode => {
            let left = state.scroll_left + 1;
            let right = state.scroll_right + 1;
            let mut reply = String::with_capacity(24);
            let _ = write!(reply, "\x1bP1$r{left};{right}s\x1b\\");
            outgoing.extend_from_slice(reply.as_bytes());
        }
        b" q" => {
            let code = match (state.cursor_style.shape, state.cursor_style.blink) {
                (CursorShape::Block, true) => 1,
                (CursorShape::Block, false) => 2,
                (CursorShape::Underline, true) => 3,
                (CursorShape::Underline, false) => 4,
                (CursorShape::Bar, true) => 5,
                (CursorShape::Bar, false) => 6,
            };
            let mut reply = String::with_capacity(16);
            let _ = write!(reply, "\x1bP1$r{code} q\x1b\\");
            outgoing.extend_from_slice(reply.as_bytes());
        }
        b"\"q" => {
            outgoing.extend_from_slice(b"\x1bP1$r0\"q\x1b\\");
        }
        _ => {
            outgoing.extend_from_slice(b"\x1bP0$r\x1b\\");
        }
    }
}
