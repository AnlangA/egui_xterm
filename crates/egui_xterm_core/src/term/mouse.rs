//! Mouse event encoding (`?9`, `?1000`, `?1002`, `?1003`, `?1005`, `?1006`, `?1007`),
//! bracketed paste formatting, and focus reporting helpers.

use crate::term::modes::{Modes, MouseEncoding, MouseProtocol};

/// Mouse button or wheel direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MouseButton {
    /// Primary (left) mouse button.
    Left,
    /// Middle mouse button.
    Middle,
    /// Secondary (right) mouse button.
    Right,
    /// Vertical scroll wheel up.
    WheelUp,
    /// Vertical scroll wheel down.
    WheelDown,
    /// Horizontal scroll wheel left.
    WheelLeft,
    /// Horizontal scroll wheel right.
    WheelRight,
}

impl MouseButton {
    const fn is_wheel(self) -> bool {
        matches!(
            self,
            Self::WheelUp | Self::WheelDown | Self::WheelLeft | Self::WheelRight
        )
    }

    const fn protocol_code(self) -> u32 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::WheelUp => 64,
            Self::WheelDown => 65,
            Self::WheelLeft => 66,
            Self::WheelRight => 67,
        }
    }
}

/// Mouse interaction phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MouseEventKind {
    /// Button press or wheel tick.
    Press,
    /// Button release.
    Release,
    /// Pointer movement (with or without a held button).
    Motion,
}

/// Modifier keys held during a mouse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MouseModifiers {
    /// Shift key held.
    pub shift: bool,
    /// Alt / Meta key held.
    pub alt: bool,
    /// Control key held.
    pub ctrl: bool,
}

/// Grid-relative mouse input event to encode into VT protocol bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MouseInputEvent {
    /// Press, release, or motion.
    pub kind: MouseEventKind,
    /// Involved button or wheel direction (`None` for unpressed hover motion in `?1003`).
    pub button: Option<MouseButton>,
    /// Zero-based grid column.
    pub col: u16,
    /// Zero-based grid row.
    pub row: u16,
    /// Active keyboard modifiers.
    pub modifiers: MouseModifiers,
}

fn push_u32_dec(out: &mut Vec<u8>, mut val: u32) {
    let mut buf = [0u8; 10];
    let mut len = 0usize;
    loop {
        buf[len] = b'0' + (val % 10) as u8;
        len += 1;
        val /= 10;
        if val == 0 {
            break;
        }
    }
    for &b in buf[..len].iter().rev() {
        out.push(b);
    }
}

/// Encodes `ev` into VT mouse reporting bytes according to `modes`, or `None` if suppressed.
#[must_use]
pub fn encode_mouse_event(modes: &Modes, ev: MouseInputEvent) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(20);
    if encode_mouse_event_into(modes, ev, &mut out) {
        Some(out)
    } else {
        None
    }
}

/// Encodes `ev` into VT mouse reporting bytes according to `modes`, using `(pixel_x, pixel_y)`
/// when [`MouseEncoding::SgrPixels`] (`DEC ? 1016`) is active.
#[must_use]
pub fn encode_mouse_event_pixels(
    modes: &Modes,
    ev: MouseInputEvent,
    pixel_x: u32,
    pixel_y: u32,
) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(24);
    if encode_mouse_event_pixels_into(modes, ev, pixel_x, pixel_y, &mut out) {
        Some(out)
    } else {
        None
    }
}

/// Appends VT mouse reporting bytes for `ev` into `out` without intermediate allocations,
/// returning `true` if bytes were written or `false` if the event was suppressed.
pub fn encode_mouse_event_into(modes: &Modes, ev: MouseInputEvent, out: &mut Vec<u8>) -> bool {
    encode_mouse_event_with_cell_px_into(modes, ev, None, (8, 16), out)
}

/// Appends VT mouse reporting bytes for `ev` into `out` using `(pixel_x, pixel_y)` when
/// [`MouseEncoding::SgrPixels`] (`DEC ? 1016`) is active, returning `true` if written.
pub fn encode_mouse_event_pixels_into(
    modes: &Modes,
    ev: MouseInputEvent,
    pixel_x: u32,
    pixel_y: u32,
    out: &mut Vec<u8>,
) -> bool {
    encode_mouse_event_with_cell_px_into(modes, ev, Some((pixel_x, pixel_y)), (8, 16), out)
}

