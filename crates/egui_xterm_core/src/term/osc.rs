//! OSC (`ESC ] ... BEL/ST`) handlers for title, CWD, palette queries/overrides, and OSC 52.

use std::fmt::Write as _;

#[cfg(feature = "hyperlinks")]
use crate::cell::{HyperlinkId, HyperlinkPool};
use crate::color::ColorScheme;
use crate::parser::OscTerminator;
use crate::term::{ClipboardSelection, PaletteOverrides, TerminalEvent};

/// Mutable state passed to the OSC dispatcher.
pub(crate) struct OscContext<'a> {
    pub palette: &'a mut ColorScheme,
    pub default_palette: &'a ColorScheme,
    pub palette_overrides: &'a mut PaletteOverrides,
    pub extended_palette: &'a mut [Option<[u8; 3]>; 240],
    pub title: &'a mut String,
    pub icon_name: &'a mut String,
    pub cwd: &'a mut Option<String>,
    pub allow_osc52_read: bool,
    pub outgoing: &'a mut Vec<u8>,
    pub events: &'a mut Vec<TerminalEvent>,
    pub palette_changed: &'a mut bool,
    #[cfg(feature = "hyperlinks")]
    pub hyperlink_pool: &'a mut HyperlinkPool,
    #[cfg(feature = "hyperlinks")]
    pub active_hyperlink: &'a mut Option<HyperlinkId>,
    #[cfg(feature = "hyperlinks")]
    pub hyperlink_inserted: &'a mut bool,
}

fn terminator_bytes(terminator: OscTerminator) -> &'static [u8] {
    match terminator {
        OscTerminator::Bel => b"\x07",
        OscTerminator::St => b"\x1b\\",
    }
}

fn join_osc_tail(params: &[&[u8]], start: usize) -> Option<String> {
    if start >= params.len() {
        return Some(String::new());
    }
    let total_len: usize = params[start..].iter().map(|p| p.len() + 1).sum();
    let mut raw = Vec::with_capacity(total_len);
    for (i, part) in params[start..].iter().enumerate() {
        if i > 0 {
            raw.push(b';');
        }
        raw.extend_from_slice(part);
    }
    String::from_utf8(raw).ok()
}

fn parse_hex_component(bytes: &[u8]) -> Option<u8> {
    if bytes.is_empty() || bytes.len() > 4 {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let val = u16::from_str_radix(text, 16).ok()?;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "scaled component is strictly in 0..=255"
    )]
    let scaled = match bytes.len() {
        1 => (val * 17) as u8,
        2 => val as u8,
        3 => (val >> 4) as u8,
        4 => (val >> 8) as u8,
        _ => return None,
    };
    Some(scaled)
}

