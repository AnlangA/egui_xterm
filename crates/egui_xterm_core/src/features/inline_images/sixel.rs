//! Pure-Rust streaming Sixel (`DCS P1 ; P2 ; P3 q ... ST`) decoder.

use crate::parser::CsiParams;

use super::store::MAX_IMAGE_DIMENSION;

const MAX_SIXEL_DIM: usize = MAX_IMAGE_DIMENSION as usize;
const MAX_SIXEL_PIXELS: usize = 16 * 1024 * 1024;
const MAX_COLOR_REGISTERS: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SixelState {
    Normal,
    Raster,
    Color,
    Repeat,
}

/// Decoded Sixel image with pixel aspect ratio metadata and final color palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SixelImage {
    /// Decoded image width in pixels.
    pub width: u32,
    /// Decoded image height in pixels.
    pub height: u32,
    /// Pixel aspect ratio numerator (`Pan`, vertical factor, `>= 1`).
    pub pan: u16,
    /// Pixel aspect ratio denominator (`Pad`, horizontal factor, `>= 1`).
    pub pad: u16,
    /// Decoded `RGBA8` pixel buffer (`width * height * 4` bytes).
    pub rgba: Vec<u8>,
    /// Final Sixel color register table after decoding the stream.
    pub palette: Vec<[u8; 4]>,
}

/// Streaming Sixel graphics decoder driven by `DCS ... q` `hook`/`put`/`unhook`.
///
/// # Examples
///
/// Decode a Sixel `DCS` payload and persist color registers across images (`DEC ?1070l`):
///
/// ```
/// use egui_xterm_core::CsiParams;
/// use egui_xterm_core::features::inline_images::sixel::SixelDecoder;
///
/// let params = CsiParams::new();
/// let mut decoder = SixelDecoder::new(&params);
/// for &byte in b"\"1;1;2;6#1;2;100;0;0#1!2~" {
///     decoder.put(byte);
/// }
///
/// let image = decoder
///     .finish_sixel()
///     .ok_or("expected non-empty Sixel image")?;
/// assert_eq!((image.width, image.height), (2, 6));
/// assert_eq!((image.pan, image.pad), (1, 1));
/// assert_eq!(&image.rgba[..4], &[255, 0, 0, 255]);
///
/// // A palette-only DCS stream updates color registers and returns `Err(palette)`.
/// let mut palette_only = SixelDecoder::with_shared_palette(&params, image.palette);
/// for &byte in b"#7;2;0;100;0" {
///     palette_only.put(byte);
/// }
/// let shared_palette = palette_only
///     .finish_sixel_or_palette()
///     .err()
///     .ok_or("palette-only stream emits no image")?;
///
/// let mut next = SixelDecoder::with_shared_palette(&params, shared_palette);
/// for &byte in b"#7@" {
///     next.put(byte);
/// }
/// let reused = next
///     .finish_sixel()
///     .ok_or("expected Sixel image using shared register 7")?;
/// assert_eq!(reused.rgba, vec![0, 255, 0, 255]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct SixelDecoder {
    state: SixelState,
    params: [u32; 5],
    param_idx: usize,
    has_digit: bool,
    palette: Vec<[u8; 4]>,
    active_color: usize,
    p1: u16,
    p2: u16,
    p3: u16,
    pan: u16,
    pad: u16,
    bg_transparent: bool,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    hint_width: usize,
    rows: Vec<Vec<[u8; 4]>>,
    has_unset_in_rows: bool,
    overflow: bool,
}

impl SixelDecoder {
    /// Creates a new [`SixelDecoder`] from the `DCS` header parameters with a fresh VT340 palette.
    #[must_use]
    pub fn new(dcs_params: &CsiParams) -> Self {
        Self::with_palette(dcs_params, default_vt340_palette())
    }

    /// Creates a new [`SixelDecoder`] seeded with a shared color register table (`DEC ?1070l`).
    ///
    /// If `palette` is empty, the default 16-color VT340 palette is used.
    #[must_use]
    pub fn with_shared_palette(dcs_params: &CsiParams, mut palette: Vec<[u8; 4]>) -> Self {
        if palette.is_empty() {
            palette = default_vt340_palette();
        } else if palette.len() > MAX_COLOR_REGISTERS {
            palette.truncate(MAX_COLOR_REGISTERS);
        }
        Self::with_palette(dcs_params, palette)
    }

