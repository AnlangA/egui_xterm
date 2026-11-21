//! Kitty Graphics Protocol (`APC G <key=val,...>;<base64>`) parser and executor.

use std::fmt::Write as _;

use crate::grid::{AbsLine, ScreenKind};

use super::base64::decode_base64;
use super::inflate::inflate_zlib;
use super::png::decode_png;
use super::store::{
    ImageFormat, ImagePlacement, ImageStore, MAX_IMAGE_DIMENSION, PendingKittyUpload, StoredImage,
};

const MAX_KITTY_PENDING_BYTES: usize = 32 * 1024 * 1024;

/// Kitty Graphics Protocol action (`a=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum KittyAction {
    /// Transmit image data without displaying (`a=t`).
    #[default]
    Transmit,
    /// Transmit image data and display immediately (`a=T`).
    TransmitAndDisplay,
    /// Put/display a previously transmitted image (`a=p`).
    Put,
    /// Delete images/placements (`a=d`).
    Delete,
    /// Query protocol support (`a=q`).
    Query,
}

/// Outcome returned to [`Terminal`](crate::term::Terminal) after executing a Kitty Graphics APC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KittyOutcome {
    /// Whether image store or placements changed.
    pub modified: bool,
    /// Whether the cursor should advance after placing an image.
    pub move_cursor: bool,
    /// Number of columns to advance when `move_cursor` is `true`.
    pub advance_cols: u16,
    /// Number of rows to advance when `move_cursor` is `true`.
    pub advance_rows: u16,
}

#[derive(Debug, Clone, Copy)]
struct KittyParams {
    action: Option<KittyAction>,
    format: u32,
    compressed: bool,
    more: bool,
    quiet: u8,
    image_id: u32,
    image_number: u32,
    placement_id: u32,
    width: u32,
    height: u32,
    cols: u16,
    rows: u16,
    src_x: u32,
    src_y: u32,
    src_w: u32,
    src_h: u32,
    z_index: i32,
    no_cursor_move: bool,
    delete_target: u8,
}

impl Default for KittyParams {
    fn default() -> Self {
        Self {
            action: None,
            format: 32,
            compressed: false,
            more: false,
            quiet: 0,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            width: 0,
            height: 0,
            cols: 0,
            rows: 0,
            src_x: 0,
            src_y: 0,
            src_w: 0,
            src_h: 0,
            z_index: 0,
            no_cursor_move: false,
            delete_target: b'a',
        }
    }
}

fn parse_u32(val: &[u8]) -> Option<u32> {
    std::str::from_utf8(val).ok()?.parse::<u32>().ok()
}

fn parse_i32(val: &[u8]) -> Option<i32> {
    std::str::from_utf8(val).ok()?.parse::<i32>().ok()
}

fn parse_u16(val: &[u8]) -> Option<u16> {
    std::str::from_utf8(val).ok()?.parse::<u16>().ok()
}

fn parse_header(header: &[u8]) -> KittyParams {
    let mut params = KittyParams::default();
    if header.is_empty() {
        return params;
    }
    for pair in header.split(|&b| b == b',') {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            continue;
        };
        let key = &pair[..eq];
        let val = &pair[eq + 1..];
        match key {
            b"a" => {
                params.action = match val {
                    b"t" => Some(KittyAction::Transmit),
                    b"T" => Some(KittyAction::TransmitAndDisplay),
                    b"p" => Some(KittyAction::Put),
                    b"d" => Some(KittyAction::Delete),
                    b"q" => Some(KittyAction::Query),
                    _ => params.action,
                };
            }
            b"f" => {
                if let Some(v) = parse_u32(val) {
                    params.format = v;
                }
            }
            b"o" => {
                params.compressed = val == b"z";
            }
            b"m" => {
                params.more = val == b"1";
            }
            b"q" => {
                if let Some(v) = parse_u32(val) {
                    params.quiet = u8::try_from(v.min(2)).unwrap_or(2);
                }
            }
            b"i" => {
                if let Some(v) = parse_u32(val) {
                    params.image_id = v;
                }
            }
            b"I" => {
                if let Some(v) = parse_u32(val) {
                    params.image_number = v;
                }
            }
            b"p" => {
                if let Some(v) = parse_u32(val) {
                    params.placement_id = v;
                }
            }
            b"s" => {
                if let Some(v) = parse_u32(val) {
                    params.width = v;
                }
            }
            b"v" => {
                if let Some(v) = parse_u32(val) {
                    params.height = v;
                }
            }
            b"c" => {
                if let Some(v) = parse_u16(val) {
                    params.cols = v;
                }
            }
            b"r" => {
                if let Some(v) = parse_u16(val) {
                    params.rows = v;
                }
            }
            b"x" => {
                if let Some(v) = parse_u32(val) {
                    params.src_x = v;
                }
            }
            b"y" => {
                if let Some(v) = parse_u32(val) {
                    params.src_y = v;
                }
            }
            b"w" => {
                if let Some(v) = parse_u32(val) {
                    params.src_w = v;
                }
            }
            b"h" => {
                if let Some(v) = parse_u32(val) {
                    params.src_h = v;
                }
            }
            b"z" => {
                if let Some(v) = parse_i32(val) {
                    params.z_index = v;
                }
            }
            b"C" => {
                params.no_cursor_move = val == b"1";
            }
            b"d" => {
                if let Some(&b) = val.first() {
                    params.delete_target = b;
                }
            }
            _ => {}
        }
    }
    params
}