pub(crate) fn parse_x11_color(spec: &[u8]) -> Option<[u8; 3]> {
    if let Some(rest) = spec.strip_prefix(b"rgb:") {
        let mut parts = rest.split(|&b| b == b'/');
        let r = parse_hex_component(parts.next()?)?;
        let g = parse_hex_component(parts.next()?)?;
        let b = parse_hex_component(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        return Some([r, g, b]);
    }
    if let Some(hex) = spec.strip_prefix(b"#") {
        let step = match hex.len() {
            3 => 1,
            6 => 2,
            9 => 3,
            12 => 4,
            _ => return None,
        };
        let r = parse_hex_component(&hex[..step])?;
        let g = parse_hex_component(&hex[step..2 * step])?;
        let b = parse_hex_component(&hex[2 * step..3 * step])?;
        return Some([r, g, b]);
    }
    None
}

fn format_osc_color_reply(
    prefix: &str,
    rgb: [u8; 3],
    terminator: OscTerminator,
    out: &mut Vec<u8>,
) {
    let r = u16::from(rgb[0]) * 0x0101;
    let g = u16::from(rgb[1]) * 0x0101;
    let b = u16::from(rgb[2]) * 0x0101;
    let mut s = String::with_capacity(32);
    let _ = write!(s, "\x1b]{prefix};rgb:{r:04x}/{g:04x}/{b:04x}");
    out.extend_from_slice(s.as_bytes());
    out.extend_from_slice(terminator_bytes(terminator));
}

pub(crate) fn effective_palette_color(
    palette: &ColorScheme,
    extended: &[Option<[u8; 3]>; 240],
    idx: u8,
) -> [u8; 3] {
    if idx < 16 {
        palette.ansi[idx as usize]
    } else if let Some(override_rgb) = extended[(idx - 16) as usize] {
        override_rgb
    } else {
        ColorScheme::xterm_256_color(idx, &palette.ansi)
    }
}

fn parse_osc7_cwd(uri: &[u8]) -> Option<String> {
    if uri.is_empty() {
        return Some(String::new());
    }
    let rest = if uri.len() >= 7 && uri[..7].eq_ignore_ascii_case(b"file://") {
        let after_scheme = &uri[7..];
        let slash_pos = after_scheme.iter().position(|&b| b == b'/')?;
        &after_scheme[slash_pos..]
    } else {
        uri
    };

    let mut decoded = Vec::with_capacity(rest.len());
    let mut i = 0usize;
    while i < rest.len() {
        if rest[i] == b'%'
            && i + 2 < rest.len()
            && let Ok(hex) = std::str::from_utf8(&rest[i + 1..i + 3])
            && let Ok(val) = u8::from_str_radix(hex, 16)
        {
            decoded.push(val);
            i += 3;
            continue;
        }
        decoded.push(rest[i]);
        i += 1;
    }
    String::from_utf8(decoded).ok()
}

fn decode_base64_char(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

pub(crate) fn decode_base64(input: &[u8]) -> Option<Vec<u8>> {
    let mut clean = Vec::with_capacity(input.len());
    for &b in input {
        if !b.is_ascii_whitespace() {
            clean.push(b);
        }
    }
    if clean.is_empty() {
        return Some(Vec::new());
    }
    if clean.len() % 4 != 0 {
        return None;
    }

    let mut out = Vec::with_capacity((clean.len() / 4) * 3);
    let (chunks, _) = clean.as_chunks::<4>();
    let total_chunks = chunks.len();
    for (idx, chunk) in chunks.iter().enumerate() {
        let is_last = idx + 1 == total_chunks;
        let a = decode_base64_char(chunk[0])?;
        let b = decode_base64_char(chunk[1])?;
        let pad_c = chunk[2] == b'=';
        let pad_d = chunk[3] == b'=';

        if pad_c && !pad_d {
            return None;
        }
        if (pad_c || pad_d) && !is_last {
            return None;
        }

        let c = if pad_c {
            0
        } else {
            decode_base64_char(chunk[2])?
        };
        let d = if pad_d {
            0
        } else {
            decode_base64_char(chunk[3])?
        };

        let triple =
            (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(c) << 6) | u32::from(d);

        out.push(((triple >> 16) & 0xFF) as u8);
        if !pad_c {
            out.push(((triple >> 8) & 0xFF) as u8);
        }
        if !pad_d {
            out.push((triple & 0xFF) as u8);
        }
    }
    Some(out)
}

fn parse_clipboard_selection(pc: &[u8]) -> ClipboardSelection {
    if pc.is_empty() || pc.contains(&b'c') {
        ClipboardSelection::Clipboard
    } else if pc.contains(&b'p') {
        ClipboardSelection::Primary
    } else if pc.contains(&b's') {
        ClipboardSelection::Secondary
    } else {
        ClipboardSelection::Select
    }
}

pub(crate) fn handle_osc(params: &[&[u8]], terminator: OscTerminator, ctx: &mut OscContext<'_>) {
    let Some(&cmd_bytes) = params.first() else {
        return;
    };
    let Ok(cmd_str) = std::str::from_utf8(cmd_bytes) else {
        return;
    };
    let Ok(cmd) = cmd_str.parse::<u16>() else {
        return;
    };

    match cmd {
        0 => {
            if let Some(text) = join_osc_tail(params, 1) {
                if *ctx.icon_name != text {
                    ctx.icon_name.clone_from(&text);
                    ctx.events
                        .push(TerminalEvent::IconNameChanged(text.clone()));
                }
                if *ctx.title != text {
                    ctx.title.clone_from(&text);
                    ctx.events.push(TerminalEvent::TitleChanged(text));
                }
            }
        }
        1 => {
            if let Some(text) = join_osc_tail(params, 1)
                && *ctx.icon_name != text
            {
                ctx.icon_name.clone_from(&text);
                ctx.events.push(TerminalEvent::IconNameChanged(text));
            }
        }
        2 => {
            if let Some(text) = join_osc_tail(params, 1)
                && *ctx.title != text
            {
                ctx.title.clone_from(&text);
                ctx.events.push(TerminalEvent::TitleChanged(text));
            }
        }
        4 => {
            let mut i = 1usize;
            while i + 1 < params.len() {
                if let Ok(idx_str) = std::str::from_utf8(params[i])
                    && let Ok(idx) = idx_str.parse::<u8>()
                {
                    let spec = params[i + 1];
                    if spec == b"?" {
                        let rgb = effective_palette_color(ctx.palette, ctx.extended_palette, idx);
                        format_osc_color_reply(&format!("4;{idx}"), rgb, terminator, ctx.outgoing);
                    } else if let Some(rgb) = parse_x11_color(spec) {
                        if idx < 16 {
                            ctx.palette.ansi[idx as usize] = rgb;
                            ctx.palette_overrides.ansi[idx as usize] = true;
                        } else {
                            ctx.extended_palette[(idx - 16) as usize] = Some(rgb);
                        }
                        *ctx.palette_changed = true;
                    }
                }
                i += 2;
            }
        }
        7 => {
            if let Some(uri_raw) = join_osc_tail(params, 1)
                && let Some(decoded) = parse_osc7_cwd(uri_raw.as_bytes())
            {
                if decoded.is_empty() {
                    *ctx.cwd = None;
                } else {
                    *ctx.cwd = Some(decoded.clone());
                    ctx.events.push(TerminalEvent::CwdChanged(decoded));
                }
            }
        }
        #[cfg(feature = "hyperlinks")]
        8 if params.len() >= 3 => {
            if let Some(uri) = join_osc_tail(params, 2) {
                if uri.is_empty() {
                    *ctx.active_hyperlink = None;
                } else if let Some(hid) = crate::features::hyperlinks::parse_osc8_hyperlink(
                    params[1],
                    &uri,
                    ctx.hyperlink_pool,
                ) {
                    *ctx.active_hyperlink = Some(hid);
                    *ctx.hyperlink_inserted = true;
                } else {
                    *ctx.active_hyperlink = None;
                }
            } else {
                *ctx.active_hyperlink = None;
            }
        }
        10..=12 => {
            for (offset, &spec) in params.iter().skip(1).enumerate() {
                let target = cmd as usize + offset;
                if target > 12 {
                    break;
                }
                if spec == b"?" {
                    let rgb = match target {
                        10 => ctx.palette.foreground,
                        11 => ctx.palette.background,
                        12 => ctx.palette.cursor,
                        _ => continue,
                    };
                    format_osc_color_reply(&target.to_string(), rgb, terminator, ctx.outgoing);
                } else if let Some(rgb) = parse_x11_color(spec) {
                    match target {
                        10 => {
                            ctx.palette.foreground = rgb;
                            ctx.palette_overrides.foreground = true;
                        }
                        11 => {
                            ctx.palette.background = rgb;
                            ctx.palette_overrides.background = true;
                        }
                        12 => {
                            ctx.palette.cursor = rgb;
                            ctx.palette_overrides.cursor = true;
                        }
                        _ => continue,
                    }
                    *ctx.palette_changed = true;
                }
            }
        }
        104 => {
            if params.len() <= 1 || (params.len() == 2 && params[1].is_empty()) {
                ctx.palette.ansi = ctx.default_palette.ansi;
                ctx.palette_overrides.ansi = [false; 16];
                ctx.extended_palette.fill(None);
                *ctx.palette_changed = true;
            } else {
                for &idx_bytes in &params[1..] {
                    if let Ok(idx_str) = std::str::from_utf8(idx_bytes)
                        && let Ok(idx) = idx_str.parse::<u8>()
                    {
                        if idx < 16 {
                            ctx.palette.ansi[idx as usize] = ctx.default_palette.ansi[idx as usize];
                            ctx.palette_overrides.ansi[idx as usize] = false;
                        } else {
                            ctx.extended_palette[(idx - 16) as usize] = None;
                        }
                        *ctx.palette_changed = true;
                    }
                }
            }
        }
        110 => {
            ctx.palette.foreground = ctx.default_palette.foreground;
            ctx.palette_overrides.foreground = false;
            *ctx.palette_changed = true;
        }
        111 => {
            ctx.palette.background = ctx.default_palette.background;
            ctx.palette_overrides.background = false;
            *ctx.palette_changed = true;
        }
        112 => {
            ctx.palette.cursor = ctx.default_palette.cursor;
            ctx.palette_overrides.cursor = false;
            *ctx.palette_changed = true;
        }
        52 if params.len() >= 3 => {
            let selection = parse_clipboard_selection(params[1]);
            let payload = params[2];
            if payload == b"?" {
                if ctx.allow_osc52_read {
                    ctx.events
                        .push(TerminalEvent::ClipboardReadRequested { selection });
                }
            } else if payload.is_empty() {
                ctx.events
                    .push(TerminalEvent::ClipboardWrite(String::new()));
            } else if let Some(decoded) = decode_base64(payload)
                && let Ok(text) = String::from_utf8(decoded)
            {
                ctx.events.push(TerminalEvent::ClipboardWrite(text));
            }
        }
        _ => {}
    }
}
