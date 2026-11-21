//! Pure-Rust PNG decoder (CRC-32, IHDR, PLTE, tRNS, IDAT, IEND, filters 0..=4, Adam7).

use super::inflate::{InflateError, inflate_zlib};
use super::store::MAX_IMAGE_DIMENSION;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Maximum legal PNG chunk data length per W3C PNG spec (`2^31 - 1`).
const MAX_PNG_CHUNK_LEN: u32 = 0x7FFF_FFFF;

/// Slack added on top of `2 * max_rgba_bytes` for the pre-inflate `IDAT` cap so
/// small images with zlib framing overhead never hit the pre-inflate limit.
const IDAT_CAP_SLACK: usize = 1024 * 1024;

/// Adam7 interlacing passes: `(x0, y0, x_step, y_step)`.
const ADAM7_PASSES: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

/// IEEE 802.3 CRC-32 lookup table (reflected polynomial `0xEDB88320`).
const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        let mut crc = i;
        let mut bit = 0;
        while bit < 8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
            bit += 1;
        }
        table[i as usize] = crc;
        i += 1;
    }
    table
};

/// Decoded RGBA8 image produced by [`decode_png`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPng {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Tightly packed row-major `RGBA8` pixel buffer (`width * height * 4` bytes).
    pub rgba: Vec<u8>,
}

/// Errors returned by [`decode_png`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PngError {
    /// Input does not begin with the 8-byte PNG signature.
    InvalidSignature,
    /// Input ended before completing a chunk or before `IEND`.
    UnexpectedEof,
    /// A chunk's IEEE 802.3 CRC-32 did not match `chunk_type || chunk_data`.
    InvalidCrc,
    /// Malformed chunk framing, chunk type bytes, or non-empty `IEND`.
    InvalidChunk,
    /// Chunk violated PNG ordering or multiplicity constraints.
    InvalidChunkOrder,
    /// Encountered an unrecognized critical chunk.
    UnsupportedCriticalChunk,
    /// `IHDR` chunk was missing.
    MissingIhdr,
    /// `IHDR` chunk payload had invalid length or zero dimensions.
    InvalidIhdr,
    /// Unsupported color type, bit depth, compression, filter, or interlace method.
    UnsupportedFormat,
    /// Invalid, forbidden, or missing `PLTE` chunk, or out-of-range palette index.
    InvalidPalette,
    /// Invalid or forbidden `tRNS` chunk.
    InvalidTransparency,
    /// No `IDAT` chunk appeared before `IEND`.
    MissingIdat,
    /// Compressed `IDAT` payload exceeded the pre-inflate byte budget.
    IdatLimitExceeded,
    /// Zlib decompression or uncompressed stream length validation failed.
    DecompressFailed,
    /// Scanline used an unknown PNG filter type (`> 4`).
    InvalidFilter,
    /// Image dimensions or decoded RGBA buffer exceeded configured limits.
    DimensionLimitExceeded,
}

impl std::fmt::Display for PngError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::InvalidSignature => "invalid PNG signature",
            Self::UnexpectedEof => "unexpected end of PNG input",
            Self::InvalidCrc => "invalid PNG chunk CRC-32",
            Self::InvalidChunk => "malformed PNG chunk framing",
            Self::InvalidChunkOrder => "invalid PNG chunk ordering",
            Self::UnsupportedCriticalChunk => "unsupported critical PNG chunk",
            Self::MissingIhdr => "missing PNG IHDR chunk",
            Self::InvalidIhdr => "invalid PNG IHDR chunk",
            Self::UnsupportedFormat => "unsupported PNG format",
            Self::InvalidPalette => "invalid PNG palette",
            Self::InvalidTransparency => "invalid PNG transparency chunk",
            Self::MissingIdat => "missing PNG IDAT chunk",
            Self::IdatLimitExceeded => "PNG IDAT payload exceeds size limit",
            Self::DecompressFailed => "PNG zlib decompression failed",
            Self::InvalidFilter => "invalid PNG scanline filter",
            Self::DimensionLimitExceeded => "PNG dimensions exceed configured budget",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for PngError {}

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
    interlace: u8,
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

    const fn bits_per_pixel(self) -> usize {
        self.channels() * (self.bit_depth as usize)
    }

    const fn filter_bpp(self) -> usize {
        let bytes = self.bits_per_pixel().div_ceil(8);
        if bytes == 0 { 1 } else { bytes }
    }
}

#[derive(Default)]
struct TrnsData {
    alpha: Vec<u8>,
    gray: Option<u16>,
    rgb: Option<[u16; 3]>,
}

struct ParsedChunks {
    ihdr: Ihdr,
    palette: Vec<[u8; 3]>,
    trns: TrnsData,
    idat: Vec<u8>,
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        let idx = usize::from(u8::try_from((crc ^ u32::from(b)) & 0xFF).unwrap_or(0));
        crc = (crc >> 8) ^ CRC32_TABLE[idx];
    }
    crc ^ 0xFFFF_FFFF
}