fn write_response(
    outgoing: &mut Vec<u8>,
    image_id: u32,
    image_number: u32,
    placement_id: u32,
    quiet: u8,
    status: Result<(), &str>,
) {
    let is_ok = status.is_ok();
    if is_ok && quiet >= 1 {
        return;
    }
    if !is_ok && quiet >= 2 {
        return;
    }
    if image_id == 0 && image_number == 0 && is_ok {
        return;
    }

    let mut resp = String::from("\x1b_G");
    if image_id != 0 {
        let _ = write!(resp, "i={image_id}");
    } else if image_number != 0 {
        let _ = write!(resp, "I={image_number}");
    }
    if placement_id != 0 {
        let _ = write!(resp, ",p={placement_id}");
    }
    match status {
        Ok(()) => resp.push_str(";OK\x1b\\"),
        Err(msg) => {
            let _ = write!(resp, ";{msg}\x1b\\");
        }
    }
    outgoing.extend_from_slice(resp.as_bytes());
}

fn decode_image_payload(
    raw_b64: &[u8],
    format: u32,
    compressed: bool,
    width: u32,
    height: u32,
    max_budget: usize,
) -> Result<(u32, u32, Vec<u8>), &'static str> {
    let decoded = decode_base64(raw_b64).ok_or("EINVAL:invalid base64")?;
    let payload = if compressed {
        inflate_zlib(&decoded, max_budget).map_err(|_| "EINVAL:zlib inflate failed")?
    } else {
        decoded
    };

    match format {
        100 => {
            let png = decode_png(&payload, max_budget).map_err(|_| "EINVAL:png decode failed")?;
            Ok((png.width, png.height, png.rgba))
        }
        24 => {
            if width == 0
                || height == 0
                || width > MAX_IMAGE_DIMENSION
                || height > MAX_IMAGE_DIMENSION
            {
                return Err("EINVAL:invalid dimensions");
            }
            let expected = (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(3))
                .ok_or("EINVAL:dimension overflow")?;
            if payload.len() < expected {
                return Err("EINVAL:truncated rgb payload");
            }
            let rgba_len = (width as usize) * (height as usize) * 4;
            if rgba_len > max_budget {
                return Err("ENOMEM:image exceeds budget");
            }
            let mut rgba = Vec::with_capacity(rgba_len);
            for rgb in payload[..expected].as_chunks::<3>().0 {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            Ok((width, height, rgba))
        }
        32 => {
            if width == 0
                || height == 0
                || width > MAX_IMAGE_DIMENSION
                || height > MAX_IMAGE_DIMENSION
            {
                return Err("EINVAL:invalid dimensions");
            }
            let expected = (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(4))
                .ok_or("EINVAL:dimension overflow")?;
            if payload.len() < expected {
                return Err("EINVAL:truncated rgba payload");
            }
            if expected > max_budget {
                return Err("ENOMEM:image exceeds budget");
            }
            Ok((width, height, payload[..expected].to_vec()))
        }
        _ => Err("EINVAL:unsupported format"),
    }
}

fn compute_cursor_advance(
    cols: u16,
    rows: u16,
    img_w: u32,
    img_h: u32,
    cell_px: (u16, u16),
    no_cursor_move: bool,
) -> (bool, u16, u16) {
    if no_cursor_move {
        return (false, 0, 0);
    }
    let cw = u32::from(cell_px.0.max(1));
    let ch = u32::from(cell_px.1.max(1));
    let eff_cols = if cols > 0 {
        cols
    } else {
        u16::try_from(img_w.div_ceil(cw)).unwrap_or(u16::MAX)
    };
    let eff_rows = if rows > 0 {
        rows
    } else {
        u16::try_from(img_h.div_ceil(ch)).unwrap_or(u16::MAX)
    };
    (true, eff_cols, eff_rows)
}

