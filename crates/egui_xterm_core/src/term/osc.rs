//! OSC (`ESC ] ... BEL/ST`) handlers for title, CWD, palette queries/overrides,
//! special colors, document URI, pointer shapes, text sizing, notifications, and OSC 52.

use std::collections::VecDeque;
use std::fmt::Write as _;

#[cfg(feature = "hyperlinks")]
use crate::cell::{HyperlinkId, HyperlinkPool};
use crate::cell::{MAX_HYPERLINK_URI_BYTES, TextSizing};
use crate::color::{ColorScheme, SpecialColors};
use crate::parser::OscTerminator;
use crate::term::{ClipboardSelection, Osc52ReadPolicy, PaletteOverrides, TerminalEvent};

const MAX_MOUSE_SHAPE_STACK: usize = 32;
const MAX_MOUSE_SHAPE_LEN: usize = 256;
const MAX_OSC66_TEXT_BYTES: usize = 4096;
const MAX_OSC99_PENDING: usize = 16;
const MAX_OSC99_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_OSC99_ID_BYTES: usize = 64;

/// Pending chunked `OSC 99` notification keyed by `i=<id>`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Osc99Pending {
    pub id: String,
    pub title: String,
    pub body: String,
    pub urgency: u8,
    pub occasion: u8,
    pub total_bytes: usize,
}