    fn with_palette(dcs_params: &CsiParams, palette: Vec<[u8; 4]>) -> Self {
        let p1 = dcs_params.get_or(0, 0);
        let p2 = dcs_params.get_or(1, 0);
        let p3 = dcs_params.get_or(2, 0);
        let (pan, pad) = aspect_from_p1(p1);
        // VT340 P2: 1 keeps background transparent; 0 or 2 fills unset pixels with register 0.
        let bg_transparent = p2 == 1;
        Self {
            state: SixelState::Normal,
            params: [0; 5],
            param_idx: 0,
            has_digit: false,
            palette,
            active_color: 0,
            p1,
            p2,
            p3,
            pan,
            pad,
            bg_transparent,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            hint_width: 0,
            rows: Vec::new(),
            has_unset_in_rows: false,
            overflow: false,
        }
    }

    /// Returns the `P1` macro aspect ratio parameter from the `DCS` header.
    #[must_use]
    pub const fn p1(&self) -> u16 {
        self.p1
    }

    /// Returns the `P2` background select parameter from the `DCS` header.
    #[must_use]
    pub const fn p2(&self) -> u16 {
        self.p2
    }

    /// Returns the `P3` horizontal grid size parameter from the `DCS` header.
    #[must_use]
    pub const fn p3(&self) -> u16 {
        self.p3
    }

    /// Returns the effective vertical pixel aspect ratio numerator (`Pan`).
    #[must_use]
    pub const fn pan(&self) -> u16 {
        self.pan
    }

    /// Returns the effective horizontal pixel aspect ratio denominator (`Pad`).
    #[must_use]
    pub const fn pad(&self) -> u16 {
        self.pad
    }

    /// Returns `true` when `P2 == 1` (unset pixels remain transparent `[0, 0, 0, 0]`).
    #[must_use]
    pub const fn is_bg_transparent(&self) -> bool {
        self.bg_transparent
    }

    /// Returns the current color register table.
    #[must_use]
    pub fn palette(&self) -> &[[u8; 4]] {
        &self.palette
    }

    /// Consumes the decoder and returns its color register table.
    #[must_use]
    pub fn into_palette(mut self) -> Vec<[u8; 4]> {
        self.flush_pending_state();
        self.palette
    }

    /// Feeds a single raw `DCS` payload byte into the decoder.
    pub fn put(&mut self, byte: u8) {
        if self.overflow {
            return;
        }
        match self.state {
            SixelState::Normal => self.handle_normal(byte),
            SixelState::Raster => self.handle_raster(byte),
            SixelState::Color => self.handle_color(byte),
            SixelState::Repeat => self.handle_repeat(byte),
        }
    }

    /// Finalizes the decoded Sixel image as `(width, height, rgba8)` if non-empty.
    #[must_use]
    pub fn finish(self) -> Option<(u32, u32, Vec<u8>)> {
        self.finish_sixel()
            .map(|img| (img.width, img.height, img.rgba))
    }

    /// Finalizes the decoded Sixel image with aspect ratio metadata and palette if non-empty.
    #[must_use]
    pub fn finish_sixel(self) -> Option<SixelImage> {
        self.finish_sixel_or_palette().ok()
    }

