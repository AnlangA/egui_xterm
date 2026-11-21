//! Pure-Rust RFC 1950 (zlib) and RFC 1951 (DEFLATE) decompressor.

const MAX_BITS: usize = 15;

/// Errors returned by [`inflate_zlib`] / [`inflate_raw`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InflateError {
    UnexpectedEof,
    InvalidZlibHeader,
    UnsupportedPresetDict,
    InvalidBlockType,
    InvalidStoredLength,
    InvalidHuffmanTree,
    InvalidSymbol,
    InvalidDistance,
    OutputLimitExceeded,
    Adler32Mismatch,
}

struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_buf: u32,
    bit_count: u8,
}

impl<'a> BitReader<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_buf: 0,
            bit_count: 0,
        }
    }

    fn ensure_bits(&mut self, n: u8) -> Result<(), InflateError> {
        while self.bit_count < n {
            if self.byte_pos >= self.data.len() {
                return Err(InflateError::UnexpectedEof);
            }
            self.bit_buf |= u32::from(self.data[self.byte_pos]) << self.bit_count;
            self.byte_pos += 1;
            self.bit_count += 8;
        }
        Ok(())
    }

    fn read_bits(&mut self, n: u8) -> Result<u32, InflateError> {
        if n == 0 {
            return Ok(0);
        }
        self.ensure_bits(n)?;
        let mask = (1u32 << n) - 1;
        let val = self.bit_buf & mask;
        self.bit_buf >>= n;
        self.bit_count -= n;
        Ok(val)
    }

    fn align_to_byte(&mut self) {
        let drop_bits = self.bit_count % 8;
        self.bit_buf >>= drop_bits;
        self.bit_count -= drop_bits;
        // Rewind any whole bytes buffered in bit_buf back to byte_pos.
        let buffered_bytes = usize::from(self.bit_count / 8);
        self.byte_pos = self.byte_pos.saturating_sub(buffered_bytes);
        self.bit_buf = 0;
        self.bit_count = 0;
    }

    fn read_u16_le(&mut self) -> Result<u16, InflateError> {
        if self.byte_pos + 2 > self.data.len() {
            return Err(InflateError::UnexpectedEof);
        }
        let val = u16::from_le_bytes([self.data[self.byte_pos], self.data[self.byte_pos + 1]]);
        self.byte_pos += 2;
        Ok(val)
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], InflateError> {
        if self.byte_pos.saturating_add(len) > self.data.len() {
            return Err(InflateError::UnexpectedEof);
        }
        let slice = &self.data[self.byte_pos..self.byte_pos + len];
        self.byte_pos += len;
        Ok(slice)
    }
}

struct HuffmanTable {
    counts: [u16; MAX_BITS + 1],
    symbols: Vec<u16>,
}

impl HuffmanTable {
    fn from_lengths(lengths: &[u8]) -> Result<Self, InflateError> {
        let mut counts = [0u16; MAX_BITS + 1];
        for &len in lengths {
            let l = usize::from(len);
            if l > MAX_BITS {
                return Err(InflateError::InvalidHuffmanTree);
            }
            if l > 0 {
                counts[l] = counts[l].saturating_add(1);
            }
        }

        // Check for oversubscribed tree.
        let mut left = 1i32;
        for &c in &counts[1..] {
            left <<= 1;
            left -= i32::from(c);
            if left < 0 {
                return Err(InflateError::InvalidHuffmanTree);
            }
        }

        let mut offsets = [0u16; MAX_BITS + 1];
        let mut total = 0u16;
        for len in 1..=MAX_BITS {
            offsets[len] = total;
            total = total.saturating_add(counts[len]);
        }

        let mut symbols = vec![0u16; usize::from(total)];
        for (sym, &len) in lengths.iter().enumerate() {
            let l = usize::from(len);
            if l != 0 {
                let idx = usize::from(offsets[l]);
                if idx >= symbols.len() {
                    return Err(InflateError::InvalidHuffmanTree);
                }
                symbols[idx] = u16::try_from(sym).map_err(|_| InflateError::InvalidHuffmanTree)?;
                offsets[l] += 1;
            }
        }

        Ok(Self { counts, symbols })
    }

    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16, InflateError> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;