pub(crate) fn encode_mouse_event_with_cell_px_into(
    modes: &Modes,
    ev: MouseInputEvent,
    pixels: Option<(u32, u32)>,
    cell_px: (u16, u16),
    out: &mut Vec<u8>,
) -> bool {
    match modes.mouse_protocol {
        MouseProtocol::None => {
            if modes.alt_screen && modes.alt_scroll && ev.kind == MouseEventKind::Press {
                match ev.button {
                    Some(MouseButton::WheelUp) => {
                        out.extend_from_slice(if modes.app_cursor_keys {
                            b"\x1bOA"
                        } else {
                            b"\x1b[A"
                        });
                        return true;
                    }
                    Some(MouseButton::WheelDown) => {
                        out.extend_from_slice(if modes.app_cursor_keys {
                            b"\x1bOB"
                        } else {
                            b"\x1b[B"
                        });
                        return true;
                    }
                    _ => return false,
                }
            }
            return false;
        }
        MouseProtocol::X10 => {
            if ev.kind != MouseEventKind::Press || ev.button.is_none_or(MouseButton::is_wheel) {
                return false;
            }
        }
        MouseProtocol::Normal => {
            if ev.kind == MouseEventKind::Motion {
                return false;
            }
            if ev.kind == MouseEventKind::Release && ev.button.is_some_and(MouseButton::is_wheel) {
                return false;
            }
        }
        MouseProtocol::ButtonEvent => {
            if ev.kind == MouseEventKind::Motion && ev.button.is_none() {
                return false;
            }
            if ev.kind == MouseEventKind::Release && ev.button.is_some_and(MouseButton::is_wheel) {
                return false;
            }
        }
        MouseProtocol::AnyEvent => {
            if ev.kind == MouseEventKind::Release && ev.button.is_some_and(MouseButton::is_wheel) {
                return false;
            }
        }
    }

    let is_x10 = modes.mouse_protocol == MouseProtocol::X10;
    let is_sgr = matches!(
        modes.mouse_encoding,
        MouseEncoding::Sgr | MouseEncoding::SgrPixels
    );
    let mut cb = if ev.kind == MouseEventKind::Release && !is_sgr {
        3
    } else {
        ev.button.map_or(3, MouseButton::protocol_code)
    };

    if ev.kind == MouseEventKind::Motion {
        cb += 32;
    }
    if !is_x10 {
        if ev.modifiers.shift {
            cb += 4;
        }
        if ev.modifiers.alt {
            cb += 8;
        }
        if ev.modifiers.ctrl {
            cb += 16;
        }
    }

    let cx = u32::from(ev.col) + 1;
    let cy = u32::from(ev.row) + 1;

    match modes.mouse_encoding {
        MouseEncoding::Sgr => {
            let final_ch = if ev.kind == MouseEventKind::Release {
                b'm'
            } else {
                b'M'
            };
            out.extend_from_slice(b"\x1b[<");
            push_u32_dec(out, cb);
            out.push(b';');
            push_u32_dec(out, cx);
            out.push(b';');
            push_u32_dec(out, cy);
            out.push(final_ch);
        }
        MouseEncoding::SgrPixels => {
            let (px, py) = pixels.unwrap_or_else(|| {
                let cw = u32::from(cell_px.0.max(1));
                let ch = u32::from(cell_px.1.max(1));
                (
                    u32::from(ev.col) * cw + cw / 2,
                    u32::from(ev.row) * ch + ch / 2,
                )
            });
            let final_ch = if ev.kind == MouseEventKind::Release {
                b'm'
            } else {
                b'M'
            };
            out.extend_from_slice(b"\x1b[<");
            push_u32_dec(out, cb);
            out.push(b';');
            push_u32_dec(out, px);
            out.push(b';');
            push_u32_dec(out, py);
            out.push(final_ch);
        }
        MouseEncoding::Utf8 => {
            out.extend_from_slice(b"\x1b[M");
            let mut buf = [0u8; 4];
            for val in [cb + 32, cx.min(2015) + 32, cy.min(2015) + 32] {
                if let Some(ch) = char::from_u32(val) {
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        MouseEncoding::Default => {
            let button_byte = u8::try_from(cb + 32).unwrap_or(255);
            let col_byte = u8::try_from(cx.min(223) + 32).unwrap_or(255);
            let row_byte = u8::try_from(cy.min(223) + 32).unwrap_or(255);
            out.extend_from_slice(&[0x1B, b'[', b'M', button_byte, col_byte, row_byte]);
        }
    }
    true
}

/// Formats pasted text for transmission to the backend, wrapping in DEC `?2004` bracketed
/// paste markers when enabled and sanitizing embedded control/escape/marker sequences.
#[must_use]
pub fn format_paste_bytes(modes: &Modes, text: &str) -> Vec<u8> {
    let extra = if modes.bracketed_paste { 12 } else { 0 };
    let mut out = Vec::with_capacity(text.len() + extra);
    format_paste_bytes_into(modes, text, &mut out);
    out
}

/// Appends bracketed-paste-formatted and sanitized `text` into `out` without extra heap allocations.
pub fn format_paste_bytes_into(modes: &Modes, text: &str, out: &mut Vec<u8>) {
    if !modes.bracketed_paste {
        out.extend_from_slice(text.as_bytes());
        return;
    }

    // Step A & B: Stream UTF-8 scalars directly into `out` after the opening marker, stripping
    // unsafe C0/DEL/C1 controls while temporarily preserving `\x1b` so nested `\x1b[200~` and
    // `\x1b[201~` sequences fold in a single pass.
    out.extend_from_slice(b"\x1b[200~");
    let body_start = out.len();
    let mut utf8_buf = [0u8; 4];
    for ch in text.chars() {
        if matches!(ch, '\t' | '\n' | '\r' | '\x1b') || !ch.is_control() {
            out.extend_from_slice(ch.encode_utf8(&mut utf8_buf).as_bytes());
            if out.len() - body_start >= 6
                && (out.ends_with(b"\x1b[200~") || out.ends_with(b"\x1b[201~"))
            {
                out.truncate(out.len() - 6);
            }
        }
    }

    // Step C: Strip any remaining ESC (0x1B) bytes in place inside `out[body_start..]`.
    let mut write_idx = body_start;
    for read_idx in body_start..out.len() {
        let b = out[read_idx];
        if b != 0x1B {
            out[write_idx] = b;
            write_idx += 1;
        }
    }
    out.truncate(write_idx);
    out.extend_from_slice(b"\x1b[201~");
}

/// Returns the VT focus report bytes (`\x1b[I` / `\x1b[O`) when DEC `?1004` is active.
#[must_use]
pub const fn focus_event_bytes(modes: &Modes, focused: bool) -> Option<&'static [u8]> {
    if !modes.focus_reporting {
        return None;
    }
    if focused {
        Some(b"\x1b[I")
    } else {
        Some(b"\x1b[O")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_raw_when_bracketed_paste_disabled() {
        let modes = Modes::default();
        assert!(!modes.bracketed_paste);
        let raw = "hello\x1b[201~\x07\nworld";
        assert_eq!(format_paste_bytes(&modes, raw), raw.as_bytes());
    }

    #[test]
    fn paste_sanitizes_nested_markers_and_controls_while_keeping_whitespace_and_utf8() {
        let modes = Modes {
            bracketed_paste: true,
            ..Modes::default()
        };

        assert_eq!(
            format_paste_bytes(&modes, "evil\x1b[201~rm -rf\x1b[200~"),
            b"\x1b[200~evilrm -rf\x1b[201~"
        );
        assert_eq!(
            format_paste_bytes(&modes, "\x1b[20\x1b[201~1~whoami\r"),
            b"\x1b[200~whoami\r\x1b[201~"
        );
        assert_eq!(
            format_paste_bytes(&modes, "\x1b[20\x1b[200~0~echo\tline1\nline2"),
            b"\x1b[200~echo\tline1\nline2\x1b[201~"
        );

        let mixed = format_paste_bytes(&modes, "safe\x1b[31m\x07\x7f\u{0085}\u{009B}你好🚀");
        assert!(mixed.starts_with(b"\x1b[200~"));
        assert!(mixed.ends_with(b"\x1b[201~"));
        let inner = &mixed[6..mixed.len() - 6];
        assert!(!inner.contains(&0x1B));
        assert!(!inner.contains(&0x07));
        assert!(!inner.contains(&0x7F));
        let inner_str = std::str::from_utf8(inner).expect("valid utf8");
        assert!(inner_str.contains("safe"));
        assert!(inner_str.contains("你好🚀"));
    }
}