/// Processes a Kitty Graphics Protocol `APC` payload (`G...`).
pub(crate) fn handle_kitty_apc(
    store: &mut ImageStore,
    data: &[u8],
    screen: ScreenKind,
    cursor_abs_line: AbsLine,
    cursor_col: u16,
    cell_px: (u16, u16),
    outgoing: &mut Vec<u8>,
) -> KittyOutcome {
    if !data.starts_with(b"G") {
        return KittyOutcome::default();
    }
    let rest = &data[1..];
    let (header, payload_b64) = match rest.iter().position(|&b| b == b';') {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => (rest, &[][..]),
    };

    let params = parse_header(header);

    // Handle continuation of an in-progress chunked upload.
    if let Some(mut pending) = store.pending_kitty.take() {
        if pending.payload.len().saturating_add(payload_b64.len()) > MAX_KITTY_PENDING_BYTES {
            write_response(
                outgoing,
                pending.image_id,
                pending.image_number,
                pending.placement_id,
                pending.quiet,
                Err("ENOMEM:chunked payload too large"),
            );
            return KittyOutcome::default();
        }
        pending.payload.extend_from_slice(payload_b64);
        if params.more {
            store.pending_kitty = Some(pending);
            return KittyOutcome::default();
        }
        return finish_transmit(
            store,
            pending,
            screen,
            cursor_abs_line,
            cursor_col,
            cell_px,
            outgoing,
        );
    }

    let action = params.action.unwrap_or(KittyAction::Transmit);

    match action {
        KittyAction::Query => {
            let status = decode_image_payload(
                payload_b64,
                params.format,
                params.compressed,
                params.width,
                params.height,
                store.memory_budget(),
            )
            .map(|_| ());
            write_response(
                outgoing,
                params.image_id,
                params.image_number,
                params.placement_id,
                params.quiet,
                status,
            );
            KittyOutcome::default()
        }
        KittyAction::Delete => {
            execute_delete(store, &params, screen, cursor_abs_line, cursor_col);
            KittyOutcome {
                modified: true,
                ..KittyOutcome::default()
            }
        }
        KittyAction::Put => {
            let resolved_id = if params.image_id != 0 {
                Some(params.image_id)
            } else if params.image_number != 0 {
                store.resolve_image_number(params.image_number)
            } else {
                None
            };
            let Some(img_id) = resolved_id else {
                write_response(
                    outgoing,
                    params.image_id,
                    params.image_number,
                    params.placement_id,
                    params.quiet,
                    Err("ENOENT:image not found"),
                );
                return KittyOutcome::default();
            };
            let Some((img_w, img_h)) = store.get_image(img_id).map(|img| (img.width, img.height))
            else {
                write_response(
                    outgoing,
                    img_id,
                    params.image_number,
                    params.placement_id,
                    params.quiet,
                    Err("ENOENT:image not found"),
                );
                return KittyOutcome::default();
            };

            let placement = ImagePlacement {
                image_id: img_id,
                placement_id: params.placement_id,
                abs_line: cursor_abs_line,
                col: cursor_col,
                cols: params.cols,
                rows: params.rows,
                src_x: params.src_x,
                src_y: params.src_y,
                src_w: params.src_w,
                src_h: params.src_h,
                z_index: params.z_index,
            };
            store.add_placement(screen, placement);
            write_response(
                outgoing,
                img_id,
                params.image_number,
                params.placement_id,
                params.quiet,
                Ok(()),
            );
            let (move_cursor, advance_cols, advance_rows) = compute_cursor_advance(
                params.cols,
                params.rows,
                img_w,
                img_h,
                cell_px,
                params.no_cursor_move,
            );
            KittyOutcome {
                modified: true,
                move_cursor,
                advance_cols,
                advance_rows,
            }
        }
        KittyAction::Transmit | KittyAction::TransmitAndDisplay => {
            let pending = PendingKittyUpload {
                image_id: params.image_id,
                image_number: params.image_number,
                placement_id: params.placement_id,
                action,
                format: params.format,
                compressed: params.compressed,
                width: params.width,
                height: params.height,
                cols: params.cols,
                rows: params.rows,
                src_x: params.src_x,
                src_y: params.src_y,
                src_w: params.src_w,
                src_h: params.src_h,
                z_index: params.z_index,
                no_cursor_move: params.no_cursor_move,
                quiet: params.quiet,
                payload: payload_b64.to_vec(),
            };
            if params.more {
                store.pending_kitty = Some(pending);
                return KittyOutcome::default();
            }
            finish_transmit(
                store,
                pending,
                screen,
                cursor_abs_line,
                cursor_col,
                cell_px,
                outgoing,
            )
        }
    }
}