    /// Finalizes the decoded Sixel image, returning `Err(palette)` if empty or overflowed.
    ///
    /// Useful in shared-palette (`DEC ?1070l`) mode when a DCS stream updates color registers
    /// without emitting visible pixels.
    pub fn finish_sixel_or_palette(mut self) -> Result<SixelImage, Vec<[u8; 4]>> {
        self.flush_pending_state();
        if self.overflow || self.width == 0 || self.height == 0 {
            return Err(self.palette);
        }
        let Ok(w) = u32::try_from(self.width) else {
            return Err(self.palette);
        };
        let Ok(h) = u32::try_from(self.height) else {
            return Err(self.palette);
        };
        let Some(total_bytes) = self
            .width
            .checked_mul(self.height)
            .and_then(|px| px.checked_mul(4))
        else {
            return Err(self.palette);
        };

        let mut rgba = Vec::with_capacity(total_bytes);
        let rows = std::mem::take(&mut self.rows);
        let row_count = rows.len();

        if self.bg_transparent {
            for row in rows {
                let row_len = row.len().min(self.width);
                if row_len > 0 {
                    rgba.extend_from_slice(row[..row_len].as_flattened());
                }
                let pad_pixels = self.width - row_len;
                if pad_pixels > 0 {
                    rgba.resize(rgba.len() + pad_pixels * 4, 0);
                }
            }
            if row_count < self.height {
                rgba.resize(rgba.len() + (self.height - row_count) * self.width * 4, 0);
            }
        } else {
            let bg = self.palette.first().copied().unwrap_or([0, 0, 0, 255]);
            for row in rows {
                let row_len = row.len().min(self.width);
                if row_len > 0 {
                    if self.has_unset_in_rows {
                        for &px in &row[..row_len] {
                            let resolved = if px == [0, 0, 0, 0] { bg } else { px };
                            rgba.extend_from_slice(&resolved);
                        }
                    } else {
                        rgba.extend_from_slice(row[..row_len].as_flattened());
                    }
                }
                let pad_pixels = self.width - row_len;
                if pad_pixels > 0 {
                    fill_rgba_slice(&mut rgba, bg, pad_pixels);
                }
            }
            if row_count < self.height {
                fill_rgba_slice(&mut rgba, bg, (self.height - row_count) * self.width);
            }
        }

        Ok(SixelImage {
            width: w,
            height: h,
            pan: self.pan,
            pad: self.pad,
            rgba,
            palette: self.palette,
        })
    }

    fn reset_params(&mut self) {
        self.params = [0; 5];
        self.param_idx = 0;
        self.has_digit = false;
    }

    fn push_param_byte(&mut self, byte: u8) -> bool {
        if byte.is_ascii_digit() {
            if self.param_idx < self.params.len() {
                self.params[self.param_idx] = self.params[self.param_idx]
                    .saturating_mul(10)
                    .saturating_add(u32::from(byte - b'0'));
                self.has_digit = true;
            }
            true
        } else if byte == b';' {
            if self.param_idx + 1 < self.params.len() {
                self.param_idx += 1;
            }
            self.has_digit = false;
            true
        } else {
            false
        }
    }

    fn handle_normal(&mut self, byte: u8) {
        match byte {
            b'"' => {
                self.reset_params();
                self.state = SixelState::Raster;
            }
            b'#' => {
                self.reset_params();
                self.state = SixelState::Color;
            }
            b'!' => {
                self.reset_params();
                self.state = SixelState::Repeat;
            }
            b'$' => {
                self.x = 0;
            }
            b'-' => {
                self.x = 0;
                self.y = self.y.saturating_add(6);
            }
            0x3F..=0x7E => {
                self.draw_sixel(byte - 0x3F, 1);
            }
            _ => {}
        }
    }

    fn flush_pending_state(&mut self) {
        if self.overflow {
            self.state = SixelState::Normal;
            return;
        }
        match self.state {
            SixelState::Normal => {}
            SixelState::Raster => {
                self.commit_raster();
                self.state = SixelState::Normal;
            }
            SixelState::Color => {
                self.commit_color();
                self.state = SixelState::Normal;
            }
            SixelState::Repeat => {
                self.state = SixelState::Normal;
            }
        }
    }

    fn commit_raster(&mut self) {
        if !self.has_digit && self.param_idx == 0 {
            return;
        }
        self.pan = clamp_aspect(self.params[0]);
        self.pad = clamp_aspect(self.params[1]);
        let ph = usize::try_from(self.params[2]).unwrap_or(usize::MAX);
        let pv = usize::try_from(self.params[3]).unwrap_or(usize::MAX);
        if ph > 0 {
            self.hint_width = self.hint_width.max(ph.min(MAX_SIXEL_DIM));
        }
        if ph > 0 && pv > 0 {
            self.ensure_canvas(ph, pv);
        }
    }

