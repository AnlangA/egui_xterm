//! Pure-Rust streaming Sixel (`DCS Pa ; Pb ; Ph q ... ST`) decoder.

use crate::parser::CsiParams;

use super::store::MAX_IMAGE_DIMENSION;

const MAX_SIXEL_DIM: usize = MAX_IMAGE_DIMENSION as usize;
const MAX_SIXEL_PIXELS: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SixelState {
    Normal,
    Raster,
    Color,
    Repeat,
}

/// Streaming Sixel graphics decoder driven by `DCS ... q` `hook`/`put`/`unhook`.
#[derive(Debug)]
pub(crate) struct SixelDecoder {
    state: SixelState,
    params: [u32; 5],
    param_idx: usize,
    has_digit: bool,
    palette: Vec<[u8; 4]>,
    active_color: usize,
    bg_transparent: bool,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    pixels: Vec<[u8; 4]>,
    overflow: bool,
}

impl SixelDecoder {
    /// Creates a new [`SixelDecoder`] from the `DCS` header parameters.
    pub(crate) fn new(dcs_params: &CsiParams) -> Self {
        let pb = dcs_params.get_or(1, 0);
        let bg_transparent = pb != 1;
        let palette = default_vt340_palette();
        Self {
            state: SixelState::Normal,
            params: [0; 5],
            param_idx: 0,
            has_digit: false,
            palette,
            active_color: 0,
            bg_transparent,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            pixels: Vec::new(),
            overflow: false,
        }
    }

    /// Feeds a single raw `DCS` payload byte into the decoder.
    pub(crate) fn put(&mut self, byte: u8) {
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
    pub(crate) fn finish(self) -> Option<(u32, u32, Vec<u8>)> {
        if self.overflow || self.width == 0 || self.height == 0 {
            return None;
        }
        let w = u32::try_from(self.width).ok()?;
        let h = u32::try_from(self.height).ok()?;
        let mut rgba = Vec::with_capacity(self.width * self.height * 4);
        for px in &self.pixels {
            rgba.extend_from_slice(px);
        }
        Some((w, h, rgba))
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

    fn handle_raster(&mut self, byte: u8) {
        if self.push_param_byte(byte) {
            return;
        }
        let ph = usize::try_from(self.params[2]).unwrap_or(0);
        let pv = usize::try_from(self.params[3]).unwrap_or(0);
        if ph > 0 && pv > 0 {
            self.ensure_canvas(ph, pv);
        }
        self.state = SixelState::Normal;
        self.handle_normal(byte);
    }

    fn handle_color(&mut self, byte: u8) {
        if self.push_param_byte(byte) {
            return;
        }
        let reg = usize::try_from(self.params[0]).unwrap_or(0).min(1023);
        if reg >= self.palette.len() {
            self.palette.resize(reg + 1, [255, 255, 255, 255]);
        }
        if self.param_idx >= 1 {
            let pu = self.params[1];
            let px = self.params[2];
            let py = self.params[3];
            let pz = self.params[4];
            let rgb = match pu {
                1 => hls_to_rgb(px, py, pz),
                2 => [pct_to_u8(px), pct_to_u8(py), pct_to_u8(pz)],
                _ => [255, 255, 255],
            };
            self.palette[reg] = [rgb[0], rgb[1], rgb[2], 255];
        }
        self.active_color = reg;
        self.state = SixelState::Normal;
        self.handle_normal(byte);
    }

    fn handle_repeat(&mut self, byte: u8) {
        if byte.is_ascii_digit() {
            let _ = self.push_param_byte(byte);
            return;
        }
        let count = usize::try_from(self.params[0].max(1)).unwrap_or(1);
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
        if new_w == self.width && new_h == self.height {
            return;
        }
        let fill = if self.bg_transparent {
            [0, 0, 0, 0]
        } else {
            self.palette[0]
        };
        let mut next = vec![fill; total];
        for row in 0..self.height {
            let src_start = row * self.width;
            let dst_start = row * new_w;
            next[dst_start..dst_start + self.width]
                .copy_from_slice(&self.pixels[src_start..src_start + self.width]);
        }
        self.pixels = next;
        self.width = new_w;
        self.height = new_h;
    }

    fn draw_sixel(&mut self, bits: u8, repeat: usize) {
        let count = repeat.min(MAX_SIXEL_DIM.saturating_sub(self.x));
        if count == 0 {
            return;
        }
        let color = self
            .palette
            .get(self.active_color)
            .copied()
            .unwrap_or([255, 255, 255, 255]);
        if bits == 0 {
            self.x = self.x.saturating_add(count);
            return;
        }

        let max_bit = (0..6usize).rfind(|&b| (bits & (1 << b)) != 0).unwrap_or(0);
        let req_w = self.x + count;
        let req_h = self.y + max_bit + 1;
        self.ensure_canvas(req_w, req_h);
        if self.overflow {
            return;
        }

        for bit in 0..6usize {
            if (bits & (1 << bit)) != 0 {
                let py = self.y + bit;
                let row_start = py * self.width + self.x;
                self.pixels[row_start..row_start + count].fill(color);
            }
        }
        self.x = req_w;
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

fn default_vt340_palette() -> Vec<[u8; 4]> {
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