        for len in 1..=MAX_BITS {
            code |= i32::try_from(reader.read_bits(1)?).unwrap_or(0);
            let count = i32::from(self.counts[len]);
            if code - count < first {
                let sym_idx = usize::try_from(index + (code - first))
                    .map_err(|_| InflateError::InvalidSymbol)?;
                return self
                    .symbols
                    .get(sym_idx)
                    .copied()
                    .ok_or(InflateError::InvalidSymbol);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(InflateError::InvalidSymbol)
    }
}

fn fixed_tables() -> Result<(HuffmanTable, HuffmanTable), InflateError> {
    let mut lit_lengths = [0u8; 288];
    lit_lengths[..144].fill(8);
    lit_lengths[144..256].fill(9);
    lit_lengths[256..280].fill(7);
    lit_lengths[280..288].fill(8);
    let dist_lengths = [5u8; 32];
    Ok((
        HuffmanTable::from_lengths(&lit_lengths)?,
        HuffmanTable::from_lengths(&dist_lengths)?,
    ))
}

fn dynamic_tables(
    reader: &mut BitReader<'_>,
) -> Result<(HuffmanTable, HuffmanTable), InflateError> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];

    let hlit = usize::try_from(reader.read_bits(5)?).unwrap_or(0) + 257;
    let hdist = usize::try_from(reader.read_bits(5)?).unwrap_or(0) + 1;
    let hclen = usize::try_from(reader.read_bits(4)?).unwrap_or(0) + 4;

    let mut cl_lengths = [0u8; 19];
    for &idx in &ORDER[..hclen] {
        cl_lengths[idx] = u8::try_from(reader.read_bits(3)?).unwrap_or(0);
    }
    let cl_table = HuffmanTable::from_lengths(&cl_lengths)?;

    let total = hlit + hdist;
    let mut lengths = Vec::with_capacity(total);
    while lengths.len() < total {
        let sym = cl_table.decode(reader)?;
        match sym {
            0..=15 => lengths.push(u8::try_from(sym).unwrap_or(0)),
            16 => {
                let prev = *lengths.last().ok_or(InflateError::InvalidHuffmanTree)?;
                let repeat = usize::try_from(reader.read_bits(2)?).unwrap_or(0) + 3;
                if lengths.len() + repeat > total {
                    return Err(InflateError::InvalidHuffmanTree);
                }
                lengths.resize(lengths.len() + repeat, prev);
            }
            17 => {
                let repeat = usize::try_from(reader.read_bits(3)?).unwrap_or(0) + 3;
                if lengths.len() + repeat > total {
                    return Err(InflateError::InvalidHuffmanTree);
                }
                lengths.resize(lengths.len() + repeat, 0);
            }
            18 => {
                let repeat = usize::try_from(reader.read_bits(7)?).unwrap_or(0) + 11;
                if lengths.len() + repeat > total {
                    return Err(InflateError::InvalidHuffmanTree);
                }
                lengths.resize(lengths.len() + repeat, 0);
            }
            _ => return Err(InflateError::InvalidHuffmanTree),
        }
    }

    let lit_table = HuffmanTable::from_lengths(&lengths[..hlit])?;
    let dist_table = HuffmanTable::from_lengths(&lengths[hlit..])?;
    Ok((lit_table, dist_table))
}

