//! Pure-Rust base64 decoder for Kitty Graphics payloads (supports unpadded and whitespace).

fn decode_char(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decodes base64 `input` into raw bytes, ignoring ASCII whitespace and allowing optional `=`
/// padding (or unpadded trailing 2/3-char groups).
pub(crate) fn decode_base64(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity((input.len() / 4) * 3 + 3);
    let mut buf = [0u8; 4];
    let mut count = 0usize;

    for &b in input {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            return decode_base64_padded(input);
        }
        let val = decode_char(b)?;
        buf[count] = val;
        count += 1;
        if count == 4 {
            let triple = (u32::from(buf[0]) << 18)
                | (u32::from(buf[1]) << 12)
                | (u32::from(buf[2]) << 6)
                | u32::from(buf[3]);
            out.push(((triple >> 16) & 0xFF) as u8);
            out.push(((triple >> 8) & 0xFF) as u8);
            out.push((triple & 0xFF) as u8);
            count = 0;
        }
    }

    match count {
        0 => Some(out),
        1 => None,
        2 => {
            let triple = (u32::from(buf[0]) << 18) | (u32::from(buf[1]) << 12);
            out.push(((triple >> 16) & 0xFF) as u8);
            Some(out)
        }
        3 => {
            let triple =
                (u32::from(buf[0]) << 18) | (u32::from(buf[1]) << 12) | (u32::from(buf[2]) << 6);
            out.push(((triple >> 16) & 0xFF) as u8);
            out.push(((triple >> 8) & 0xFF) as u8);
            Some(out)
        }
        _ => None,
    }
}

fn decode_base64_padded(input: &[u8]) -> Option<Vec<u8>> {
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
    let total = chunks.len();
    for (idx, chunk) in chunks.iter().enumerate() {
        let is_last = idx + 1 == total;
        let a = decode_char(chunk[0])?;
        let b = decode_char(chunk[1])?;
        let pad_c = chunk[2] == b'=';
        let pad_d = chunk[3] == b'=';

        if pad_c && !pad_d {
            return None;
        }
        if (pad_c || pad_d) && !is_last {
            return None;
        }

        let c = if pad_c { 0 } else { decode_char(chunk[2])? };
        let d = if pad_d { 0 } else { decode_char(chunk[3])? };

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