    fn commit_color(&mut self) {
        if !self.has_digit && self.param_idx == 0 {
            return;
        }
        let max_reg = MAX_COLOR_REGISTERS - 1;
        let reg = usize::try_from(self.params[0])
            .unwrap_or(max_reg)
            .min(max_reg);
        if reg >= self.palette.len() {
            self.palette.resize(reg + 1, [255, 255, 255, 255]);
        }
        if self.param_idx >= 1 {
            let pu = self.params[1];
            let px = self.params[2];
            let py = self.params[3];
            let pz = self.params[4];
            match pu {
                1 => {
                    let rgb = hls_to_rgb(px, py, pz);
                    self.palette[reg] = [rgb[0], rgb[1], rgb[2], 255];
                }
                2 => {
                    self.palette[reg] = [pct_to_u8(px), pct_to_u8(py), pct_to_u8(pz), 255];
                }
                _ => {}
            }
        }
        self.active_color = reg;
    }

    fn handle_raster(&mut self, byte: u8) {
        if self.push_param_byte(byte) {
            return;
        }
        self.commit_raster();
        self.state = SixelState::Normal;
        self.handle_normal(byte);
    }

    fn handle_color(&mut self, byte: u8) {
        if self.push_param_byte(byte) {
            return;
        }
        self.commit_color();
        self.state = SixelState::Normal;
        self.handle_normal(byte);
    }

    fn handle_repeat(&mut self, byte: u8) {
        if byte.is_ascii_digit() {
            self.push_param_byte(byte);
            return;
        }
        let count = usize::try_from(self.params[0].max(1)).unwrap_or(usize::MAX);
        self.state = SixelState::Normal;
        if matches!(byte, 0x3F..=0x7E) {
            self.draw_sixel(byte - 0x3F, count);
        } else {
            self.handle_normal(byte);
        }
    }

    fn ensure_canvas(&mut self, req_w: usize, req_h: usize) {
        let new_w = self.width.max(req_w);
        let new_h = self.height.max(req_h);
        if new_w > MAX_SIXEL_DIM || new_h > MAX_SIXEL_DIM {
            self.overflow = true;
            return;
        }
        let Some(total) = new_w.checked_mul(new_h) else {
            self.overflow = true;
            return;
        };
        if total > MAX_SIXEL_PIXELS {
            self.overflow = true;
            return;
        }
        if new_h > self.rows.len() {
            self.rows.resize_with(new_h, Vec::new);
        }
        self.width = new_w;
        self.height = new_h;
    }

    fn draw_sixel(&mut self, bits: u8, repeat: usize) {
        let count = repeat.min(MAX_SIXEL_DIM.saturating_sub(self.x));
        if count == 0 {
            return;
        }
        let Some(req_w) = self.x.checked_add(count) else {
            self.overflow = true;
            return;
        };
        if bits == 0 {
            if !self.bg_transparent {
                let Some(req_h) = self.y.checked_add(6) else {
                    self.overflow = true;
                    return;
                };
                self.ensure_canvas(req_w, req_h);
                if self.overflow {
                    return;
                }
            }
            self.x = req_w;
            return;
        }

        let color = self
            .palette
            .get(self.active_color)
            .copied()
            .unwrap_or([255, 255, 255, 255]);
        let max_bit = (7 - bits.leading_zeros()) as usize;
        let Some(req_h) = self.y.checked_add(max_bit + 1) else {
            self.overflow = true;
            return;
        };
        self.ensure_canvas(req_w, req_h);
        if self.overflow {
            return;
        }

        let target_cap = self.width.max(self.hint_width).max(req_w);
        for bit in 0..=max_bit {
            if (bits & (1 << bit)) != 0 {
                let py = self.y + bit;
                if let Some(row) = self.rows.get_mut(py) {
                    if row.capacity() < target_cap {
                        row.reserve(target_cap - row.len());
                    }
                    if row.len() < self.x {
                        row.resize(self.x, [0, 0, 0, 0]);
                        self.has_unset_in_rows = true;
                    }
                    if row.len() < req_w {
                        let overlap = row.len().saturating_sub(self.x);
                        if overlap > 0 {
                            row[self.x..].fill(color);
                        }
                        row.resize(req_w, color);
                    } else {
                        row[self.x..req_w].fill(color);
                    }
                }
            }
        }
        self.x = req_w;
    }
}