fn read_u32_be(data: &[u8], pos: usize) -> Result<u32, PngError> {
    let end = pos.checked_add(4).ok_or(PngError::UnexpectedEof)?;
    let slice = data.get(pos..end).ok_or(PngError::UnexpectedEof)?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn parse_ihdr(data: &[u8], max_rgba_bytes: usize) -> Result<Ihdr, PngError> {
    if data.len() != 13 {
        return Err(PngError::InvalidIhdr);
    }
    let width = read_u32_be(data, 0)?;
    let height = read_u32_be(data, 4)?;
    let bit_depth = data[8];
    let color_type = data[9];
    let compression = data[10];
    let filter = data[11];
    let interlace = data[12];

    if width == 0 || height == 0 {
        return Err(PngError::InvalidIhdr);
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(PngError::DimensionLimitExceeded);
    }
    let w = usize::try_from(width).map_err(|_| PngError::DimensionLimitExceeded)?;
    let h = usize::try_from(height).map_err(|_| PngError::DimensionLimitExceeded)?;
    let rgba_len = w
        .checked_mul(h)
        .and_then(|px| px.checked_mul(4))
        .ok_or(PngError::DimensionLimitExceeded)?;
    if rgba_len > max_rgba_bytes {
        return Err(PngError::DimensionLimitExceeded);
    }

    if compression != 0 || filter != 0 || (interlace != 0 && interlace != 1) {
        return Err(PngError::UnsupportedFormat);
    }
    let valid = matches!(
        (color_type, bit_depth),
        (0, 1 | 2 | 4 | 8 | 16) | (2, 8 | 16) | (3, 1 | 2 | 4 | 8) | (4, 8 | 16) | (6, 8 | 16)
    );
    if !valid {
        return Err(PngError::UnsupportedFormat);
    }

    Ok(Ihdr {
        width,
        height,
        bit_depth,
        color_type,
        interlace,
    })
}

fn parse_chunks(input: &[u8], max_rgba_bytes: usize) -> Result<ParsedChunks, PngError> {
    if input.len() < 8 || input[..8] != PNG_SIGNATURE {
        return Err(PngError::InvalidSignature);
    }

    let max_idat_bytes = max_rgba_bytes
        .saturating_mul(2)
        .saturating_add(IDAT_CAP_SLACK);

    let mut pos = 8usize;
    let mut ihdr: Option<Ihdr> = None;
    let mut saw_pre_ihdr_chunk = false;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut saw_plte = false;
    let mut trns = TrnsData::default();
    let mut saw_trns = false;
    let mut idat = Vec::new();
    let mut saw_idat = false;
    let mut idat_finished = false;
    let mut saw_iend = false;

    while pos < input.len() {
        let len_u32 = read_u32_be(input, pos)?;
        if len_u32 > MAX_PNG_CHUNK_LEN {
            return Err(PngError::InvalidChunk);
        }
        let len = usize::try_from(len_u32).map_err(|_| PngError::InvalidChunk)?;
        let type_start = pos.checked_add(4).ok_or(PngError::UnexpectedEof)?;
        let data_start = pos.checked_add(8).ok_or(PngError::UnexpectedEof)?;
        let kind_slice = input
            .get(type_start..data_start)
            .ok_or(PngError::UnexpectedEof)?;
        let kind = [kind_slice[0], kind_slice[1], kind_slice[2], kind_slice[3]];
        let data_end = data_start.checked_add(len).ok_or(PngError::UnexpectedEof)?;
        let crc_end = data_end.checked_add(4).ok_or(PngError::UnexpectedEof)?;
        if crc_end > input.len() {
            return Err(PngError::UnexpectedEof);
        }

        let expected_crc = read_u32_be(input, data_end)?;
        if crc32(&input[type_start..data_end]) != expected_crc {
            return Err(PngError::InvalidCrc);
        }
        if !kind.iter().all(u8::is_ascii_alphabetic) {
            return Err(PngError::InvalidChunk);
        }

        let data = &input[data_start..data_end];
        pos = crc_end;

        let Some(hdr) = ihdr else {
            match &kind {
                b"IHDR" => {
                    if saw_pre_ihdr_chunk {
                        return Err(PngError::InvalidChunkOrder);
                    }
                    ihdr = Some(parse_ihdr(data, max_rgba_bytes)?);
                }
                b"IEND" | b"IDAT" | b"tRNS" => return Err(PngError::MissingIhdr),
                b"PLTE" => return Err(PngError::InvalidChunkOrder),
                _ => {
                    if (kind[0] & 0x20) == 0 {
                        return Err(PngError::UnsupportedCriticalChunk);
                    }
                    saw_pre_ihdr_chunk = true;
                }
            }
            continue;
        };

        match &kind {
            b"IHDR" => return Err(PngError::InvalidChunkOrder),
            b"PLTE" => {
                if hdr.color_type == 0 || hdr.color_type == 4 {
                    return Err(PngError::InvalidPalette);
                }
                if saw_plte || saw_trns || saw_idat {
                    return Err(PngError::InvalidChunkOrder);
                }
                if data.is_empty() || !data.len().is_multiple_of(3) || data.len() > 256 * 3 {
                    return Err(PngError::InvalidPalette);
                }
                for chunk in data.as_chunks::<3>().0 {
                    palette.push([chunk[0], chunk[1], chunk[2]]);
                }
                saw_plte = true;
            }
            b"tRNS" => {
                if saw_trns || saw_idat {
                    return Err(PngError::InvalidChunkOrder);
                }
                match hdr.color_type {
                    3 => {
                        if !saw_plte {
                            return Err(PngError::InvalidChunkOrder);
                        }
                        if data.len() > palette.len() {
                            return Err(PngError::InvalidTransparency);
                        }
                        trns.alpha = data.to_vec();
                    }
                    0 => {
                        if data.len() != 2 {
                            return Err(PngError::InvalidTransparency);
                        }
                        trns.gray = Some(u16::from_be_bytes([data[0], data[1]]));
                    }
                    2 => {
                        if data.len() != 6 {
                            return Err(PngError::InvalidTransparency);
                        }
                        trns.rgb = Some([
                            u16::from_be_bytes([data[0], data[1]]),
                            u16::from_be_bytes([data[2], data[3]]),
                            u16::from_be_bytes([data[4], data[5]]),
                        ]);
                    }
                    _ => return Err(PngError::InvalidTransparency),
                }
                saw_trns = true;
            }
            b"IDAT" => {
                if idat_finished {
                    return Err(PngError::InvalidChunkOrder);
                }
                if idat.len().saturating_add(data.len()) > max_idat_bytes {
                    return Err(PngError::IdatLimitExceeded);
                }
                idat.extend_from_slice(data);
                saw_idat = true;
            }
            b"IEND" => {
                if !data.is_empty() {
                    return Err(PngError::InvalidChunk);
                }
                saw_iend = true;
                break;
            }
            _ => {
                if (kind[0] & 0x20) == 0 {
                    return Err(PngError::UnsupportedCriticalChunk);
                }
                if saw_idat {
                    idat_finished = true;
                }
            }
        }
    }

    if !saw_iend {
        return Err(PngError::UnexpectedEof);
    }
    let hdr = ihdr.ok_or(PngError::MissingIhdr)?;
    if hdr.color_type == 3 && !saw_plte {
        return Err(PngError::InvalidPalette);
    }
    if !saw_idat {
        return Err(PngError::MissingIdat);
    }

    Ok(ParsedChunks {
        ihdr: hdr,
        palette,
        trns,
        idat,
    })
}

fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
    let ai = i32::from(a);
    let bi = i32::from(b);
    let ci = i32::from(c);
    let p = ai + bi - ci;
    let pa = (p - ai).abs();
    let pb = (p - bi).abs();
    let pc = (p - ci).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
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
    let out_len = height
        .checked_mul(row_bytes)
        .ok_or(PngError::DimensionLimitExceeded)?;

    let mut out = vec![0u8; out_len];
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

const fn scale_gray_sample(sample: u8, bit_depth: u8) -> u8 {
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

const fn pass_dims(
    w: usize,
    h: usize,
    x0: usize,
    y0: usize,
    x_step: usize,
    y_step: usize,
) -> (usize, usize) {
    let pw = if w <= x0 {
        0
    } else {
        (w - x0).div_ceil(x_step)
    };
    let ph = if h <= y0 {
        0
    } else {
        (h - y0).div_ceil(y_step)
    };
    (pw, ph)
}

fn expected_raw_len(
    w: usize,
    h: usize,
    bits_per_pixel: usize,
    passes: &[(usize, usize, usize, usize)],
) -> Result<usize, PngError> {
    let mut total = 0usize;
    for &(x0, y0, x_step, y_step) in passes {
        let (pw, ph) = pass_dims(w, h, x0, y0, x_step, y_step);
        if pw == 0 || ph == 0 {
            continue;
        }
        let pass_row_bytes = pw
            .checked_mul(bits_per_pixel)
            .ok_or(PngError::DimensionLimitExceeded)?
            .div_ceil(8);
        let pass_bytes = ph
            .checked_mul(1 + pass_row_bytes)
            .ok_or(PngError::DimensionLimitExceeded)?;
        total = total
            .checked_add(pass_bytes)
            .ok_or(PngError::DimensionLimitExceeded)?;
    }
    Ok(total)
}

struct ScanlineTarget<'a> {
    row: &'a mut [u8],
    x0: usize,
    x_step: usize,
}

fn decode_scanline_into_rgba(
    hdr: Ihdr,
    src: &[u8],
    pw: usize,
    palette: &[[u8; 3]],
    trns: &TrnsData,
    dst: ScanlineTarget<'_>,
) -> Result<(), PngError> {
    let ScanlineTarget {
        row: dst_row,
        x0,
        x_step,
    } = dst;
    match (hdr.color_type, hdr.bit_depth) {
        (0, 1 | 2 | 4) => {
            for px in 0..pw {
                let raw_val = extract_subbyte_sample(src, px, hdr.bit_depth);
                let g = scale_gray_sample(raw_val, hdr.bit_depth);
                let alpha = if trns.gray == Some(u16::from(raw_val)) {
                    0
                } else {
                    255
                };
                let dst_x = x0 + px * x_step;
                dst_row[dst_x * 4..dst_x * 4 + 4].copy_from_slice(&[g, g, g, alpha]);
            }
        }
        (0, 8) => {
            for (px, &g) in src.iter().take(pw).enumerate() {
                let alpha = if trns.gray == Some(u16::from(g)) {
                    0
                } else {
                    255
                };
                let dst_x = x0 + px * x_step;
                dst_row[dst_x * 4..dst_x * 4 + 4].copy_from_slice(&[g, g, g, alpha]);
            }
        }
        (0, 16) => {
            for px in 0..pw {
                let raw_u16 = u16::from_be_bytes([src[px * 2], src[px * 2 + 1]]);
                let g = src[px * 2];
                let alpha = if trns.gray == Some(raw_u16) { 0 } else { 255 };
                let dst_x = x0 + px * x_step;
                dst_row[dst_x * 4..dst_x * 4 + 4].copy_from_slice(&[g, g, g, alpha]);
            }
        }
        (2, 8) => {
            for px in 0..pw {
                let r = src[px * 3];
                let g = src[px * 3 + 1];
                let b = src[px * 3 + 2];
                let alpha = if trns.rgb == Some([u16::from(r), u16::from(g), u16::from(b)]) {
                    0
                } else {
                    255
                };
                let dst_x = x0 + px * x_step;
                dst_row[dst_x * 4..dst_x * 4 + 4].copy_from_slice(&[r, g, b, alpha]);
            }
        }
        (2, 16) => {
            for px in 0..pw {
                let r16 = u16::from_be_bytes([src[px * 6], src[px * 6 + 1]]);
                let g16 = u16::from_be_bytes([src[px * 6 + 2], src[px * 6 + 3]]);
                let b16 = u16::from_be_bytes([src[px * 6 + 4], src[px * 6 + 5]]);
                let alpha = if trns.rgb == Some([r16, g16, b16]) {
                    0
                } else {
                    255
                };
                let dst_x = x0 + px * x_step;
                dst_row[dst_x * 4..dst_x * 4 + 4].copy_from_slice(&[
                    src[px * 6],
                    src[px * 6 + 2],
                    src[px * 6 + 4],
                    alpha,
                ]);
            }
        }
        (3, 1 | 2 | 4 | 8) => {
            for px in 0..pw {
                let idx = if hdr.bit_depth == 8 {
                    usize::from(src[px])
                } else {
                    usize::from(extract_subbyte_sample(src, px, hdr.bit_depth))
                };
                let rgb = palette.get(idx).copied().ok_or(PngError::InvalidPalette)?;
                let alpha = trns.alpha.get(idx).copied().unwrap_or(255);
                let dst_x = x0 + px * x_step;
                dst_row[dst_x * 4..dst_x * 4 + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
            }
        }
        (4, 8) => {
            for px in 0..pw {
                let g = src[px * 2];
                let a = src[px * 2 + 1];
                let dst_x = x0 + px * x_step;
                dst_row[dst_x * 4..dst_x * 4 + 4].copy_from_slice(&[g, g, g, a]);
            }
        }
        (4, 16) => {
            for px in 0..pw {
                let g = src[px * 4];
                let a = src[px * 4 + 2];
                let dst_x = x0 + px * x_step;
                dst_row[dst_x * 4..dst_x * 4 + 4].copy_from_slice(&[g, g, g, a]);
            }
        }
        (6, 8) => {
            if x0 == 0 && x_step == 1 {
                dst_row.copy_from_slice(&src[..pw * 4]);
            } else {
                for px in 0..pw {
                    let dst_x = x0 + px * x_step;
                    dst_row[dst_x * 4..dst_x * 4 + 4].copy_from_slice(&src[px * 4..px * 4 + 4]);
                }
            }
        }
        (6, 16) => {
            for px in 0..pw {
                let dst_x = x0 + px * x_step;
                dst_row[dst_x * 4..dst_x * 4 + 4].copy_from_slice(&[
                    src[px * 8],
                    src[px * 8 + 2],
                    src[px * 8 + 4],
                    src[px * 8 + 6],
                ]);
            }
        }
        _ => return Err(PngError::UnsupportedFormat),
    }
    Ok(())
}

/// Decodes a PNG byte slice (non-interlaced or Adam7 interlaced) into `Rgba8` pixels,
/// validating chunk CRCs, chunk ordering, and enforcing `max_rgba_bytes`.
///
/// # Errors
///
/// Returns a [`PngError`] when the signature, chunk framing, IEEE 802.3 CRC-32, chunk
/// ordering, zlib stream, scanline filter, or `max_rgba_bytes` budget check fails.
///
/// # Examples
///
/// Decode a valid `1x1` RGBA8 PNG and reject corrupted CRCs, truncated streams,
/// invalid signatures, and oversized pixel budgets:
///
/// ```
/// use egui_xterm_core::features::inline_images::png::{PngError, decode_png};
///
/// // Minimal valid 1x1 RGBA8 PNG with pixel [10, 20, 30, 255].
/// let valid_png: [u8; 70] = [
///     0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
///     0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
///     0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
///     0x9C, 0x63, 0xE0, 0x12, 0x91, 0xFB, 0x0F, 0x00, 0x01, 0xA4, 0x01, 0x3C, 0x93, 0x8B,
///     0x0E, 0xB7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
/// ];
///
/// let decoded = decode_png(&valid_png, 1024)?;
/// assert_eq!((decoded.width, decoded.height), (1, 1));
/// assert_eq!(decoded.rgba, [10, 20, 30, 255]);
///
/// // Corrupting a byte inside IHDR's IEEE 802.3 CRC-32 fails with InvalidCrc.
/// let mut bad_crc = valid_png;
/// bad_crc[29] ^= 0x01;
/// assert_eq!(decode_png(&bad_crc, 1024), Err(PngError::InvalidCrc));
///
/// // Truncating before IEND completes fails with UnexpectedEof.
/// assert_eq!(
///     decode_png(&valid_png[..valid_png.len() - 4], 1024),
///     Err(PngError::UnexpectedEof)
/// );
///
/// // Enforcing max_rgba_bytes rejects images whose RGBA8 buffer exceeds the cap.
/// assert_eq!(
///     decode_png(&valid_png, 3),
///     Err(PngError::DimensionLimitExceeded)
/// );
/// assert_eq!(decode_png(b"not a png", 1024), Err(PngError::InvalidSignature));
/// # Ok::<(), PngError>(())
/// ```
pub fn decode_png(input: &[u8], max_rgba_bytes: usize) -> Result<DecodedPng, PngError> {
    let ParsedChunks {
        ihdr,
        palette,
        trns,
        idat,
    } = parse_chunks(input, max_rgba_bytes)?;

    let w = usize::try_from(ihdr.width).map_err(|_| PngError::DimensionLimitExceeded)?;
    let h = usize::try_from(ihdr.height).map_err(|_| PngError::DimensionLimitExceeded)?;
    let rgba_len = w
        .checked_mul(h)
        .and_then(|px| px.checked_mul(4))
        .ok_or(PngError::DimensionLimitExceeded)?;
    if rgba_len > max_rgba_bytes {
        return Err(PngError::DimensionLimitExceeded);
    }

    let bits_per_pixel = ihdr.bits_per_pixel();
    let bpp = ihdr.filter_bpp();
    let passes: &[(usize, usize, usize, usize)] = if ihdr.interlace == 1 {
        &ADAM7_PASSES
    } else {
        &[(0, 0, 1, 1)]
    };

    let expected_raw = expected_raw_len(w, h, bits_per_pixel, passes)?;
    let raw = inflate_zlib(&idat, expected_raw)?;
    if raw.len() != expected_raw {
        return Err(PngError::DecompressFailed);
    }

    let mut rgba = vec![0u8; rgba_len];
    let mut raw_offset = 0usize;

    for &(x0, y0, x_step, y_step) in passes {
        let (pw, ph) = pass_dims(w, h, x0, y0, x_step, y_step);
        if pw == 0 || ph == 0 {
            continue;
        }
        let pass_row_bytes = pw
            .checked_mul(bits_per_pixel)
            .ok_or(PngError::DimensionLimitExceeded)?
            .div_ceil(8);
        let pass_raw_len = ph
            .checked_mul(1 + pass_row_bytes)
            .ok_or(PngError::DimensionLimitExceeded)?;
        let pass_raw = raw
            .get(raw_offset..raw_offset + pass_raw_len)
            .ok_or(PngError::DecompressFailed)?;
        raw_offset += pass_raw_len;

        let unfiltered = unfilter_scanlines(pass_raw, ph, pass_row_bytes, bpp)?;
        for py in 0..ph {
            let src = &unfiltered[py * pass_row_bytes..(py + 1) * pass_row_bytes];
            let dst_y = y0 + py * y_step;
            let dst_row = &mut rgba[dst_y * w * 4..(dst_y + 1) * w * 4];
            decode_scanline_into_rgba(
                ihdr,
                src,
                pw,
                &palette,
                &trns,
                ScanlineTarget {
                    row: dst_row,
                    x0,
                    x_step,
                },
            )?;
        }
    }

    Ok(DecodedPng {
        width: ihdr.width,
        height: ihdr.height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(0x78);
        out.push(0x01);

        let mut pos = 0;
        while pos < data.len() || pos == 0 {
            let end = (pos + 65535).min(data.len());
            let is_final = end == data.len();
            out.push(u8::from(is_final));
            let len = u16::try_from(end - pos).unwrap();
            let nlen = !len;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&nlen.to_le_bytes());
            out.extend_from_slice(&data[pos..end]);
            pos = end;
            if pos == data.len() {
                break;
            }
        }

        let mut s1 = 1u32;
        let mut s2 = 0u32;
        for &b in data {
            s1 = (s1 + u32::from(b)) % 65521;
            s2 = (s2 + s1) % 65521;
        }
        let adler = (s2 << 16) | s1;
        out.extend_from_slice(&adler.to_be_bytes());
        out
    }

    fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        let len = u32::try_from(data.len()).unwrap();
        out.extend_from_slice(&len.to_be_bytes());
        let crc_start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[crc_start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }

    fn make_ihdr(
        width: u32,
        height: u32,
        bit_depth: u8,
        color_type: u8,
        interlace: u8,
    ) -> [u8; 13] {
        let mut buf = [0u8; 13];
        buf[0..4].copy_from_slice(&width.to_be_bytes());
        buf[4..8].copy_from_slice(&height.to_be_bytes());
        buf[8] = bit_depth;
        buf[9] = color_type;
        buf[10] = 0;
        buf[11] = 0;
        buf[12] = interlace;
        buf
    }

    fn build_png(
        ihdr: [u8; 13],
        plte: Option<&[u8]>,
        trns: Option<&[u8]>,
        raw_scanlines: &[u8],
    ) -> Vec<u8> {
        let mut out = PNG_SIGNATURE.to_vec();
        push_chunk(&mut out, b"IHDR", &ihdr);
        if let Some(p) = plte {
            push_chunk(&mut out, b"PLTE", p);
        }
        if let Some(t) = trns {
            push_chunk(&mut out, b"tRNS", t);
        }
        let idat = zlib_stored(raw_scanlines);
        push_chunk(&mut out, b"IDAT", &idat);
        push_chunk(&mut out, b"IEND", &[]);
        out
    }

    fn build_png_from_chunks(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut out = PNG_SIGNATURE.to_vec();
        for &(kind, data) in chunks {
            push_chunk(&mut out, kind, data);
        }
        out
    }

    /// Encodes a full image of raw sample arrays (each `x, y` holds raw packed bytes or
    /// sub-byte sample value) into Adam7 filtered scanline stream.
    fn encode_adam7_filtered(
        w: usize,
        h: usize,
        color_type: u8,
        bit_depth: u8,
        sample_at: impl Fn(usize, usize) -> Vec<u8>,
        filter_for_row: impl Fn(usize, usize) -> u8,
    ) -> Vec<u8> {
        let channels = match color_type {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            6 => 4,
            _ => unreachable!(),
        };
        let bpp_bits = channels * usize::from(bit_depth);
        let bpp_bytes = bpp_bits.div_ceil(8).max(1);
        let mut out = Vec::new();

        for (pass_idx, &(x0, y0, x_step, y_step)) in ADAM7_PASSES.iter().enumerate() {
            let (pw, ph) = pass_dims(w, h, x0, y0, x_step, y_step);
            if pw == 0 || ph == 0 {
                continue;
            }
            let row_bytes = (pw * bpp_bits).div_ceil(8);
            let mut unfiltered = vec![0u8; ph * row_bytes];
            for py in 0..ph {
                let dst_y = y0 + py * y_step;
                let row_off = py * row_bytes;
                for px in 0..pw {
                    let dst_x = x0 + px * x_step;
                    let s = sample_at(dst_x, dst_y);
                    if bit_depth < 8 {
                        let ppb = 8 / usize::from(bit_depth);
                        let shift = (ppb - 1 - (px % ppb)) * usize::from(bit_depth);
                        unfiltered[row_off + px / ppb] |= s[0] << shift;
                    } else {
                        let byte_per_px = bpp_bits / 8;
                        unfiltered[row_off + px * byte_per_px..row_off + (px + 1) * byte_per_px]
                            .copy_from_slice(&s);
                    }
                }
            }

            for py in 0..ph {
                let filter = filter_for_row(pass_idx, py);
                out.push(filter);
                let row_start = py * row_bytes;
                for x in 0..row_bytes {
                    let cur = unfiltered[row_start + x];
                    let a = if x >= bpp_bytes {
                        unfiltered[row_start + x - bpp_bytes]
                    } else {
                        0
                    };
                    let b = if py > 0 {
                        unfiltered[(py - 1) * row_bytes + x]
                    } else {
                        0
                    };
                    let c = if py > 0 && x >= bpp_bytes {
                        unfiltered[(py - 1) * row_bytes + x - bpp_bytes]
                    } else {
                        0
                    };
                    let filt = match filter {
                        0 => cur,
                        1 => cur.wrapping_sub(a),
                        2 => cur.wrapping_sub(b),
                        3 => cur.wrapping_sub(((u16::from(a) + u16::from(b)) >> 1) as u8),
                        4 => cur.wrapping_sub(paeth_predictor(a, b, c)),
                        _ => unreachable!(),
                    };
                    out.push(filt);
                }
            }
        }
        out
    }

    #[test]
    fn crc32_matches_ieee_vectors() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    #[test]
    fn rejects_invalid_crc_on_any_chunk() {
        let valid = build_png(
            make_ihdr(2, 1, 8, 3, 0),
            Some(&[10, 20, 30, 40, 50, 60]),
            Some(&[128]),
            &[0, 0, 1],
        );
        assert!(decode_png(&valid, 1024).is_ok());

        // Corrupt IHDR CRC (offset: 8 sig + 4 len + 4 kind + 13 data = 29).
        let mut bad_ihdr_crc = valid.clone();
        bad_ihdr_crc[29] ^= 0x01;
        assert_eq!(
            decode_png(&bad_ihdr_crc, 1024).err(),
            Some(PngError::InvalidCrc)
        );

        // Corrupt a byte inside IHDR payload without updating CRC.
        let mut bad_ihdr_data = valid.clone();
        bad_ihdr_data[16] ^= 0x80;
        assert_eq!(
            decode_png(&bad_ihdr_data, 1024).err(),
            Some(PngError::InvalidCrc)
        );

        // Corrupt a byte in IEND CRC (last byte).
        let mut bad_iend_crc = valid.clone();
        let last = bad_iend_crc.len() - 1;
        bad_iend_crc[last] ^= 0xFF;
        assert_eq!(
            decode_png(&bad_iend_crc, 1024).err(),
            Some(PngError::InvalidCrc)
        );

        // Corrupt a byte inside an ancillary chunk.
        let ihdr = make_ihdr(1, 1, 8, 6, 0);
        let idat = zlib_stored(&[0, 1, 2, 3, 4]);
        let mut with_text = build_png_from_chunks(&[
            (b"IHDR", &ihdr),
            (b"tEXt", b"Title\0Test"),
            (b"IDAT", &idat),
            (b"IEND", &[]),
        ]);
        // tEXt starts at 8 + (12 + 13) = 33; payload byte at 33 + 8 = 41.
        with_text[41] ^= 0x02;
        assert_eq!(
            decode_png(&with_text, 1024).err(),
            Some(PngError::InvalidCrc)
        );
    }

    #[test]
    fn decodes_sub_byte_1_2_4_bit_grayscale_and_indexed_with_trns() {
        // 1-bit grayscale 3x2:
        // row 0: [1, 0, 1] -> 0b101_00000 = 0xA0 (tRNS = 0 -> pixel 0 opaque 255, pixel 1 transparent 0)
        // row 1: [0, 1, 0] -> 0b010_00000 = 0x40
        let g1 = build_png(
            make_ihdr(3, 2, 1, 0, 0),
            None,
            Some(&[0, 0]),
            &[0, 0xA0, 0, 0x40],
        );
        let dec1 = decode_png(&g1, 1024).unwrap();
        assert_eq!(
            dec1.rgba,
            vec![
                255, 255, 255, 255, 0, 0, 0, 0, 255, 255, 255, 255, // row 0
                0, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 0, // row 1
            ]
        );

        // 2-bit grayscale 5x1: [0, 1, 2, 3, 1] -> [0b00_01_10_11 = 0x1B, 0b01_00_00_00 = 0x40], tRNS=2
        let g2 = build_png(
            make_ihdr(5, 1, 2, 0, 0),
            None,
            Some(&[0, 2]),
            &[0, 0x1B, 0x40],
        );
        let dec2 = decode_png(&g2, 1024).unwrap();
        assert_eq!(
            dec2.rgba,
            vec![
                0, 0, 0, 255, // 0
                85, 85, 85, 255, // 1
                170, 170, 170, 0, // 2 (transparent)
                255, 255, 255, 255, // 3
                85, 85, 85, 255, // 1
            ]
        );

        // 4-bit grayscale 3x1: [0, 7, 15] -> [0x07, 0xF0], tRNS=15
        let g4 = build_png(
            make_ihdr(3, 1, 4, 0, 0),
            None,
            Some(&[0, 15]),
            &[0, 0x07, 0xF0],
        );
        let dec4 = decode_png(&g4, 1024).unwrap();
        assert_eq!(
            dec4.rgba,
            vec![
                0, 0, 0, 255, // 0
                119, 119, 119, 255, // 7 * 17
                255, 255, 255, 0, // 15 * 17, transparent
            ]
        );

        // 4-bit indexed 3x1 with partial tRNS (only entry 0 has alpha=64; entries 1,2 default 255)
        let plte = [255, 0, 0, 0, 255, 0, 0, 0, 255];
        let i4 = build_png(
            make_ihdr(3, 1, 4, 3, 0),
            Some(&plte),
            Some(&[64]),
            &[0, 0x01, 0x20],
        );
        let dec_i4 = decode_png(&i4, 1024).unwrap();
        assert_eq!(
            dec_i4.rgba,
            vec![255, 0, 0, 64, 0, 255, 0, 255, 0, 0, 255, 255]
        );

        // 1-bit & 2-bit indexed ([0, 1] -> 0b01_000000 = 0x40)
        let i1 = build_png(
            make_ihdr(2, 1, 1, 3, 0),
            Some(&plte[..6]),
            Some(&[10]),
            &[0, 0x40],
        );
        assert_eq!(
            decode_png(&i1, 1024).unwrap().rgba,
            vec![255, 0, 0, 10, 0, 255, 0, 255]
        );
        let i2 = build_png(
            make_ihdr(3, 1, 2, 3, 0),
            Some(&plte),
            None,
            &[0, 0b00_01_10_00],
        );
        assert_eq!(
            decode_png(&i2, 1024).unwrap().rgba,
            vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255]
        );
    }

    #[test]
    fn decodes_16_bit_all_color_types_and_exact_trns_match() {
        // 16-bit grayscale with tRNS=0x1234: 0x1234 -> transparent, 0x1235 -> opaque (same high byte 0x12!)
        let g16 = build_png(
            make_ihdr(2, 1, 16, 0, 0),
            None,
            Some(&[0x12, 0x34]),
            &[0, 0x12, 0x34, 0x12, 0x35],
        );
        assert_eq!(
            decode_png(&g16, 1024).unwrap().rgba,
            vec![0x12, 0x12, 0x12, 0, 0x12, 0x12, 0x12, 255]
        );

        // 16-bit RGB with tRNS=(0x1100, 0x2200, 0x3300): second pixel differs only in LSB of B
        let rgb16 = build_png(
            make_ihdr(2, 1, 16, 2, 0),
            None,
            Some(&[0x11, 0x00, 0x22, 0x00, 0x33, 0x00]),
            &[
                0, 0x11, 0x00, 0x22, 0x00, 0x33, 0x00, // px 0 -> transparent
                0x11, 0x00, 0x22, 0x00, 0x33, 0x01, // px 1 -> opaque
            ],
        );
        assert_eq!(
            decode_png(&rgb16, 1024).unwrap().rgba,
            vec![0x11, 0x22, 0x33, 0, 0x11, 0x22, 0x33, 255]
        );

        // 8-bit & 16-bit grayscale + alpha (color_type = 4)
        let ga8 = build_png(make_ihdr(2, 1, 8, 4, 0), None, None, &[0, 50, 200, 120, 40]);
        assert_eq!(
            decode_png(&ga8, 1024).unwrap().rgba,
            vec![50, 50, 50, 200, 120, 120, 120, 40]
        );
        let ga16 = build_png(
            make_ihdr(1, 1, 16, 4, 0),
            None,
            None,
            &[0, 0xAA, 0x11, 0x77, 0x99],
        );
        assert_eq!(
            decode_png(&ga16, 1024).unwrap().rgba,
            vec![0xAA, 0xAA, 0xAA, 0x77]
        );

        // 16-bit RGBA (color_type = 6)
        let rgba16 = build_png(
            make_ihdr(1, 1, 16, 6, 0),
            None,
            None,
            &[0, 0x10, 0xFF, 0x20, 0xEE, 0x30, 0xDD, 0x40, 0xCC],
        );
        assert_eq!(
            decode_png(&rgba16, 1024).unwrap().rgba,
            vec![0x10, 0x20, 0x30, 0x40]
        );
    }

    #[test]
    fn decodes_adam7_empty_passes_and_all_formats() {
        // 1x1 RGBA8 Adam7 (passes 2..=7 are empty and emit zero bytes)
        let raw_1x1 = encode_adam7_filtered(1, 1, 6, 8, |_x, _y| vec![9, 8, 7, 255], |_p, _py| 0);
        let p_1x1 = build_png(make_ihdr(1, 1, 8, 6, 1), None, None, &raw_1x1);
        assert_eq!(decode_png(&p_1x1, 1024).unwrap().rgba, vec![9, 8, 7, 255]);

        // 3x3 RGBA8 Adam7 (passes 2 and 3 empty) with varied filters per pass row
        let expected_3x3 = |x: usize, y: usize| -> [u8; 4] {
            [
                u8::try_from(x * 40 + 10).unwrap(),
                u8::try_from(y * 50 + 20).unwrap(),
                u8::try_from((x + y) * 30).unwrap(),
                255,
            ]
        };
        let raw_3x3 = encode_adam7_filtered(
            3,
            3,
            6,
            8,
            |x, y| expected_3x3(x, y).to_vec(),
            |pass_idx, py| u8::try_from((pass_idx + py) % 5).unwrap(),
        );
        let p_3x3 = build_png(make_ihdr(3, 3, 8, 6, 1), None, None, &raw_3x3);
        let dec_3x3 = decode_png(&p_3x3, 4096).unwrap();
        for y in 0..3 {
            for x in 0..3 {
                let idx = (y * 3 + x) * 4;
                assert_eq!(&dec_3x3.rgba[idx..idx + 4], &expected_3x3(x, y));
            }
        }

        // 5x1 and 1x5 edge shapes
        let raw_5x1 = encode_adam7_filtered(
            5,
            1,
            2,
            8,
            |x, _y| vec![u8::try_from(x * 50).unwrap(), 10, 200],
            |_p, _py| 1,
        );
        let p_5x1 = build_png(make_ihdr(5, 1, 8, 2, 1), None, None, &raw_5x1);
        let dec_5x1 = decode_png(&p_5x1, 1024).unwrap();
        for x in 0..5 {
            assert_eq!(
                &dec_5x1.rgba[x * 4..x * 4 + 4],
                &[u8::try_from(x * 50).unwrap(), 10, 200, 255]
            );
        }

        let raw_1x5 = encode_adam7_filtered(
            1,
            5,
            4,
            8,
            |_x, y| {
                vec![
                    u8::try_from(y * 40).unwrap(),
                    u8::try_from(255 - y * 20).unwrap(),
                ]
            },
            |p, _py| u8::try_from(p % 5).unwrap(),
        );
        let p_1x5 = build_png(make_ihdr(1, 5, 8, 4, 1), None, None, &raw_1x5);
        let dec_1x5 = decode_png(&p_1x5, 1024).unwrap();
        for y in 0..5 {
            let g = u8::try_from(y * 40).unwrap();
            let a = u8::try_from(255 - y * 20).unwrap();
            assert_eq!(&dec_1x5.rgba[y * 4..y * 4 + 4], &[g, g, g, a]);
        }

        // 9x9 1-bit grayscale Adam7 with tRNS + 9x9 4-bit indexed Adam7 with PLTE + tRNS
        let raw_9x9_g1 = encode_adam7_filtered(
            9,
            9,
            0,
            1,
            |x, y| vec![u8::try_from((x + y) & 1).unwrap()],
            |p, py| u8::try_from((p + py) % 5).unwrap(),
        );
        let p_9x9_g1 = build_png(make_ihdr(9, 9, 1, 0, 1), None, Some(&[0, 0]), &raw_9x9_g1);
        let dec_9x9_g1 = decode_png(&p_9x9_g1, 4096).unwrap();
        for y in 0..9 {
            for x in 0..9 {
                let idx = (y * 9 + x) * 4;
                let expected = if (x + y) & 1 == 1 {
                    [255, 255, 255, 255]
                } else {
                    [0, 0, 0, 0]
                };
                assert_eq!(&dec_9x9_g1.rgba[idx..idx + 4], &expected);
            }
        }

        // 9x9 4-bit indexed Adam7
        let mut plte = Vec::new();
        for i in 0..16u8 {
            plte.extend_from_slice(&[i * 16, 255 - i * 16, i * 8]);
        }
        let trns = [50u8, 100, 150];
        let raw_9x9_i4 = encode_adam7_filtered(
            9,
            9,
            3,
            4,
            |x, y| vec![u8::try_from((x * 3 + y) % 16).unwrap()],
            |p, py| u8::try_from((p + py) % 5).unwrap(),
        );
        let p_9x9_i4 = build_png(
            make_ihdr(9, 9, 4, 3, 1),
            Some(&plte),
            Some(&trns),
            &raw_9x9_i4,
        );
        let dec_9x9_i4 = decode_png(&p_9x9_i4, 4096).unwrap();
        for y in 0..9 {
            for x in 0..9 {
                let idx = (y * 9 + x) * 4;
                let pal_idx = (x * 3 + y) % 16;
                let alpha = trns.get(pal_idx).copied().unwrap_or(255);
                let p_u8 = u8::try_from(pal_idx).unwrap();
                assert_eq!(
                    &dec_9x9_i4.rgba[idx..idx + 4],
                    &[p_u8 * 16, 255 - p_u8 * 16, p_u8 * 8, alpha]
                );
            }
        }

        // 5x5 16-bit RGB Adam7 with tRNS
        let trns_rgb16 = [0x01, 0x00, 0x02, 0x00, 0x03, 0x00];
        let raw_5x5_rgb16 = encode_adam7_filtered(
            5,
            5,
            2,
            16,
            |x, y| {
                if x == 2 && y == 3 {
                    vec![0x01, 0x00, 0x02, 0x00, 0x03, 0x00]
                } else {
                    vec![
                        u8::try_from(x * 30).unwrap(),
                        0x11,
                        u8::try_from(y * 40).unwrap(),
                        0x22,
                        0x77,
                        0x88,
                    ]
                }
            },
            |p, py| u8::try_from((p + py) % 5).unwrap(),
        );
        let p_5x5_rgb16 = build_png(
            make_ihdr(5, 5, 16, 2, 1),
            None,
            Some(&trns_rgb16),
            &raw_5x5_rgb16,
        );
        let dec_5x5_rgb16 = decode_png(&p_5x5_rgb16, 4096).unwrap();
        for y in 0..5 {
            for x in 0..5 {
                let idx = (y * 5 + x) * 4;
                if x == 2 && y == 3 {
                    assert_eq!(&dec_5x5_rgb16.rgba[idx..idx + 4], &[1, 2, 3, 0]);
                } else {
                    assert_eq!(
                        &dec_5x5_rgb16.rgba[idx..idx + 4],
                        &[
                            u8::try_from(x * 30).unwrap(),
                            u8::try_from(y * 40).unwrap(),
                            0x77,
                            255
                        ]
                    );
                }
            }
        }

        // 3x3 16-bit grayscale+alpha (4, 16) and 16-bit RGBA (6, 16) Adam7
        let raw_ga16 = encode_adam7_filtered(
            3,
            3,
            4,
            16,
            |x, y| {
                let g = u8::try_from(x * 50 + y * 20).unwrap();
                let a = u8::try_from(200 - y * 30).unwrap();
                vec![g, 0x12, a, 0x34]
            },
            |p, py| u8::try_from((p + py) % 5).unwrap(),
        );
        let p_ga16 = build_png(make_ihdr(3, 3, 16, 4, 1), None, None, &raw_ga16);
        let dec_ga16 = decode_png(&p_ga16, 4096).unwrap();
        for y in 0..3 {
            for x in 0..3 {
                let idx = (y * 3 + x) * 4;
                let g = u8::try_from(x * 50 + y * 20).unwrap();
                let a = u8::try_from(200 - y * 30).unwrap();
                assert_eq!(&dec_ga16.rgba[idx..idx + 4], &[g, g, g, a]);
            }
        }

        let raw_rgba16 = encode_adam7_filtered(
            3,
            3,
            6,
            16,
            |x, y| {
                vec![
                    u8::try_from(x * 60).unwrap(),
                    0x01,
                    u8::try_from(y * 70).unwrap(),
                    0x02,
                    0xAB,
                    0xCD,
                    0xEF,
                    0x10,
                ]
            },
            |p, py| u8::try_from((p + py) % 5).unwrap(),
        );
        let p_rgba16 = build_png(make_ihdr(3, 3, 16, 6, 1), None, None, &raw_rgba16);
        let dec_rgba16 = decode_png(&p_rgba16, 4096).unwrap();
        for y in 0..3 {
            for x in 0..3 {
                let idx = (y * 3 + x) * 4;
                assert_eq!(
                    &dec_rgba16.rgba[idx..idx + 4],
                    &[
                        u8::try_from(x * 60).unwrap(),
                        u8::try_from(y * 70).unwrap(),
                        0xAB,
                        0xEF
                    ]
                );
            }
        }
    }

    #[test]
    fn enforces_chunk_ordering_critical_checks_and_idat_cap() {
        let ihdr_rgba = make_ihdr(1, 1, 8, 6, 0);
        let ihdr_idx = make_ihdr(1, 1, 8, 3, 0);
        let ihdr_gray = make_ihdr(1, 1, 8, 0, 0);
        let ihdr_ga = make_ihdr(1, 1, 8, 4, 0);
        let idat_rgba = zlib_stored(&[0, 10, 20, 30, 255]);
        let idat_idx = zlib_stored(&[0, 0]);

        // 1. Invalid signature / EOF / missing IHDR
        assert_eq!(
            decode_png(b"not a png", 1024).err(),
            Some(PngError::InvalidSignature)
        );
        assert_eq!(
            decode_png(&build_png_from_chunks(&[(b"IHDR", &ihdr_rgba)]), 1024).err(),
            Some(PngError::UnexpectedEof)
        );
        assert_eq!(
            decode_png(&build_png_from_chunks(&[(b"IEND", &[])]), 1024).err(),
            Some(PngError::MissingIhdr)
        );
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[(b"IDAT", &idat_rgba), (b"IEND", &[])]),
                1024
            )
            .err(),
            Some(PngError::MissingIhdr)
        );

        // 2. IHDR not first / duplicate IHDR
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"tEXt", b"k\0v"),
                    (b"IHDR", &ihdr_rgba),
                    (b"IDAT", &idat_rgba),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidChunkOrder)
        );
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_rgba),
                    (b"IHDR", &ihdr_rgba),
                    (b"IDAT", &idat_rgba),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidChunkOrder)
        );

        // 3. PLTE ordering and constraints
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_idx),
                    (b"IDAT", &idat_idx),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidPalette)
        );
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_idx),
                    (b"IDAT", &idat_idx),
                    (b"PLTE", &[1, 2, 3]),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidChunkOrder)
        );
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_idx),
                    (b"PLTE", &[1, 2, 3]),
                    (b"PLTE", &[4, 5, 6]),
                    (b"IDAT", &idat_idx),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidChunkOrder)
        );
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_gray),
                    (b"PLTE", &[1, 2, 3]),
                    (b"IDAT", &idat_idx),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidPalette)
        );
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_ga),
                    (b"PLTE", &[1, 2, 3]),
                    (b"IDAT", &idat_rgba),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidPalette)
        );

        // 4. tRNS ordering and constraints
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_idx),
                    (b"tRNS", &[128]),
                    (b"PLTE", &[1, 2, 3]),
                    (b"IDAT", &idat_idx),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidChunkOrder)
        );
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_idx),
                    (b"PLTE", &[1, 2, 3]),
                    (b"IDAT", &idat_idx),
                    (b"tRNS", &[128]),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidChunkOrder)
        );
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_idx),
                    (b"PLTE", &[1, 2, 3]),
                    (b"tRNS", &[10, 20]), // 2 entries > 1 PLTE entry
                    (b"IDAT", &idat_idx),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidTransparency)
        );
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_rgba),
                    (b"tRNS", &[0]),
                    (b"IDAT", &idat_rgba),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidTransparency)
        );

        // 5. Missing IDAT, non-consecutive IDAT, and split valid IDAT
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[(b"IHDR", &ihdr_rgba), (b"IEND", &[])]),
                1024
            )
            .err(),
            Some(PngError::MissingIdat)
        );
        let (part1, part2) = idat_rgba.split_at(idat_rgba.len() / 2);
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_rgba),
                    (b"IDAT", part1),
                    (b"tEXt", b"k\0v"),
                    (b"IDAT", part2),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidChunkOrder)
        );
        let split_ok = build_png_from_chunks(&[
            (b"IHDR", &ihdr_rgba),
            (b"tEXt", b"k\0v"),
            (b"IDAT", part1),
            (b"IDAT", part2),
            (b"tEXt", b"after\0idat"),
            (b"IEND", &[]),
        ]);
        assert_eq!(
            decode_png(&split_ok, 1024).unwrap().rgba,
            vec![10, 20, 30, 255]
        );

        // 6. Non-empty IEND & unknown critical chunk
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_rgba),
                    (b"IDAT", &idat_rgba),
                    (b"IEND", &[0]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidChunk)
        );
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_rgba),
                    (b"CRIT", &[1, 2]),
                    (b"IDAT", &idat_rgba),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::UnsupportedCriticalChunk)
        );
        // Unknown ancillary chunk (lowercase first byte) is ignored when CRC is valid.
        let anc_ok = build_png_from_chunks(&[
            (b"IHDR", &ihdr_rgba),
            (b"cRIT", &[1, 2]),
            (b"IDAT", &idat_rgba),
            (b"IEND", &[]),
        ]);
        assert_eq!(
            decode_png(&anc_ok, 1024).unwrap().rgba,
            vec![10, 20, 30, 255]
        );

        // 7. Pre-inflate IDAT byte cap
        let huge_idat = vec![0u8; 1024 * 1024 + 8192];
        let oversized =
            build_png_from_chunks(&[(b"IHDR", &ihdr_rgba), (b"IDAT", &huge_idat), (b"IEND", &[])]);
        assert_eq!(
            decode_png(&oversized, 1024).err(),
            Some(PngError::IdatLimitExceeded)
        );

        // 8. Palette out-of-range index & invalid filter & dimension cap
        let bad_idx_png = build_png(make_ihdr(1, 1, 8, 3, 0), Some(&[1, 2, 3]), None, &[0, 5]);
        assert_eq!(
            decode_png(&bad_idx_png, 1024).err(),
            Some(PngError::InvalidPalette)
        );

        let bad_filter = build_png(make_ihdr(1, 1, 8, 6, 0), None, None, &[5, 1, 2, 3, 4]);
        assert_eq!(
            decode_png(&bad_filter, 1024).err(),
            Some(PngError::InvalidFilter)
        );

        let ok_1x1 = build_png(make_ihdr(1, 1, 8, 6, 0), None, None, &[0, 1, 2, 3, 4]);
        assert_eq!(
            decode_png(&ok_1x1, 3).err(),
            Some(PngError::DimensionLimitExceeded)
        );

        // 9. IHDR validation, optional PLTE on RGB, non-alpha chunk type, oversized inflate stream
        assert_eq!(
            decode_png(&build_png(make_ihdr(0, 1, 8, 6, 0), None, None, &[0]), 1024).err(),
            Some(PngError::InvalidIhdr)
        );
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[(b"IHDR", &[0; 12]), (b"IEND", &[])]),
                1024
            )
            .err(),
            Some(PngError::InvalidIhdr)
        );
        assert_eq!(
            decode_png(
                &build_png(make_ihdr(1, 1, 16, 3, 0), None, None, &[0, 0]),
                1024
            )
            .err(),
            Some(PngError::UnsupportedFormat)
        );
        let mut bad_interlace = make_ihdr(1, 1, 8, 6, 2);
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &bad_interlace),
                    (b"IDAT", &idat_rgba),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::UnsupportedFormat)
        );
        bad_interlace[10] = 1; // invalid compression
        bad_interlace[12] = 0;
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &bad_interlace),
                    (b"IDAT", &idat_rgba),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::UnsupportedFormat)
        );

        // Non-alphabetic chunk type fails with InvalidChunk even with matching CRC.
        assert_eq!(
            decode_png(
                &build_png_from_chunks(&[
                    (b"IHDR", &ihdr_rgba),
                    (b"tE1t", b"bad"),
                    (b"IDAT", &idat_rgba),
                    (b"IEND", &[]),
                ]),
                1024
            )
            .err(),
            Some(PngError::InvalidChunk)
        );

        // Optional PLTE on color_type=2 (RGB) before IDAT succeeds.
        let rgb_with_plte = build_png(
            make_ihdr(1, 1, 8, 2, 0),
            Some(&[1, 2, 3]),
            None,
            &[0, 100, 150, 200],
        );
        assert_eq!(
            decode_png(&rgb_with_plte, 1024).unwrap().rgba,
            vec![100, 150, 200, 255]
        );

        // Decompressed stream larger than expected scanline bytes fails DecompressFailed.
        let extra_raw = build_png(make_ihdr(1, 1, 8, 6, 0), None, None, &[0, 1, 2, 3, 4, 99]);
        assert_eq!(
            decode_png(&extra_raw, 1024).err(),
            Some(PngError::DecompressFailed)
        );
    }
}
