//! Pure-Rust minimal PNG decoder (IHDR, PLTE, tRNS, IDAT, IEND, filters 0..=4).

use super::inflate::{InflateError, inflate_zlib};
use super::store::MAX_IMAGE_DIMENSION;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Decoded RGBA8 image produced by [`decode_png`].
pub(crate) struct DecodedPng {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Errors returned by [`decode_png`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PngError {
    InvalidSignature,
    UnexpectedEof,
    MissingIhdr,
    InvalidIhdr,
    UnsupportedFormat,
    InvalidPalette,
    InvalidTransparency,
    DecompressFailed,
    InvalidFilter,
    DimensionLimitExceeded,
}

impl From<InflateError> for PngError {
    fn from(_: InflateError) -> Self {
        Self::DecompressFailed
    }
}

#[derive(Clone, Copy)]
struct Ihdr {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
}

impl Ihdr {
    const fn channels(self) -> usize {
        match self.color_type {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            6 => 4,
            _ => 0,
        }
    }
}

fn read_u32_be(data: &[u8], pos: usize) -> Result<u32, PngError> {
    let slice = data.get(pos..pos + 4).ok_or(PngError::UnexpectedEof)?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
    let a = i32::from(a);
    let b = i32::from(b);
    let c = i32::from(c);
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        u8::try_from(a).unwrap_or(0)
    } else if pb <= pc {
        u8::try_from(b).unwrap_or(0)
    } else {
        u8::try_from(c).unwrap_or(0)
    }
}

fn unfilter_scanlines(
    raw: &[u8],
    height: usize,
    row_bytes: usize,
    bpp: usize,
) -> Result<Vec<u8>, PngError> {
    let stride = 1 + row_bytes;
    let expected = height
        .checked_mul(stride)
        .ok_or(PngError::DimensionLimitExceeded)?;
    if raw.len() < expected {
        return Err(PngError::DecompressFailed);
    }

    let mut out = vec![0u8; height * row_bytes];
    for y in 0..height {
        let src_row = &raw[y * stride..(y + 1) * stride];
        let filter = src_row[0];
        let src = &src_row[1..];
        let row_start = y * row_bytes;

        for x in 0..row_bytes {
            let filt = src[x];
            let a = if x >= bpp {
                out[row_start + x - bpp]
            } else {
                0
            };
            let b = if y > 0 {
                out[(y - 1) * row_bytes + x]
            } else {
                0
            };
            let c = if y > 0 && x >= bpp {
                out[(y - 1) * row_bytes + x - bpp]
            } else {
                0
            };

            let val = match filter {
                0 => filt,
                1 => filt.wrapping_add(a),
                2 => filt.wrapping_add(b),
                3 => {
                    let avg = u8::try_from((u16::from(a) + u16::from(b)) >> 1).unwrap_or(0);
                    filt.wrapping_add(avg)
                }
                4 => filt.wrapping_add(paeth_predictor(a, b, c)),
                _ => return Err(PngError::InvalidFilter),
            };
            out[row_start + x] = val;
        }
    }
    Ok(out)
}

fn scale_gray_sample(sample: u8, bit_depth: u8) -> u8 {
    match bit_depth {
        1 => {
            if sample != 0 {
                255
            } else {
                0
            }
        }
        2 => sample * 85,
        4 => sample * 17,
        _ => sample,
    }
}

fn extract_subbyte_sample(row: &[u8], x: usize, bit_depth: u8) -> u8 {
    let ppb = 8 / usize::from(bit_depth);
    let byte = row[x / ppb];
    let shift = (ppb - 1 - (x % ppb)) * usize::from(bit_depth);
    let mask = (1u8 << bit_depth) - 1;
    (byte >> shift) & mask
}

/// Decodes a non-interlaced PNG byte slice into `Rgba8` pixels, enforcing `max_rgba_bytes`.
pub(crate) fn decode_png(input: &[u8], max_rgba_bytes: usize) -> Result<DecodedPng, PngError> {
    if input.len() < 8 || input[..8] != PNG_SIGNATURE {
        return Err(PngError::InvalidSignature);
    }

    let mut pos = 8usize;
    let mut ihdr: Option<Ihdr> = None;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns_alpha: Vec<u8> = Vec::new();
    let mut trns_gray: Option<u16> = None;
    let mut trns_rgb: Option<[u16; 3]> = None;
    let mut idat = Vec::new();
    let mut saw_iend = false;

    while pos < input.len() {
        let len = usize::try_from(read_u32_be(input, pos)?).unwrap_or(usize::MAX);
        let kind = input.get(pos + 4..pos + 8).ok_or(PngError::UnexpectedEof)?;
        let data_start = pos + 8;
        let data_end = data_start.checked_add(len).ok_or(PngError::UnexpectedEof)?;
        let crc_end = data_end.checked_add(4).ok_or(PngError::UnexpectedEof)?;
        let data = input
            .get(data_start..data_end)
            .ok_or(PngError::UnexpectedEof)?;
        if crc_end > input.len() {
            return Err(PngError::UnexpectedEof);
        }
        pos = crc_end;

        match kind {
            b"IHDR" => {
                if ihdr.is_some() || data.len() != 13 {
                    return Err(PngError::InvalidIhdr);
                }
                let width = read_u32_be(data, 0)?;
                let height = read_u32_be(data, 4)?;
                let bit_depth = data[8];
                let color_type = data[9];
                let compression = data[10];
                let filter = data[11];
                let interlace = data[12];

                if width == 0
                    || height == 0
                    || width > MAX_IMAGE_DIMENSION
                    || height > MAX_IMAGE_DIMENSION
                {
                    return Err(PngError::DimensionLimitExceeded);
                }
                if compression != 0 || filter != 0 || interlace != 0 {
                    return Err(PngError::UnsupportedFormat);
                }
                let valid = matches!(
                    (color_type, bit_depth),
                    (0, 1 | 2 | 4 | 8 | 16)
                        | (2, 8 | 16)
                        | (3, 1 | 2 | 4 | 8)
                        | (4, 8 | 16)
                        | (6, 8 | 16)
                );
                if !valid {
                    return Err(PngError::UnsupportedFormat);
                }
                ihdr = Some(Ihdr {
                    width,
                    height,
                    bit_depth,
                    color_type,
                });
            }
            b"PLTE" => {
                if data.is_empty() || data.len() % 3 != 0 || data.len() > 256 * 3 {
                    return Err(PngError::InvalidPalette);
                }
                palette.clear();
                for chunk in data.as_chunks::<3>().0 {
                    palette.push([chunk[0], chunk[1], chunk[2]]);
                }
            }
            b"tRNS" => {
                let hdr = ihdr.ok_or(PngError::MissingIhdr)?;
                match hdr.color_type {
                    3 => {
                        if data.len() > 256 {
                            return Err(PngError::InvalidTransparency);
                        }
                        trns_alpha = data.to_vec();
                    }
                    0 => {
                        if data.len() != 2 {
                            return Err(PngError::InvalidTransparency);
                        }
                        trns_gray = Some(u16::from_be_bytes([data[0], data[1]]));
                    }
                    2 => {
                        if data.len() != 6 {
                            return Err(PngError::InvalidTransparency);
                        }
                        trns_rgb = Some([
                            u16::from_be_bytes([data[0], data[1]]),
                            u16::from_be_bytes([data[2], data[3]]),
                            u16::from_be_bytes([data[4], data[5]]),
                        ]);
                    }
                    _ => return Err(PngError::InvalidTransparency),
                }
            }
            b"IDAT" => {
                if ihdr.is_none() {
                    return Err(PngError::MissingIhdr);
                }
                idat.extend_from_slice(data);
            }
            b"IEND" => {
                saw_iend = true;
                break;
            }
            _ => {}
        }
    }

    if !saw_iend {
        return Err(PngError::UnexpectedEof);
    }
    let hdr = ihdr.ok_or(PngError::MissingIhdr)?;
    if hdr.color_type == 3 && palette.is_empty() {
        return Err(PngError::InvalidPalette);
    }

    let w = usize::try_from(hdr.width).map_err(|_| PngError::DimensionLimitExceeded)?;
    let h = usize::try_from(hdr.height).map_err(|_| PngError::DimensionLimitExceeded)?;
    let rgba_len = w
        .checked_mul(h)
        .and_then(|px| px.checked_mul(4))
        .ok_or(PngError::DimensionLimitExceeded)?;
    if rgba_len > max_rgba_bytes {
        return Err(PngError::DimensionLimitExceeded);
    }

    let bits_per_pixel = hdr.channels() * usize::from(hdr.bit_depth);
    let row_bytes = (w * bits_per_pixel).div_ceil(8);
    let bpp = bits_per_pixel.div_ceil(8).max(1);
    let max_raw = h
        .checked_mul(1 + row_bytes)
        .ok_or(PngError::DimensionLimitExceeded)?;

    let raw = inflate_zlib(&idat, max_raw)?;
    let unfiltered = unfilter_scanlines(&raw, h, row_bytes, bpp)?;

    let mut rgba = vec![0u8; rgba_len];
    for y in 0..h {
        let src = &unfiltered[y * row_bytes..(y + 1) * row_bytes];
        let dst_row = &mut rgba[y * w * 4..(y + 1) * w * 4];
        match (hdr.color_type, hdr.bit_depth) {
            (0, 1 | 2 | 4) => {
                for x in 0..w {
                    let raw_val = extract_subbyte_sample(src, x, hdr.bit_depth);
                    let g = scale_gray_sample(raw_val, hdr.bit_depth);
                    let alpha = if trns_gray == Some(u16::from(raw_val)) {
                        0
                    } else {
                        255
                    };
                    dst_row[x * 4..x * 4 + 4].copy_from_slice(&[g, g, g, alpha]);
                }
            }
            (0, 8) => {
                for x in 0..w {
                    let g = src[x];
                    let alpha = if trns_gray == Some(u16::from(g)) {
                        0
                    } else {
                        255
                    };
                    dst_row[x * 4..x * 4 + 4].copy_from_slice(&[g, g, g, alpha]);
                }
            }
            (0, 16) => {
                for x in 0..w {
                    let raw_u16 = u16::from_be_bytes([src[x * 2], src[x * 2 + 1]]);
                    let g = src[x * 2];
                    let alpha = if trns_gray == Some(raw_u16) { 0 } else { 255 };
                    dst_row[x * 4..x * 4 + 4].copy_from_slice(&[g, g, g, alpha]);
                }
            }
            (2, 8) => {
                for x in 0..w {
                    let r = src[x * 3];
                    let g = src[x * 3 + 1];
                    let b = src[x * 3 + 2];
                    let alpha = if trns_rgb == Some([u16::from(r), u16::from(g), u16::from(b)]) {
                        0
                    } else {
                        255
                    };
                    dst_row[x * 4..x * 4 + 4].copy_from_slice(&[r, g, b, alpha]);
                }
            }
            (2, 16) => {
                for x in 0..w {
                    let r16 = u16::from_be_bytes([src[x * 6], src[x * 6 + 1]]);
                    let g16 = u16::from_be_bytes([src[x * 6 + 2], src[x * 6 + 3]]);
                    let b16 = u16::from_be_bytes([src[x * 6 + 4], src[x * 6 + 5]]);
                    let alpha = if trns_rgb == Some([r16, g16, b16]) {
                        0
                    } else {
                        255
                    };
                    dst_row[x * 4..x * 4 + 4].copy_from_slice(&[
                        src[x * 6],
                        src[x * 6 + 2],
                        src[x * 6 + 4],
                        alpha,
                    ]);
                }
            }
            (3, 1 | 2 | 4 | 8) => {
                for x in 0..w {
                    let idx = if hdr.bit_depth == 8 {
                        usize::from(src[x])
                    } else {
                        usize::from(extract_subbyte_sample(src, x, hdr.bit_depth))
                    };
                    let rgb = palette.get(idx).copied().ok_or(PngError::InvalidPalette)?;
                    let alpha = trns_alpha.get(idx).copied().unwrap_or(255);
                    dst_row[x * 4..x * 4 + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
                }
            }
            (4, 8) => {
                for x in 0..w {
                    let g = src[x * 2];
                    let a = src[x * 2 + 1];
                    dst_row[x * 4..x * 4 + 4].copy_from_slice(&[g, g, g, a]);
                }
            }
            (4, 16) => {
                for x in 0..w {
                    let g = src[x * 4];
                    let a = src[x * 4 + 2];
                    dst_row[x * 4..x * 4 + 4].copy_from_slice(&[g, g, g, a]);
                }
            }
            (6, 8) => {
                dst_row.copy_from_slice(&src[..w * 4]);
            }
            (6, 16) => {
                for x in 0..w {
                    dst_row[x * 4..x * 4 + 4].copy_from_slice(&[
                        src[x * 8],
                        src[x * 8 + 2],
                        src[x * 8 + 4],
                        src[x * 8 + 6],
                    ]);
                }
            }
            _ => return Err(PngError::UnsupportedFormat),
        }
    }

    Ok(DecodedPng {
        width: hdr.width,
        height: hdr.height,
        rgba,
    })
}