const fn aspect_from_p1(p1: u16) -> (u16, u16) {
    match p1 {
        0 | 1 | 5 | 6 => (2, 1),
        2 => (5, 1),
        3 | 4 => (3, 1),
        7..=9 => (1, 1),
        _ => (2, 1),
    }
}

fn clamp_aspect(value: u32) -> u16 {
    if value == 0 {
        1
    } else {
        u16::try_from(value).unwrap_or(u16::MAX)
    }
}

fn fill_rgba_slice(dst: &mut Vec<u8>, px: [u8; 4], pixel_count: usize) {
    if pixel_count == 0 {
        return;
    }
    if px[0] == px[1] && px[1] == px[2] && px[2] == px[3] {
        dst.resize(dst.len() + pixel_count * 4, px[0]);
        return;
    }
    let start = dst.len();
    dst.resize(start + pixel_count * 4, 0);
    for chunk in dst[start..].as_chunks_mut::<4>().0 {
        *chunk = px;
    }
}

fn pct_to_u8(pct: u32) -> u8 {
    let clamped = pct.min(100);
    u8::try_from((clamped * 255 + 50) / 100).unwrap_or(255)
}

fn hls_to_rgb(hue_deg: u32, light_pct: u32, sat_pct: u32) -> [u8; 3] {
    let h = f64::from(hue_deg % 360) / 360.0;
    let l = f64::from(light_pct.min(100)) / 100.0;
    let s = f64::from(sat_pct.min(100)) / 100.0;

    if s == 0.0 {
        let v = (l * 255.0).round().clamp(0.0, 255.0) as u8;
        return [v, v, v];
    }

    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;

    let hue_to_rgb = |t_in: f64| -> u8 {
        let mut t = t_in;
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let c = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (c * 255.0).round().clamp(0.0, 255.0) as u8
    };

    [
        hue_to_rgb(h + 1.0 / 3.0),
        hue_to_rgb(h),
        hue_to_rgb(h - 1.0 / 3.0),
    ]
}