fn decode_huffman_block(
    reader: &mut BitReader<'_>,
    lit_table: &HuffmanTable,
    dist_table: &HuffmanTable,
    out: &mut Vec<u8>,
    max_out: usize,
) -> Result<(), InflateError> {
    const LEN_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LEN_EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DIST_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DIST_EXTRA: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];

    loop {
        let sym = lit_table.decode(reader)?;
        if sym < 256 {
            if out.len() >= max_out {
                return Err(InflateError::OutputLimitExceeded);
            }
            out.push(u8::try_from(sym).unwrap_or(0));
        } else if sym == 256 {
            return Ok(());
        } else {
            let len_idx = usize::from(sym - 257);
            if len_idx >= LEN_BASE.len() {
                return Err(InflateError::InvalidSymbol);
            }
            let extra_len = LEN_EXTRA[len_idx];
            let length = usize::from(LEN_BASE[len_idx])
                + usize::try_from(reader.read_bits(extra_len)?).unwrap_or(0);

            let dist_sym = usize::from(dist_table.decode(reader)?);
            if dist_sym >= DIST_BASE.len() {
                return Err(InflateError::InvalidDistance);
            }
            let extra_dist = DIST_EXTRA[dist_sym];
            let dist = usize::from(DIST_BASE[dist_sym])
                + usize::try_from(reader.read_bits(extra_dist)?).unwrap_or(0);

            if dist == 0 || dist > out.len() {
                return Err(InflateError::InvalidDistance);
            }
            if out.len().saturating_add(length) > max_out {
                return Err(InflateError::OutputLimitExceeded);
            }
            for _ in 0..length {
                let b = out[out.len() - dist];
                out.push(b);
            }
        }
    }
}

/// Decompresses a raw RFC 1951 DEFLATE stream up to `max_out` bytes, returning `(output, bytes_consumed)`.
pub(crate) fn inflate_raw(input: &[u8], max_out: usize) -> Result<(Vec<u8>, usize), InflateError> {
    let mut reader = BitReader::new(input);
    let mut out = Vec::new();

    loop {
        let bfinal = reader.read_bits(1)?;
        let btype = reader.read_bits(2)?;
        match btype {
            0 => {
                reader.align_to_byte();
                let len = reader.read_u16_le()?;
                let nlen = reader.read_u16_le()?;
                if len ^ nlen != 0xFFFF {
                    return Err(InflateError::InvalidStoredLength);
                }
                let len_u = usize::from(len);
                if out.len().saturating_add(len_u) > max_out {
                    return Err(InflateError::OutputLimitExceeded);
                }
                let bytes = reader.read_bytes(len_u)?;
                out.extend_from_slice(bytes);
            }
            1 => {
                let (lit, dist) = fixed_tables()?;
                decode_huffman_block(&mut reader, &lit, &dist, &mut out, max_out)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut reader)?;
                decode_huffman_block(&mut reader, &lit, &dist, &mut out, max_out)?;
            }
            _ => return Err(InflateError::InvalidBlockType),
        }

        if bfinal != 0 {
            break;
        }
    }

    reader.align_to_byte();
    Ok((out, reader.byte_pos))
}

fn adler32(data: &[u8]) -> u32 {
    const MOD_ADLER: u32 = 65521;
    let mut s1 = 1u32;
    let mut s2 = 0u32;
    for &b in data {
        s1 = (s1 + u32::from(b)) % MOD_ADLER;
        s2 = (s2 + s1) % MOD_ADLER;
    }
    (s2 << 16) | s1
}

/// Decompresses an RFC 1950 zlib stream up to `max_out` bytes and verifies its Adler-32 checksum.
pub(crate) fn inflate_zlib(input: &[u8], max_out: usize) -> Result<Vec<u8>, InflateError> {
    if input.len() < 6 {
        return Err(InflateError::UnexpectedEof);
    }
    let cmf = input[0];
    let flg = input[1];
    if (u16::from(cmf) * 256 + u16::from(flg)) % 31 != 0 {
        return Err(InflateError::InvalidZlibHeader);
    }
    let cm = cmf & 0x0F;
    let cinfo = cmf >> 4;
    if cm != 8 || cinfo > 7 {
        return Err(InflateError::InvalidZlibHeader);
    }
    if (flg & 0x20) != 0 {
        return Err(InflateError::UnsupportedPresetDict);
    }

    let (out, consumed) = inflate_raw(&input[2..], max_out)?;
    let trailer_pos = 2 + consumed;
    if trailer_pos + 4 > input.len() {
        return Err(InflateError::UnexpectedEof);
    }
    let expected = u32::from_be_bytes([
        input[trailer_pos],
        input[trailer_pos + 1],
        input[trailer_pos + 2],
        input[trailer_pos + 3],
    ]);
    if adler32(&out) != expected {
        return Err(InflateError::Adler32Mismatch);
    }
    Ok(out)
}