/// Mutable state passed to the OSC dispatcher.
pub(crate) struct OscContext<'a> {
    pub palette: &'a mut ColorScheme,
    pub default_palette: &'a ColorScheme,
    pub palette_overrides: &'a mut PaletteOverrides,
    pub extended_palette: &'a mut [Option<[u8; 3]>; 240],
    pub special_colors: &'a mut SpecialColors,
    pub title: &'a mut String,
    pub icon_name: &'a mut String,
    pub cwd: &'a mut Option<String>,
    pub document_uri: &'a mut Option<String>,
    pub osc52_read_policy: Osc52ReadPolicy,
    pub outgoing: &'a mut Vec<u8>,
    pub events: &'a mut Vec<TerminalEvent>,
    pub palette_changed: &'a mut bool,
    pub mouse_shape_stack: &'a mut Vec<String>,
    pub osc99_pending: &'a mut VecDeque<Osc99Pending>,
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
    if write!(s, "\x1b]{prefix};rgb:{r:04x}/{g:04x}/{b:04x}").is_err() {
        // `String`'s `fmt::Write` impl is infallible.
    }
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
        5 => {
            let mut i = 1usize;
            while i + 1 < params.len() {
                if let Ok(idx_str) = std::str::from_utf8(params[i])
                    && let Ok(idx) = idx_str.parse::<usize>()
                    && idx <= 4
                {
                    let spec = params[i + 1];
                    if spec == b"?" {
                        let fallback = if idx == 3 {
                            ctx.palette.background
                        } else {
                            ctx.palette.foreground
                        };
                        let rgb = ctx.special_colors.get(idx).unwrap_or(fallback);
                        format_osc_color_reply(&format!("5;{idx}"), rgb, terminator, ctx.outgoing);
                    } else if let Some(rgb) = parse_x11_color(spec) {
                        ctx.special_colors.set(idx, Some(rgb));
                        *ctx.palette_changed = true;
                    }
                }
                i += 2;
            }
        }
        6 => handle_osc6(params, ctx),
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
        9 => handle_osc9(params, ctx),
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
        105 => {
            if params.len() <= 1 || (params.len() == 2 && params[1].is_empty()) {
                ctx.special_colors.clear();
                *ctx.palette_changed = true;
            } else {
                for &idx_bytes in &params[1..] {
                    if let Ok(idx_str) = std::str::from_utf8(idx_bytes)
                        && let Ok(idx) = idx_str.parse::<usize>()
                        && idx <= 4
                    {
                        ctx.special_colors.set(idx, None);
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
        22 => handle_osc22(params, terminator, ctx),
        99 => handle_osc99(params, terminator, ctx),
        777 => {
            if params.len() >= 3
                && params[1] == b"notify"
                && let Ok(title) = std::str::from_utf8(params[2])
            {
                let body = join_osc_tail(params, 3).unwrap_or_default();
                ctx.events.push(TerminalEvent::Notification {
                    title: title.to_owned(),
                    body,
                });
            }
        }
        52 if params.len() >= 3 => {
            let selection = parse_clipboard_selection(params[1]);
            let payload = params[2];
            if payload == b"?" {
                match ctx.osc52_read_policy {
                    Osc52ReadPolicy::Deny => {}
                    Osc52ReadPolicy::Allow => {
                        ctx.events
                            .push(TerminalEvent::ClipboardReadRequested { selection });
                    }
                    Osc52ReadPolicy::Prompt => {
                        ctx.events
                            .push(TerminalEvent::ClipboardReadPrompt { selection });
                    }
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

fn handle_osc6(params: &[&[u8]], ctx: &mut OscContext<'_>) {
    if params.len() < 2 {
        return;
    }
    if params.len() == 2 && params[1].is_empty() {
        if ctx.document_uri.take().is_some() {
            ctx.events.push(TerminalEvent::DocumentChanged(None));
        }
        return;
    }
    // Disambiguate iTerm2 tab color (`OSC 6 ; 1 ; bg ; ...`).
    if params.len() >= 5 && params[1] == b"1" && params[2] == b"bg" {
        return;
    }
    // Disambiguate xterm special color enable/disable (`OSC 6 ; c ; f`, c in 0..=4, f in 0|1).
    if params.len() == 3
        && params[1].len() == 1
        && (b'0'..=b'4').contains(&params[1][0])
        && (params[2] == b"0" || params[2] == b"1")
    {
        return;
    }

    let Some(uri) = join_osc_tail(params, 1) else {
        return;
    };
    if uri.len() > MAX_HYPERLINK_URI_BYTES || uri.chars().any(char::is_control) {
        return;
    }
    let has_scheme = (uri.len() >= 7 && uri.as_bytes()[..7].eq_ignore_ascii_case(b"file://"))
        || uri.contains("://");
    if !has_scheme {
        return;
    }
    if ctx.document_uri.as_deref() != Some(uri.as_str()) {
        *ctx.document_uri = Some(uri.clone());
        ctx.events.push(TerminalEvent::DocumentChanged(Some(uri)));
    }
}

fn is_supported_mouse_shape(name: &str) -> bool {
    matches!(
        name,
        "default"
            | "none"
            | "context-menu"
            | "help"
            | "pointer"
            | "progress"
            | "wait"
            | "cell"
            | "crosshair"
            | "text"
            | "vertical-text"
            | "alias"
            | "copy"
            | "move"
            | "no-drop"
            | "not-allowed"
            | "grab"
            | "grabbing"
            | "e-resize"
            | "n-resize"
            | "ne-resize"
            | "nw-resize"
            | "s-resize"
            | "se-resize"
            | "sw-resize"
            | "w-resize"
            | "ew-resize"
            | "ns-resize"
            | "nesw-resize"
            | "nwse-resize"
            | "col-resize"
            | "row-resize"
            | "all-scroll"
            | "zoom-in"
            | "zoom-out"
    )
}

fn resolve_mouse_shape_candidate(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "default" {
        return String::new();
    }
    if trimmed.contains(',') {
        for cand in trimmed.split(',') {
            let c = cand.trim();
            if c == "default" {
                return String::new();
            }
            if is_supported_mouse_shape(c) {
                return c.to_owned();
            }
        }
        for cand in trimmed.split(',') {
            let c = cand.trim();
            if !c.is_empty() {
                return c.to_owned();
            }
        }
        return String::new();
    }
    trimmed.to_owned()
}

fn handle_osc22(params: &[&[u8]], terminator: OscTerminator, ctx: &mut OscContext<'_>) {
    let Some(raw) = join_osc_tail(params, 1) else {
        return;
    };
    if raw.len() > MAX_MOUSE_SHAPE_LEN || raw.chars().any(char::is_control) {
        return;
    }

    if raw == "?__current__" {
        let current = ctx.mouse_shape_stack.last().map_or("", String::as_str);
        ctx.outgoing.extend_from_slice(b"\x1b]22;");
        ctx.outgoing.extend_from_slice(current.as_bytes());
        ctx.outgoing.extend_from_slice(terminator_bytes(terminator));
        return;
    }

    if let Some(query) = raw.strip_prefix('?') {
        ctx.outgoing.extend_from_slice(b"\x1b]22;");
        for (idx, name) in query.split(',').enumerate() {
            if idx > 0 {
                ctx.outgoing.push(b',');
            }
            let supported = is_supported_mouse_shape(name.trim());
            ctx.outgoing.push(if supported { b'1' } else { b'0' });
        }
        ctx.outgoing.extend_from_slice(terminator_bytes(terminator));
        return;
    }

    let prev_active = ctx.mouse_shape_stack.last().cloned().unwrap_or_default();

    if let Some(pushed) = raw.strip_prefix('>') {
        let resolved = resolve_mouse_shape_candidate(pushed);
        if ctx.mouse_shape_stack.len() >= MAX_MOUSE_SHAPE_STACK {
            ctx.mouse_shape_stack.remove(0);
        }
        ctx.mouse_shape_stack.push(resolved.clone());
        if resolved != prev_active {
            ctx.events.push(TerminalEvent::MouseShape(resolved));
        }
        return;
    }

    if raw == "<" {
        ctx.mouse_shape_stack.pop();
        let next_active = ctx.mouse_shape_stack.last().cloned().unwrap_or_default();
        if next_active != prev_active {
            ctx.events.push(TerminalEvent::MouseShape(next_active));
        }
        return;
    }

    let stripped = raw.strip_prefix('=').unwrap_or(&raw);
    let resolved = resolve_mouse_shape_candidate(stripped);
    if resolved.is_empty() {
        if ctx.mouse_shape_stack.len() <= 1 {
            ctx.mouse_shape_stack.clear();
        } else if let Some(top) = ctx.mouse_shape_stack.last_mut() {
            top.clear();
        }
    } else if let Some(top) = ctx.mouse_shape_stack.last_mut() {
        top.clone_from(&resolved);
    } else {
        ctx.mouse_shape_stack.push(resolved.clone());
    }
    if resolved != prev_active {
        ctx.events.push(TerminalEvent::MouseShape(resolved));
    }
}

fn handle_osc9(params: &[&[u8]], ctx: &mut OscContext<'_>) {
    if params.len() >= 3 && params[1] == b"4" {
        if let Ok(st_str) = std::str::from_utf8(params[2])
            && let Ok(state) = st_str.parse::<u8>()
            && state <= 4
        {
            let progress = params
                .get(3)
                .and_then(|p| std::str::from_utf8(p).ok())
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0)
                .min(100) as u8;
            ctx.events.push(TerminalEvent::Progress { state, progress });
        }
        return;
    }
    if params.len() >= 2
        && let Some(body) = join_osc_tail(params, 1)
        && !body.is_empty()
    {
        ctx.events.push(TerminalEvent::Notification {
            title: String::new(),
            body,
        });
    }
}

fn is_valid_osc99_id(id: &[u8]) -> bool {
    id.len() <= MAX_OSC99_ID_BYTES
        && id
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Osc99Part {
    Title,
    Body,
    Close,
    Query,
    Ignore,
}

fn handle_osc99(params: &[&[u8]], terminator: OscTerminator, ctx: &mut OscContext<'_>) {
    if params.len() < 2 {
        return;
    }

    let mut id = String::new();
    let mut part = Osc99Part::Title;
    let mut base64_encoded = false;
    let mut done = true;
    let mut urgency: Option<u8> = None;
    let mut occasion: Option<u8> = None;

    for kv in params[1].split(|&b| b == b':') {
        if let Some(eq) = kv.iter().position(|&b| b == b'=') {
            let k = &kv[..eq];
            let v = &kv[eq + 1..];
            match k {
                b"i" => {
                    if !is_valid_osc99_id(v) {
                        return;
                    }
                    let Ok(s) = std::str::from_utf8(v) else {
                        return;
                    };
                    id.clear();
                    id.push_str(s);
                }
                b"p" => {
                    part = match v {
                        b"body" => Osc99Part::Body,
                        b"title" => Osc99Part::Title,
                        b"close" => Osc99Part::Close,
                        b"?" => Osc99Part::Query,
                        _ => Osc99Part::Ignore,
                    };
                }
                b"e" => base64_encoded = v == b"1",
                b"d" => done = v != b"0",
                b"u" => {
                    if let Ok(s) = std::str::from_utf8(v)
                        && let Ok(u) = s.parse::<u8>()
                        && u <= 2
                    {
                        urgency = Some(u);
                    }
                }
                b"o" => {
                    occasion = match v {
                        b"always" => Some(0),
                        b"unfocused" => Some(1),
                        b"invisible" => Some(2),
                        _ => occasion,
                    };
                }
                _ => {}
            }
        }
    }

    match part {
        Osc99Part::Query => {
            ctx.outgoing.extend_from_slice(b"\x1b]99;");
            if !id.is_empty() {
                ctx.outgoing.extend_from_slice(b"i=");
                ctx.outgoing.extend_from_slice(id.as_bytes());
                ctx.outgoing.push(b';');
            }
            ctx.outgoing.extend_from_slice(
                b"p=title,body,close,?:e=0,1:d=0,1:o=always,unfocused,invisible:u=0,1,2",
            );
            ctx.outgoing.extend_from_slice(terminator_bytes(terminator));
            return;
        }
        Osc99Part::Close => {
            if let Some(pos) = ctx.osc99_pending.iter().position(|p| p.id == id) {
                ctx.osc99_pending.remove(pos);
            }
            ctx.events.push(TerminalEvent::NotificationClosed { id });
            return;
        }
        Osc99Part::Title | Osc99Part::Body | Osc99Part::Ignore => {}
    }

    if params.len() < 3 || part == Osc99Part::Ignore {
        if done
            && let Some(pos) = ctx.osc99_pending.iter().position(|p| p.id == id)
            && let Some(entry) = ctx.osc99_pending.remove(pos)
        {
            ctx.events.push(TerminalEvent::Notification {
                title: entry.title,
                body: entry.body,
            });
        }
        return;
    }

    let Some(raw) = join_osc_tail(params, 2) else {
        return;
    };
    if raw.len() > MAX_OSC99_PAYLOAD_BYTES * 2 {
        return;
    }
    let text = if base64_encoded {
        let Some(decoded) = decode_base64(raw.as_bytes()) else {
            return;
        };
        let Ok(s) = String::from_utf8(decoded) else {
            return;
        };
        s
    } else {
        raw
    };
    if text.len() > MAX_OSC99_PAYLOAD_BYTES {
        return;
    }

    let existing_pos = ctx.osc99_pending.iter().position(|p| p.id == id);
    let idx = if let Some(pos) = existing_pos {
        pos
    } else {
        if ctx.osc99_pending.len() >= MAX_OSC99_PENDING {
            ctx.osc99_pending.pop_front();
        }
        ctx.osc99_pending.push_back(Osc99Pending {
            id,
            title: String::new(),
            body: String::new(),
            urgency: 1,
            occasion: 0,
            total_bytes: 0,
        });
        ctx.osc99_pending.len() - 1
    };

    if let Some(entry) = ctx.osc99_pending.get_mut(idx) {
        if let Some(u) = urgency {
            entry.urgency = u;
        }
        if let Some(o) = occasion {
            entry.occasion = o;
        }
        if entry.total_bytes.saturating_add(text.len()) > MAX_OSC99_PAYLOAD_BYTES {
            ctx.osc99_pending.remove(idx);
            return;
        }
        entry.total_bytes += text.len();
        match part {
            Osc99Part::Title => entry.title.push_str(&text),
            Osc99Part::Body => entry.body.push_str(&text),
            Osc99Part::Close | Osc99Part::Query | Osc99Part::Ignore => {}
        }
    }

    if done && let Some(entry) = ctx.osc99_pending.remove(idx) {
        ctx.events.push(TerminalEvent::Notification {
            title: entry.title,
            body: entry.body,
        });
    }
}

/// Parses an `OSC 66 ; <metadata> ; <text>` Kitty Text Sizing payload into `(TextSizing, String)`.
pub(crate) fn parse_osc66(params: &[&[u8]]) -> Option<(TextSizing, String)> {
    if params.len() < 3 {
        return None;
    }
    let text = join_osc_tail(params, 2)?;
    if text.len() > MAX_OSC66_TEXT_BYTES || text.chars().any(char::is_control) {
        return None;
    }

    let mut sizing = TextSizing::default();
    for kv in params[1].split(|&b| b == b':') {
        if kv.is_empty() {
            continue;
        }
        let eq = kv.iter().position(|&b| b == b'=')?;
        let k = &kv[..eq];
        let v_str = std::str::from_utf8(&kv[eq + 1..]).ok()?;
        match k {
            b"s" => {
                let val = v_str.parse::<u8>().ok()?;
                if val > 7 {
                    return None;
                }
                sizing.scale = val.max(1);
            }
            b"w" => {
                let val = v_str.parse::<u8>().ok()?;
                if val > 7 {
                    return None;
                }
                sizing.width = val;
            }
            b"n" => {
                let val = v_str.parse::<u8>().ok()?;
                if val > 15 {
                    return None;
                }
                sizing.frac_num = val;
            }
            b"d" => {
                let val = v_str.parse::<u8>().ok()?;
                if val > 15 {
                    return None;
                }
                sizing.frac_den = val;
            }
            b"v" => {
                let val = v_str.parse::<u8>().ok()?;
                sizing.v_align = val.min(2);
            }
            b"h" => {
                let val = v_str.parse::<u8>().ok()?;
                sizing.h_align = val.min(2);
            }
            _ => {}
        }
    }

    if sizing.frac_num > 0 && sizing.frac_den <= sizing.frac_num {
        sizing.frac_num = 0;
        sizing.frac_den = 0;
    }

    Some((sizing, text))
}