/// Returns the default 16-color VT340 Sixel palette.
#[must_use]
pub fn default_vt340_palette() -> Vec<[u8; 4]> {
    vec![
        [0, 0, 0, 255],
        [51, 51, 204, 255],
        [204, 33, 33, 255],
        [51, 204, 51, 255],
        [204, 51, 204, 255],
        [51, 204, 204, 255],
        [204, 204, 51, 255],
        [135, 135, 135, 255],
        [66, 66, 66, 255],
        [84, 84, 255, 255],
        [255, 66, 66, 255],
        [84, 255, 84, 255],
        [255, 84, 255, 255],
        [84, 255, 255, 255],
        [255, 255, 84, 255],
        [204, 204, 204, 255],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{OscTerminator, Parser, VtHandler};

    fn make_params(vals: &[u16]) -> CsiParams {
        struct Capture(Option<CsiParams>);
        impl VtHandler for Capture {
            fn print(&mut self, _ch: char) {}
            fn execute(&mut self, _byte: u8) {}
            fn hook(&mut self, params: &CsiParams, _intermediates: &[u8], _action: u8) {
                self.0 = Some(params.clone());
            }
            fn put(&mut self, _byte: u8) {}
            fn unhook(&mut self) {}
            fn osc_dispatch(&mut self, _params: &[&[u8]], _terminator: OscTerminator) {}
            fn csi_dispatch(&mut self, _params: &CsiParams, _intermediates: &[u8], _action: u8) {}
            fn esc_dispatch(&mut self, _intermediates: &[u8], _action: u8) {}
        }
        let mut seq = String::from("\x1bP");
        for (i, v) in vals.iter().enumerate() {
            if i > 0 {
                seq.push(';');
            }
            seq.push_str(&v.to_string());
        }
        seq.push_str("q\x1b\\");
        let mut parser = Parser::new();
        let mut cap = Capture(None);
        parser.advance(seq.as_bytes(), &mut cap);
        cap.0.expect("DCS hook captured params")
    }

    fn decode_with(params: &CsiParams, data: &[u8]) -> Option<SixelImage> {
        let mut dec = SixelDecoder::new(params);
        for &b in data {
            dec.put(b);
        }
        dec.finish_sixel()
    }

    fn px_at(img: &SixelImage, x: u32, y: u32) -> &[u8] {
        let idx = ((y * img.width + x) * 4) as usize;
        &img.rgba[idx..idx + 4]
    }

    #[test]
    fn p2_background_modes_0_1_2_and_default() {
        // Omitted P2 defaults to 0 (opaque background using register 0).
        let empty = make_params(&[]);
        let img = decode_with(&empty, b"\"1;1;3;6#0;2;0;100;0#1;2;100;0;0#1~").expect("sixel");
        assert_eq!((img.width, img.height), (3, 6));
        assert_eq!(px_at(&img, 0, 0), &[255, 0, 0, 255]);
        assert_eq!(px_at(&img, 1, 0), &[0, 255, 0, 255]);
        assert_eq!(px_at(&img, 2, 5), &[0, 255, 0, 255]);

        // P2 = 0 with leading gap '?' and raster Ph;Pv declared before #0 redefinition.
        let p2_0 = make_params(&[0, 0, 0]);
        let img0 =
            decode_with(&p2_0, b"\"1;1;4;6#0;2;10;20;30#1;2;100;0;0#1?!~").expect("p2=0 sixel");
        let expected_bg = [pct_to_u8(10), pct_to_u8(20), pct_to_u8(30), 255];
        assert_eq!(px_at(&img0, 0, 0), &expected_bg);
        assert_eq!(px_at(&img0, 1, 0), &[255, 0, 0, 255]);
        assert_eq!(px_at(&img0, 2, 0), &expected_bg);
        assert_eq!(px_at(&img0, 3, 5), &expected_bg);

        // P2 = 2 fills unset pixels with register 0, including raster with no sixel data.
        let p2_2 = make_params(&[0, 2, 0]);
        let img2 = decode_with(&p2_2, b"\"1;1;3;4#0;2;0;0;100#1;2;100;0;0#1@").expect("p2=2");
        assert_eq!((img2.width, img2.height), (3, 4));
        assert_eq!(px_at(&img2, 0, 0), &[255, 0, 0, 255]);
        assert_eq!(px_at(&img2, 0, 1), &[0, 0, 255, 255]);
        assert_eq!(px_at(&img2, 2, 3), &[0, 0, 255, 255]);

        let img2_empty = decode_with(&p2_2, b"\"1;1;2;2#0;2;100;100;0").expect("p2=2 raster only");
        assert_eq!(img2_empty.rgba, [255, 255, 0, 255].repeat(4));

        // Raster-only stream ending directly in Raster state fills with default register 0.
        let img2_raster_only = decode_with(&p2_2, b"\"1;1;2;2").expect("raster-only ST");
        assert_eq!(img2_raster_only.rgba, [0, 0, 0, 255].repeat(4));

        // P2 = 1 keeps unset pixels transparent [0, 0, 0, 0] while explicit #0 draws opaque reg 0.
        let p2_1 = make_params(&[0, 1, 0]);
        let img1 = decode_with(&p2_1, b"\"1;1;4;6#0;2;0;100;0#1;2;100;0;0#1?!~#0@").expect("p2=1");
        assert_eq!(px_at(&img1, 0, 0), &[0, 0, 0, 0]);
        assert_eq!(px_at(&img1, 1, 0), &[255, 0, 0, 255]);
        assert_eq!(px_at(&img1, 2, 0), &[0, 255, 0, 255]);
        assert_eq!(px_at(&img1, 2, 1), &[0, 0, 0, 0]);
        assert_eq!(px_at(&img1, 3, 5), &[0, 0, 0, 0]);
    }

    #[test]
    fn p1_and_raster_pan_pad_ph_pv() {
        let cases = [
            (0, (2, 1)),
            (1, (2, 1)),
            (2, (5, 1)),
            (3, (3, 1)),
            (4, (3, 1)),
            (5, (2, 1)),
            (6, (2, 1)),
            (7, (1, 1)),
            (8, (1, 1)),
            (9, (1, 1)),
        ];
        for (p1, expected) in cases {
            let params = make_params(&[p1, 1, 4]);
            let mut dec = SixelDecoder::new(&params);
            assert_eq!((dec.p1(), dec.p2(), dec.p3()), (p1, 1, 4));
            assert_eq!((dec.pan(), dec.pad()), expected);
            assert!(dec.is_bg_transparent());
            dec.put(b'#');
            dec.put(b'1');
            dec.put(b'~');
            let img = dec.finish_sixel().expect("single sixel");
            assert_eq!((img.pan, img.pad), expected);
        }

        // Raster "Pan;Pad;Ph;Pv overrides P1 and pre-allocates canvas dimensions.
        let p1_2 = make_params(&[2, 1, 0]);
        let mut dec = SixelDecoder::new(&p1_2);
        for &b in b"\"3;2;10;8#1@" {
            dec.put(b);
        }
        assert_eq!(dec.rows.len(), 8);
        assert!(dec.rows[0].capacity() >= 10);
        let img = dec.finish_sixel().expect("raster override");
        assert_eq!((img.width, img.height, img.pan, img.pad), (10, 8, 3, 2));

        // Zero Pan;Pad in raster clamps to (1, 1).
        let img_zero_aspect = decode_with(&p1_2, b"\"0;0;2;6#1~").expect("zero aspect");
        assert_eq!((img_zero_aspect.pan, img_zero_aspect.pad), (1, 1));
    }

    #[test]
    fn repeat_carriage_return_and_newline_overlay() {
        let p2_1 = make_params(&[7, 1, 0]);
        // Band 0: reg 1 draws 4 pixels of bit 0 ('@'), '$' returns x to 0, reg 2 draws bit 1 ('A')
        // at x=1..=2 using '!2A', then '-' moves to y=6 and '!0~' (count=1) draws at (0, 6..12).
        let img = decode_with(&p2_1, b"#1;2;100;0;0#2;2;0;100;0#1!4@$?#2!2A-#1!0~")
            .expect("repeat and overlay");
        assert_eq!((img.width, img.height), (4, 12));
        // Row 0: red from !4@, preserved under $ + ? + !2A (since A only sets bit 1).
        for x in 0..4 {
            assert_eq!(px_at(&img, x, 0), &[255, 0, 0, 255]);
        }
        // Row 1: transparent at x=0 and x=3, green at x=1 and x=2.
        assert_eq!(px_at(&img, 0, 1), &[0, 0, 0, 0]);
        assert_eq!(px_at(&img, 1, 1), &[0, 255, 0, 255]);
        assert_eq!(px_at(&img, 2, 1), &[0, 255, 0, 255]);
        assert_eq!(px_at(&img, 3, 1), &[0, 0, 0, 0]);
        // Band 1 (y=6..12): red at x=0, transparent at x=1..4.
        assert_eq!(px_at(&img, 0, 6), &[255, 0, 0, 255]);
        assert_eq!(px_at(&img, 0, 11), &[255, 0, 0, 255]);
        assert_eq!(px_at(&img, 1, 6), &[0, 0, 0, 0]);
    }

    #[test]
    fn color_definitions_rgb_hls_and_extended_registers() {
        let params = make_params(&[7, 1, 0]);
        // #1 RGB(100,50,0), #2 HLS(120,50,100) green, #3 HLS(240,50,100) blue,
        // #4 HLS(0,50,0) neutral gray, #300 RGB(0,100,100) cyan.
        let img = decode_with(
            &params,
            b"#1;2;100;50;0#2;1;120;50;100#3;1;240;50;100#4;1;0;50;0#300;2;0;100;100#1@#2@#3@#4@#300@",
        )
        .expect("colors");
        assert_eq!((img.width, img.height), (5, 1));
        assert_eq!(px_at(&img, 0, 0), &[255, 128, 0, 255]);
        assert_eq!(px_at(&img, 1, 0), &[0, 255, 0, 255]);
        assert_eq!(px_at(&img, 2, 0), &[0, 0, 255, 255]);
        assert_eq!(px_at(&img, 3, 0), &[128, 128, 128, 255]);
        assert_eq!(px_at(&img, 4, 0), &[0, 255, 255, 255]);
        assert!(img.palette.len() > 300);

        // finish() tuple compatibility check.
        let mut dec = SixelDecoder::new(&params);
        for &b in b"#1@" {
            dec.put(b);
        }
        let (w, h, rgba) = dec.finish().expect("finish tuple");
        assert_eq!((w, h), (1, 1));
        assert_eq!(rgba, vec![51, 51, 204, 255]);
    }

    #[test]
    fn shared_palette_persists_across_decoders() {
        let params = make_params(&[7, 1, 0]);

        // Image 1 defines registers 1 and 50.
        let mut dec1 = SixelDecoder::with_shared_palette(&params, Vec::new());
        for &b in b"#1;2;100;0;0#50;2;0;100;0#1@" {
            dec1.put(b);
        }
        let img1 = dec1.finish_sixel().expect("img1");
        assert_eq!(px_at(&img1, 0, 0), &[255, 0, 0, 255]);

        // Palette-only stream defines register 7 without emitting any sixel pixels.
        let mut dec2 = SixelDecoder::with_shared_palette(&params, img1.palette);
        for &b in b"#7;2;100;100;0" {
            dec2.put(b);
        }
        let err_palette = dec2
            .finish_sixel_or_palette()
            .expect_err("palette-only stream emits no image");
        assert_eq!(err_palette[7], [255, 255, 0, 255]);

        // Image 3 reuses registers 1, 50, and 7 without redefining them.
        let mut dec3 = SixelDecoder::with_shared_palette(&params, err_palette);
        for &b in b"#1@#50@#7@" {
            dec3.put(b);
        }
        let img3 = dec3.finish_sixel().expect("img3");
        assert_eq!(px_at(&img3, 0, 0), &[255, 0, 0, 255]);
        assert_eq!(px_at(&img3, 1, 0), &[0, 255, 0, 255]);
        assert_eq!(px_at(&img3, 2, 0), &[255, 255, 0, 255]);
        assert_eq!(dec3_roundtrip_palette(img3.palette), [255, 255, 0, 255]);
    }

    fn dec3_roundtrip_palette(pal: Vec<[u8; 4]>) -> [u8; 4] {
        let params = make_params(&[]);
        let dec = SixelDecoder::with_shared_palette(&params, pal);
        assert_eq!(dec.palette()[1], [255, 0, 0, 255]);
        dec.into_palette()[7]
    }

    #[test]
    fn growing_canvas_multiband_and_overflow_guards() {
        let p2_0 = make_params(&[7, 0, 0]);
        // Grows across 3 bands without Ph;Pv and with horizontal gaps.
        let img =
            decode_with(&p2_0, b"#1;2;100;0;0#1~~-#2;2;0;100;0#2!5?~-#1@").expect("multiband grow");
        assert_eq!((img.width, img.height), (6, 13));
        assert_eq!(px_at(&img, 0, 0), &[255, 0, 0, 255]);
        assert_eq!(px_at(&img, 5, 0), &[0, 0, 0, 255]);
        assert_eq!(px_at(&img, 4, 6), &[0, 0, 0, 255]);
        assert_eq!(px_at(&img, 5, 6), &[0, 255, 0, 255]);
        assert_eq!(px_at(&img, 0, 12), &[255, 0, 0, 255]);

        // Overflow checks: dimension > MAX_SIXEL_DIM or total pixels > MAX_SIXEL_PIXELS.
        assert!(decode_with(&p2_0, b"\"1;1;9000;10#1~").is_none());
        assert!(decode_with(&p2_0, b"\"1;1;10;9000#1~").is_none());
        assert!(decode_with(&p2_0, b"\"1;1;5000;5000#1~").is_none());

        // Huge repeat clips cleanly at MAX_SIXEL_DIM without panicking.
        let clipped = decode_with(&p2_0, b"#1!999999999@").expect("clipped repeat");
        assert_eq!((clipped.width, clipped.height), (MAX_SIXEL_DIM as u32, 1));
    }
}