fn finish_transmit(
    store: &mut ImageStore,
    pending: PendingKittyUpload,
    screen: ScreenKind,
    cursor_abs_line: AbsLine,
    cursor_col: u16,
    cell_px: (u16, u16),
    outgoing: &mut Vec<u8>,
) -> KittyOutcome {
    let decoded = decode_image_payload(
        &pending.payload,
        pending.format,
        pending.compressed,
        pending.width,
        pending.height,
        store.memory_budget(),
    );

    let (w, h, rgba) = match decoded {
        Ok(v) => v,
        Err(msg) => {
            write_response(
                outgoing,
                pending.image_id,
                pending.image_number,
                pending.placement_id,
                pending.quiet,
                Err(msg),
            );
            return KittyOutcome::default();
        }
    };

    let image_id = if pending.image_id != 0 {
        pending.image_id
    } else {
        store.allocate_id()
    };

    let stored = StoredImage {
        id: image_id,
        width: w,
        height: h,
        format: ImageFormat::Rgba8,
        rgba,
    };

    if !store.insert_image(stored) {
        write_response(
            outgoing,
            image_id,
            pending.image_number,
            pending.placement_id,
            pending.quiet,
            Err("ENOMEM:image exceeds memory budget"),
        );
        return KittyOutcome::default();
    }

    if pending.image_number != 0 {
        store.register_image_number(pending.image_number, image_id);
    }

    let mut outcome = KittyOutcome {
        modified: true,
        ..KittyOutcome::default()
    };

    if pending.action == KittyAction::TransmitAndDisplay {
        let placement = ImagePlacement {
            image_id,
            placement_id: pending.placement_id,
            abs_line: cursor_abs_line,
            col: cursor_col,
            cols: pending.cols,
            rows: pending.rows,
            src_x: pending.src_x,
            src_y: pending.src_y,
            src_w: pending.src_w,
            src_h: pending.src_h,
            z_index: pending.z_index,
        };
        store.add_placement(screen, placement);
        let (move_cursor, advance_cols, advance_rows) = compute_cursor_advance(
            pending.cols,
            pending.rows,
            w,
            h,
            cell_px,
            pending.no_cursor_move,
        );
        outcome.move_cursor = move_cursor;
        outcome.advance_cols = advance_cols;
        outcome.advance_rows = advance_rows;
    }

    write_response(
        outgoing,
        image_id,
        pending.image_number,
        pending.placement_id,
        pending.quiet,
        Ok(()),
    );
    outcome
}

fn execute_delete(
    store: &mut ImageStore,
    params: &KittyParams,
    screen: ScreenKind,
    cursor_abs_line: AbsLine,
    cursor_col: u16,
) {
    let free_images = params.delete_target.is_ascii_uppercase();
    match params.delete_target {
        b'a' | b'A' => {
            store.clear_screen(screen);
            if free_images {
                store.gc_unreferenced();
            }
        }
        b'i' | b'I' => {
            let id = params.image_id;
            let pid = params.placement_id;
            store.delete_placements_where(screen, free_images, |p| {
                p.image_id == id && (pid == 0 || p.placement_id == pid)
            });
        }
        b'n' | b'N' => {
            if let Some(id) = store.resolve_image_number(params.image_number) {
                let pid = params.placement_id;
                store.delete_placements_where(screen, free_images, |p| {
                    p.image_id == id && (pid == 0 || p.placement_id == pid)
                });
            }
        }
        b'c' | b'C' => {
            store.delete_placements_where(screen, free_images, |p| {
                let end_line = p.abs_line.0.saturating_add(u64::from(p.rows.max(1)));
                let end_col = p.col.saturating_add(p.cols.max(1));
                cursor_abs_line.0 >= p.abs_line.0
                    && cursor_abs_line.0 < end_line
                    && cursor_col >= p.col
                    && cursor_col < end_col
            });
        }
        b'p' | b'P' => {
            let target_col = u16::try_from(params.src_x.saturating_sub(1)).unwrap_or(0);
            let target_row = u64::from(params.src_y.saturating_sub(1));
            store.delete_placements_where(screen, free_images, |p| {
                let end_line = p.abs_line.0.saturating_add(u64::from(p.rows.max(1)));
                let end_col = p.col.saturating_add(p.cols.max(1));
                target_row >= p.abs_line.0
                    && target_row < end_line
                    && target_col >= p.col
                    && target_col < end_col
            });
        }
        b'r' | b'R' => {
            let min_id = params.src_x;
            let max_id = params.src_y;
            store.delete_placements_where(screen, free_images, |p| {
                p.image_id >= min_id && p.image_id <= max_id
            });
        }
        b'z' | b'Z' => {
            let z = params.z_index;
            store.delete_placements_where(screen, free_images, |p| p.z_index == z);
        }
        _ => {}
    }
}
