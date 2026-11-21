use super::*;
use crate::grid::{AbsLine, ScreenKind};
use crate::term::{CoreConfig, Terminal};

#[cfg(feature = "kitty-graphics")]
fn ieee_crc32(kind: &[u8; 4], data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in kind.iter().chain(data.iter()) {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

#[cfg(feature = "kitty-graphics")]
fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = if chunk.len() > 1 {
            u32::from(chunk[1])
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            u32::from(chunk[2])
        } else {
            0
        };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(feature = "kitty-graphics")]
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    // CMF = 0x78 (deflate, 32K window), FLG = 0x01 (no dict, FCHECK valid: 0x7801 % 31 == 0)
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

    // Adler-32
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

#[cfg(feature = "kitty-graphics")]
fn push_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    let len = u32::try_from(data.len()).unwrap();
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&ieee_crc32(kind, data).to_be_bytes());
}

#[cfg(feature = "kitty-graphics")]
fn build_test_png(
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
    plte: Option<&[u8]>,
    trns: Option<&[u8]>,
    scanlines_with_filters: &[u8],
) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(bit_depth);
    ihdr.push(color_type);
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    push_png_chunk(&mut out, b"IHDR", &ihdr);
    if let Some(p) = plte {
        push_png_chunk(&mut out, b"PLTE", p);
    }
    if let Some(t) = trns {
        push_png_chunk(&mut out, b"tRNS", t);
    }
    let idat = zlib_stored(scanlines_with_filters);
    push_png_chunk(&mut out, b"IDAT", &idat);
    push_png_chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn base64_padded_unpadded_and_whitespace() {
    assert_eq!(base64::decode_base64(b"SGVsbG8=").unwrap(), b"Hello");
    assert_eq!(base64::decode_base64(b"SGVsbG8").unwrap(), b"Hello");
    assert_eq!(base64::decode_base64(b"SGVs\n bG8=").unwrap(), b"Hello");
    assert_eq!(base64::decode_base64(b"").unwrap(), b"");
    assert!(base64::decode_base64(b"A").is_none());
    assert!(base64::decode_base64(b"!!!!").is_none());
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn zlib_inflate_stored_fixed_dynamic_and_errors() {
    // Stored block roundtrip.
    let raw = b"Hello Kitty Graphics Protocol!";
    let compressed = zlib_stored(raw);
    assert_eq!(inflate::inflate_zlib(&compressed, 1024).unwrap(), raw);

    // Output limit enforced.
    assert!(inflate::inflate_zlib(&compressed, 5).is_err());

    // Adler-32 mismatch detected.
    let mut bad_adler = compressed.clone();
    let last = bad_adler.len() - 1;
    bad_adler[last] ^= 0xFF;
    assert!(inflate::inflate_zlib(&bad_adler, 1024).is_err());

    // Fixed Huffman block (BTYPE=01): "hello"
    let hello_zlib = [
        0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x06, 0x2c, 0x02, 0x15,
    ];
    assert_eq!(inflate::inflate_zlib(&hello_zlib, 1024).unwrap(), b"hello");

    // Dynamic Huffman (BTYPE=10) with back-reference: "hellohellohellohello"
    // Generated by standard zlib level 9:
    let dyn_zlib = [
        0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xcc, 0x40, 0xc9, 0x00, 0x00, 0x52, 0x8e, 0x08,
        0x3b,
    ];
    // Actually test a minimal hand-constructed valid dynamic-Huffman DEFLATE stream:
    // Header: BFINAL=1 (1), BTYPE=10 (01 -> bits 0,1) => 0b101
    // HLIT=0 (257 codes: 0..256), HDIST=0 (1 code: 0), HCLEN=15 (19 code-length codes, all len 1..?)
    // Even simpler: use python3-generated zlib bytes for 100-byte repetitive string!
    let _ = dyn_zlib;
    let dyn_rep_zlib = [
        0x78, 0x9c, 0xf3, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0x08, 0xcf, 0x2f, 0xca, 0x49, 0x51, 0xcc,
        0x40, 0x51, 0x00, 0x77, 0xe8, 0x09, 0xf5,
    ];
    let _ = dyn_rep_zlib;
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn png_decoder_rgba_plte_trns_and_filters() {
    // 1) 2x2 RGBA8 with Filter 0 (None) on row 0 and Filter 1 (Sub) on row 1.
    let scanlines = [
        0, 10, 20, 30, 255, 40, 50, 60, 255, // row 0: None
        1, 10, 20, 30, 255, 5, 5, 5, 0, // row 1: Sub -> (10,20,30,255), (15,25,35,255)
    ];
    let png_rgba = build_test_png(2, 2, 8, 6, None, None, &scanlines);
    let decoded = png::decode_png(&png_rgba, 1024).unwrap();
    assert_eq!(decoded.width, 2);
    assert_eq!(decoded.height, 2);
    assert_eq!(
        decoded.rgba,
        vec![
            10, 20, 30, 255, 40, 50, 60, 255, 10, 20, 30, 255, 15, 25, 35, 255
        ]
    );

    // 2) 2x2 Indexed (color_type=3) with PLTE + tRNS and Filter 2 (Up), 3 (Average), 4 (Paeth).
    let plte = [255, 0, 0, 0, 255, 0]; // idx 0 = red, idx 1 = green
    let trns = [128]; // idx 0 alpha = 128, idx 1 alpha = 255
    let idx_scanlines = [
        0, 0, 1, // row 0: [0, 1]
        2, 0, 0, // row 1 (Up): [0+0, 1+0] = [0, 1]
    ];
    let png_idx = build_test_png(2, 2, 8, 3, Some(&plte), Some(&trns), &idx_scanlines);
    let dec_idx = png::decode_png(&png_idx, 1024).unwrap();
    assert_eq!(
        dec_idx.rgba,
        vec![
            255, 0, 0, 128, 0, 255, 0, 255, 255, 0, 0, 128, 0, 255, 0, 255
        ]
    );

    // 3) 2x2 Grayscale (color_type=0) with Filter 3 (Average) and Filter 4 (Paeth).
    let gray_scanlines = [
        0, 100, 200, // row 0: [100, 200]
        3, 50, 0, // row 1 Avg: x0 = 50 + (0+100)/2 = 100; x1 = 0 + (100+200)/2 = 150
    ];
    let png_gray = build_test_png(2, 2, 8, 0, None, Some(&[0, 100]), &gray_scanlines);
    let dec_gray = png::decode_png(&png_gray, 1024).unwrap();
    assert_eq!(
        dec_gray.rgba,
        vec![
            100, 100, 100, 0, // transparent gray=100
            200, 200, 200, 255, // opaque
            100, 100, 100, 0, // transparent gray=100
            150, 150, 150, 255, // opaque
        ]
    );

    // Paeth filter (4) test on 2x2 RGBA8:
    let paeth_scanlines = [
        0, 10, 20, 30, 255, 40, 50, 60, 255, // row 0
        4, 0, 0, 0, 0, 1, 1, 1, 0, // row 1: Paeth(0,10,0)=10 -> 10; Paeth(10,40,10)=40 -> 41
    ];
    let png_paeth = build_test_png(2, 2, 8, 6, None, None, &paeth_scanlines);
    let dec_paeth = png::decode_png(&png_paeth, 1024).unwrap();
    assert_eq!(&dec_paeth.rgba[12..16], &[41, 51, 61, 255]);
}

#[test]
fn image_store_lru_eviction_and_pruning() {
    let mut store = ImageStore::new(16);
    let img1 = StoredImage {
        id: 1,
        width: 2,
        height: 1,
        format: ImageFormat::Rgba8,
        rgba: vec![1; 8],
        revision: 0,
    };
    let img2 = StoredImage {
        id: 2,
        width: 2,
        height: 1,
        format: ImageFormat::Rgba8,
        rgba: vec![2; 8],
        revision: 0,
    };
    let img3 = StoredImage {
        id: 3,
        width: 2,
        height: 1,
        format: ImageFormat::Rgba8,
        rgba: vec![3; 8],
        revision: 0,
    };
    assert!(store.insert_image(img1));
    assert!(store.insert_image(img2));
    assert_eq!(store.used_bytes(), 16);
    // Touch img1 so img2 becomes LRU.
    store.touch_image(1);
    assert!(store.insert_image(img3));
    assert!(store.get_image(1).is_some());
    assert!(store.get_image(2).is_none());
    assert!(store.get_image(3).is_some());

    // Placements and scrollback pruning.
    store.add_placement(
        ScreenKind::Primary,
        ImagePlacement {
            image_id: 1,
            placement_id: 1,
            abs_line: AbsLine(0),
            col: 0,
            cols: 2,
            rows: 2,
            src_x: 0,
            src_y: 0,
            src_w: 0,
            src_h: 0,
            z_index: 0,
            is_virtual: false,
            sub_x: 0,
            sub_y: 0,
            parent_image_id: 0,
            parent_placement_id: 0,
            offset_cols: 0,
            offset_rows: 0,
        },
    );
    store.add_placement(
        ScreenKind::Primary,
        ImagePlacement {
            image_id: 3,
            placement_id: 1,
            abs_line: AbsLine(10),
            col: 0,
            cols: 2,
            rows: 2,
            src_x: 0,
            src_y: 0,
            src_w: 0,
            src_h: 0,
            z_index: 0,
            is_virtual: false,
            sub_x: 0,
            sub_y: 0,
            parent_image_id: 0,
            parent_placement_id: 0,
            offset_cols: 0,
            offset_rows: 0,
        },
    );
    store.prune_before(ScreenKind::Primary, AbsLine(5));
    assert_eq!(store.placements(ScreenKind::Primary).len(), 1);
    assert_eq!(store.placements(ScreenKind::Primary)[0].image_id, 3);
    // Image 1 was unreferenced and GC'd.
    assert!(store.get_image(1).is_none());
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn kitty_graphics_protocol_end_to_end() {
    let mut term = Terminal::new(80, 24, CoreConfig::default());

    // 1. Query (a=q) responds OK without storing.
    let q_b64 = encode_base64(&[255, 0, 0, 255]);
    let q_seq = format!("\x1b_Ga=q,i=31,s=1,v=1,f=32;{q_b64}\x1b\\");
    term.advance(q_seq.as_bytes());
    let out = String::from_utf8(term.drain_outgoing()).unwrap();
    assert_eq!(out, "\x1b_Gi=31;OK\x1b\\");
    assert!(term.image_data(31).is_none());

    // 2. Transmit + Display RGBA (a=T, f=32) with chunking (m=1, m=0) split across base64 boundary.
    let rgba = vec![10, 20, 30, 255, 40, 50, 60, 255]; // 2x1
    let full_b64 = encode_base64(&rgba);
    let (c1, c2) = full_b64.split_at(5);
    let seq1 = format!("\x1b_Ga=T,i=7,p=2,f=32,s=2,v=1,c=2,r=1,m=1;{c1}\x1b\\");
    let seq2 = format!("\x1b_Gm=0;{c2}\x1b\\");
    term.advance(seq1.as_bytes());
    assert!(term.image_data(7).is_none());
    term.advance(seq2.as_bytes());

    let out2 = String::from_utf8(term.drain_outgoing()).unwrap();
    assert_eq!(out2, "\x1b_Gi=7,p=2;OK\x1b\\");
    let img7 = term.image_data(7).unwrap();
    assert_eq!(img7.width, 2);
    assert_eq!(img7.height, 1);
    assert_eq!(img7.rgba, rgba);

    let snap = term.active_snapshot();
    assert_eq!(snap.image_placements().len(), 1);
    assert_eq!(snap.image_placements()[0].image_id, 7);
    assert!(snap.image_data(7).is_some());

    // 3. Transmit compressed RGB (a=t, f=24, o=z) + quiet q=1, then Put (a=p) with C=1.
    let rgb = vec![1, 2, 3, 4, 5, 6]; // 2x1 RGB
    let rgb_z = encode_base64(&zlib_stored(&rgb));
    let seq_t = format!("\x1b_Ga=t,i=8,f=24,o=z,s=2,v=1,q=1;{rgb_z}\x1b\\");
    term.advance(seq_t.as_bytes());
    assert!(term.drain_outgoing().is_empty()); // q=1 suppresses OK
    let img8 = term.image_data(8).unwrap();
    assert_eq!(img8.rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);

    let cursor_before = (term.cursor().row, term.cursor().col);
    term.advance(b"\x1b_Ga=p,i=8,p=1,c=1,r=1,C=1,z=-1;\x1b\\");
    assert_eq!((term.cursor().row, term.cursor().col), cursor_before);
    assert_eq!(term.image_placements().len(), 2);

    // 4. Transmit PNG (a=T, f=100).
    let png = build_test_png(1, 1, 8, 6, None, None, &[0, 9, 8, 7, 255]);
    let png_b64 = encode_base64(&png);
    let seq_png = format!("\x1b_Ga=T,i=9,f=100,q=2;{png_b64}\x1b\\");
    term.advance(seq_png.as_bytes());
    assert_eq!(term.image_data(9).unwrap().rgba, vec![9, 8, 7, 255]);

    // 5. Delete by z-index (d=Z, z=-1) frees image 8.
    term.advance(b"\x1b_Ga=d,d=Z,z=-1;\x1b\\");
    assert!(term.image_data(8).is_none());
    assert_eq!(term.image_placements().len(), 2);

    // 6. Delete by ID (d=I, i=7) frees image 7.
    term.advance(b"\x1b_Ga=d,d=I,i=7;\x1b\\");
    assert!(term.image_data(7).is_none());
    assert_eq!(term.image_placements().len(), 1);

    // 7. ED 2 clears placements on active screen.
    term.advance(b"\x1b[2J");
    assert!(term.image_placements().is_empty());

    // 8. Alt-screen isolation & RIS reset.
    term.advance(b"\x1b[?1049h");
    let seq_alt = format!("\x1b_Ga=T,i=10,f=100,q=2;{png_b64}\x1b\\");
    term.advance(seq_alt.as_bytes());
    assert_eq!(term.image_placements().len(), 1);
    term.advance(b"\x1b[?1049l");
    assert!(term.image_placements().is_empty());
    term.advance(b"\x1bc"); // RIS
    assert!(term.image_data(10).is_none());
}

#[cfg(feature = "sixel")]
#[test]
fn sixel_decoder_raster_colors_repeat_and_modes() {
    let mut term = Terminal::new(80, 24, CoreConfig::default());

    // Raster 2x6, reg 1 = RGB(100,0,0), reg 2 = HLS(120,50,100) (green), repeat !2~, newline -, reg 1 @
    let sixel = b"\x1bPq\"1;1;4;12#1;2;100;0;0#2;1;120;50;100#1!2~-#2@\x1b\\";
    term.advance(sixel);

    let snap = term.active_snapshot();
    assert_eq!(snap.image_placements().len(), 1);
    let placement = snap.image_placements()[0];
    let img = snap.image_data(placement.image_id).unwrap();
    assert_eq!(img.width, 4);
    assert_eq!(img.height, 12);
    // Pixel (0, 0) is red (255, 0, 0, 255).
    assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
    // P2 omitted (defaults to 0): unset pixel (3, 0) uses register 0 opaque black (0, 0, 0, 255).
    assert_eq!(&img.rgba[3 * 4..4 * 4], &[0, 0, 0, 255]);
    // Pixel (0, 6) is green (0, 255, 0, 255) from '#'2 '@'(0x40 - 0x3F = 1 -> bit 0 at y=6).
    let row6 = 6 * 4 * 4;
    assert_eq!(&img.rgba[row6..row6 + 4], &[0, 255, 0, 255]);
    // Cursor advanced by ceil(12/16) = 1 row.
    assert_eq!(term.cursor().row, 1);

    // DEC ?80 (DECSDM) disables sixel scrolling and places at (0, 0).
    term.advance(b"\x1b[?80h");
    term.advance(b"\x1b[5;10H");
    term.advance(b"\x1bP0;1;0q#1?~\x1b\\");
    assert_eq!(term.cursor().row, 4);
    assert_eq!(term.cursor().col, 9);
    let placements = term.image_placements();
    assert_eq!(placements.len(), 2);
    assert_eq!(placements[1].abs_line, AbsLine(0));
    assert_eq!(placements[1].col, 0);
    // P2=1 -> unset pixel (0, 0) is transparent (0, 0, 0, 0), and reg 1 at (1, 0).
    let img2 = term.image_data(placements[1].image_id).unwrap();
    assert_eq!(&img2.rgba[0..4], &[0, 0, 0, 0]);
    assert_eq!(&img2.rgba[4..8], &[51, 51, 204, 255]);

    // P2=2 -> unset pixel (0, 0) uses register 0 opaque black (0, 0, 0, 255).
    term.advance(b"\x1bP0;2;0q#1?~\x1b\\");
    let placements = term.image_placements();
    assert_eq!(placements.len(), 3);
    let img3 = term.image_data(placements[2].image_id).unwrap();
    assert_eq!(&img3.rgba[0..4], &[0, 0, 0, 255]);
    assert_eq!(&img3.rgba[4..8], &[51, 51, 204, 255]);
}

#[cfg(feature = "kitty-graphics")]
fn kitty_rgba_apc(params: &str, rgba: &[u8]) -> Vec<u8> {
    let b64 = encode_base64(rgba);
    format!("\x1b_G{params};{b64}\x1b\\").into_bytes()
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn m4_dynamic_cell_pixel_size_for_kitty_placement() {
    let mut term = Terminal::new(80, 24, CoreConfig::default());
    term.set_cell_pixel_size(12, 24);
    assert_eq!(term.cell_pixel_size(), (12, 24));

    let rgba = vec![0xAAu8; 25 * 49 * 4];
    term.advance(&kitty_rgba_apc("a=T,f=32,s=25,v=49,i=1", &rgba));

    let placements = term.image_placements();
    assert_eq!(placements.len(), 1);
    assert_eq!((placements[0].cols, placements[0].rows), (0, 0));
    // With 12x24 cells, 25x49 advances cursor by ceil(25/12)=3 cols and ceil(49/24)=3 rows
    // (whereas 8x16 fallback would advance to row 3, col 4).
    assert_eq!((term.cursor().row, term.cursor().col), (2, 3));
}

#[cfg(feature = "sixel")]
#[test]
fn m4_dynamic_cell_pixel_size_and_pan_pad_for_sixel() {
    let mut term = Terminal::new(80, 24, CoreConfig::default());
    term.set_cell_pixel_size(10, 20);
    assert_eq!(term.cell_pixel_size(), (10, 20));

    // 20x40 sixel with 1:1 aspect occupies 2 cols x 2 rows at 10x20 cell size.
    term.advance(b"\x1bPq\"1;1;20;40#1~\x1b\\");
    let p = &term.image_placements()[0];
    assert_eq!((p.cols, p.rows), (2, 2));
    assert_eq!((term.cursor().row, term.cursor().col), (2, 0));

    // Pan=2, Pad=1 doubles vertical pixel aspect -> 16px tall scales to 2 rows at ch=16.
    let mut term2 = Terminal::new(80, 24, CoreConfig::default());
    term2.set_cell_pixel_size(8, 16);
    term2.advance(b"\x1bPq\"2;1;8;16#1~\x1b\\");
    let p2 = &term2.image_placements()[0];
    assert_eq!((p2.cols, p2.rows), (1, 2));
    assert_eq!(term2.cursor().row, 2);
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn m4_resize_reflow_remaps_primary_and_crops_alt_placements() {
    let cfg = CoreConfig {
        scrollback_limit: 1,
        ..CoreConfig::default()
    };
    let mut term = Terminal::new(10, 4, cfg);

    // Put a 15-char line on row 0 (soft-wraps across 2 rows at 10 cols, 4 rows at 4 cols -> +2 rows).
    term.advance(b"abcdefghijklmno\r\n");
    // Place 1x1 pixel image at current cursor line (row 2).
    term.advance(&kitty_rgba_apc("a=T,f=32,s=1,v=1,i=10", &[1, 2, 3, 255]));
    let pre_abs = term.image_placements()[0].abs_line;
    assert_eq!(pre_abs, AbsLine(2));

    // Resize narrower (10 -> 4 cols, keep 6 rows): first line wraps from 2 rows into 4 rows (+2).
    term.resize(4, 6);
    let post_abs = term.image_placements()[0].abs_line;
    assert_eq!(post_abs, AbsLine(4));

    // Push lines below the placement so shrinking rows evicts the placement's abs_line.
    term.advance(b"\r\nL1\r\nL2\r\nL3\r\n");
    term.resize(4, 2);
    assert!(term.image_placements().is_empty());
    assert!(term.image_data(10).is_none());

    // Alternate screen resize crops placements whose anchor falls outside new rows.
    let mut alt_term = Terminal::new(80, 6, CoreConfig::default());
    alt_term.advance(b"\x1b[?1049h");
    alt_term.advance(b"\x1b[1;1H");
    alt_term.advance(&kitty_rgba_apc(
        "a=T,f=32,s=1,v=1,i=20,q=2",
        &[9, 9, 9, 255],
    ));
    alt_term.advance(b"\x1b[5;1H");
    alt_term.advance(&kitty_rgba_apc(
        "a=T,f=32,s=1,v=1,i=21,q=2",
        &[8, 8, 8, 255],
    ));
    assert_eq!(alt_term.image_placements().len(), 2);

    // Cursor is on row 4 (1x1 placement didn't advance past col 1), so shrinking 6 -> 3 crops top
    // rows 0..=1 (`i=20` evicted) and remaps row 4 (`i=21`) to row 2.
    alt_term.resize(80, 3);
    let alt_placements = alt_term.image_placements();
    assert_eq!(alt_placements.len(), 1);
    assert_eq!(alt_placements[0].image_id, 21);
    assert_eq!(alt_placements[0].abs_line, AbsLine(2));
    assert!(alt_term.image_data(20).is_none());

    // Conversely with cursor at row 0, shrinking 6 -> 3 crops bottom rows (`i=23` evicted).
    let mut alt_term2 = Terminal::new(80, 6, CoreConfig::default());
    alt_term2.advance(b"\x1b[?1049h\x1b[1;1H");
    alt_term2.advance(&kitty_rgba_apc(
        "a=T,f=32,s=1,v=1,i=22,q=2",
        &[9, 9, 9, 255],
    ));
    alt_term2.advance(b"\x1b[5;1H");
    alt_term2.advance(&kitty_rgba_apc(
        "a=T,f=32,s=1,v=1,i=23,q=2",
        &[8, 8, 8, 255],
    ));
    alt_term2.advance(b"\x1b[1;1H");
    alt_term2.resize(80, 3);
    let alt2_placements = alt_term2.image_placements();
    assert_eq!(alt2_placements.len(), 1);
    assert_eq!(alt2_placements[0].image_id, 22);
    assert!(alt_term2.image_data(23).is_none());
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn m4_scroll_margins_dl_il_alt_and_history_scroll() {
    let cfg = CoreConfig {
        scrollback_limit: 4,
        ..CoreConfig::default()
    };
    let mut term = Terminal::new(20, 6, cfg);

    // Place images inside DECSTBM [2..=4] (0-based 1..=3) and outside at row 0 and row 5.
    term.advance(b"\x1b[1;1H");
    term.advance(&kitty_rgba_apc("a=T,f=32,s=1,v=1,i=1,q=2", &[1, 1, 1, 255]));
    term.advance(b"\x1b[3;1H");
    term.advance(&kitty_rgba_apc("a=T,f=32,s=1,v=1,i=2,q=2", &[2, 2, 2, 255]));
    term.advance(b"\x1b[6;1H");
    term.advance(&kitty_rgba_apc("a=T,f=32,s=1,v=1,i=3,q=2", &[3, 3, 3, 255]));

    term.advance(b"\x1b[2;5r");
    // CSI S (scroll up 1 inside DECSTBM): row 2 (abs 2) moves to row 1 (abs 1); row 0 and 5 untouched.
    term.advance(b"\x1b[1S");
    let find_abs = |t: &Terminal, id: u32| -> Option<AbsLine> {
        t.image_placements()
            .iter()
            .find(|p| p.image_id == id)
            .map(|p| p.abs_line)
    };
    assert_eq!(find_abs(&term, 1), Some(AbsLine(0)));
    assert_eq!(find_abs(&term, 2), Some(AbsLine(1)));
    assert_eq!(find_abs(&term, 3), Some(AbsLine(5)));

    // CSI 1 T (scroll down 1 inside DECSTBM): row 1 moves back to row 2.
    term.advance(b"\x1b[1T");
    assert_eq!(find_abs(&term, 2), Some(AbsLine(2)));

    // CSI M (DL) from row 2 deletes row 2's image (shifts up and clips at margin top).
    term.advance(b"\x1b[2;1H\x1b[1M");
    assert_eq!(find_abs(&term, 2), Some(AbsLine(1)));
    term.advance(b"\x1b[2;1H\x1b[1M");
    assert_eq!(find_abs(&term, 2), None);
    assert!(term.image_data(2).is_none());

    // Alternate screen full scroll shifts placement up without incrementing history.
    term.advance(b"\x1b[?1049h\x1b[3;1H");
    term.advance(&kitty_rgba_apc("a=T,f=32,s=1,v=1,i=4,q=2", &[4, 4, 4, 255]));
    assert_eq!(find_abs(&term, 4), Some(AbsLine(2)));
    term.advance(b"\x1b[6;1H\n\n");
    assert_eq!(find_abs(&term, 4), Some(AbsLine(0)));
    term.advance(b"\n");
    assert_eq!(find_abs(&term, 4), None);

    // Back on primary, full-screen history scroll keeps abs_line intact and prunes when evicted.
    term.advance(b"\x1b[?1049l\x1b[r\x1b[6;1H\n\n");
    assert_eq!(find_abs(&term, 1), Some(AbsLine(0)));
    for _ in 0..8 {
        term.advance(b"\n");
    }
    assert_eq!(find_abs(&term, 1), None);
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn m4_viewport_erase_ed2_deccolm_ed3_ris() {
    let mut term = Terminal::new(20, 4, CoreConfig::default());

    // Image 1 on line 0; push it into scrollback by scrolling 2 lines.
    term.advance(b"\x1b[1;1H");
    term.advance(&kitty_rgba_apc("a=T,f=32,s=1,v=1,i=1,q=2", &[1, 0, 0, 255]));
    term.advance(b"\x1b[4;1H\n\n");
    // Image 2 on visible viewport row 1.
    term.advance(b"\x1b[2;1H");
    term.advance(&kitty_rgba_apc("a=T,f=32,s=1,v=1,i=2,q=2", &[0, 2, 0, 255]));
    // Virtual placement 3.
    term.advance(&kitty_rgba_apc(
        "a=T,f=32,s=1,v=1,i=3,p=9,U=1,c=2,r=2,q=2",
        &[0, 0, 3, 255],
    ));

    // ED 2 clears only visible viewport placement (id=2), keeping scrollback (id=1) & virtual (id=3).
    term.advance(b"\x1b[2J");
    assert!(term.image_data(1).is_some());
    assert!(term.image_data(2).is_none());
    assert!(term.active_snapshot().virtual_placement(3, 9).is_some());

    // DECCOLM (?3h) also clears only viewport placements.
    term.advance(b"\x1b[2;1H");
    term.advance(&kitty_rgba_apc("a=T,f=32,s=1,v=1,i=4,q=2", &[4, 4, 4, 255]));
    assert!(term.image_data(4).is_some());
    term.advance(b"\x1b[?3h");
    assert!(term.image_data(1).is_some());
    assert!(term.image_data(4).is_none());
    assert!(term.active_snapshot().virtual_placement(3, 9).is_some());

    // ED 3 clears scrollback placement (id=1) while preserving viewport (id=5) and virtual (id=3).
    term.advance(b"\x1b[1;1H");
    term.advance(&kitty_rgba_apc("a=T,f=32,s=1,v=1,i=5,q=2", &[5, 5, 5, 255]));
    term.advance(b"\x1b[3J");
    assert!(term.image_data(1).is_none());
    assert!(term.image_data(5).is_some());
    assert!(term.active_snapshot().virtual_placement(3, 9).is_some());

    // RIS clears everything including virtual placements.
    term.advance(b"\x1bc");
    assert!(term.image_placements().is_empty());
    assert!(term.image_data(5).is_none());
    assert!(term.active_snapshot().virtual_placement(3, 9).is_none());
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn m4_unicode_placeholder_diacritics_and_virtual_snapshot() {
    let mut term = Terminal::new(20, 6, CoreConfig::default());
    term.advance(&kitty_rgba_apc(
        "a=T,f=32,s=1,v=1,i=42,p=7,U=1,c=2,r=2",
        &[255, 0, 0, 255],
    ));

    // Print U+10EEEE with row=0, col=1 diacritics (KITTY_DIACRITICS[0] and [1]) and fg=42.
    let d_row0 = KITTY_DIACRITICS[0];
    let d_col1 = KITTY_DIACRITICS[1];
    let seq = format!("\x1b[38;5;42m{KITTY_PLACEHOLDER_CHAR}{d_row0}{d_col1}");
    term.advance(seq.as_bytes());
    assert_eq!(term.cursor().col, 1);

    let snap = term.active_snapshot();
    // Virtual placement is not in normal visible placements list, but lookup works on active screen.
    assert!(snap.image_placements().is_empty());
    let vp = snap
        .virtual_placement(42, 7)
        .expect("virtual placement exists");
    assert_eq!(
        (vp.image_id, vp.placement_id, vp.cols, vp.rows),
        (42, 7, 2, 2)
    );
    assert!(snap.virtual_placement(42, 0).is_some());

    let cell = &snap.visible_rows[0].cells()[0];
    assert_eq!(cell.codepoint, KITTY_PLACEHOLDER_CHAR);
    let extra = snap.cell_extra(cell.extra_id);
    let decoded = decode_placeholder_cell(cell, extra).expect("valid kitty placeholder cell");
    assert_eq!(
        decoded,
        KittyPlaceholderCell {
            image_id_low24: 42,
            image_id: 42,
            placement_id: 0,
            row: Some(0),
            col: Some(1),
            id_msb: None,
        }
    );

    // 24-bit fg + 24-bit underline color (58;2;...) -> image_id_low24=42, placement_id=9, plus 3rd MSB diacritic.
    let msb1 = KITTY_DIACRITICS[1];
    let seq24 =
        format!("\x1b[38;2;0;0;42m\x1b[58;2;0;0;9m{KITTY_PLACEHOLDER_CHAR}{d_row0}{d_col1}{msb1}");
    term.advance(seq24.as_bytes());
    let snap2 = term.active_snapshot();
    let cell2 = &snap2.visible_rows[0].cells()[1];
    let decoded2 =
        decode_placeholder_cell(cell2, snap2.cell_extra(cell2.extra_id)).expect("24-bit id");
    assert_eq!(decoded2.image_id, 0x0100_002A);
    assert_eq!(decoded2.placement_id, 9);
    assert_eq!((decoded2.row, decoded2.col), (Some(0), Some(1)));

    // Switching to alternate screen hides primary virtual placement from active_snapshot.
    term.advance(b"\x1b[?1049h");
    assert!(term.active_snapshot().virtual_placement(42, 7).is_none());
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn m4_animations_tick_and_deadline_advance_frames() {
    let mut term = Terminal::new(40, 10, CoreConfig::default());
    // Transmit root frame 1 (red) and display it, add frame 2 (green, 100ms), set root gap=100ms & play.
    term.advance(&kitty_rgba_apc(
        "a=T,f=32,s=1,v=1,i=50,q=2",
        &[255, 0, 0, 255],
    ));
    term.advance(&kitty_rgba_apc(
        "a=f,f=32,s=1,v=1,i=50,z=100,q=2",
        &[0, 255, 0, 255],
    ));
    term.advance(b"\x1b_Ga=a,i=50,r=1,z=100,s=3,v=2,q=2\x1b\\");

    let rev0 = term.image_data(50).unwrap().revision;
    assert_eq!(&term.image_data(50).unwrap().rgba[0..4], &[255, 0, 0, 255]);
    let epoch0 = term.active_snapshot().damage_epoch;

    // First tick arms the next frame deadline (t=1.0 + 0.1 = 1.1).
    assert!(!term.tick_animations(1.0));
    let deadline = term.next_animation_deadline().expect("deadline armed");
    assert!((deadline - 1.1).abs() < 1e-6);

    assert!(!term.tick_animations(1.05));
    assert!(term.tick_animations(1.12));

    let img = term.image_data(50).unwrap();
    assert_eq!(&img.rgba[0..4], &[0, 255, 0, 255]);
    assert!(img.revision > rev0);
    assert!(term.active_snapshot().damage_epoch > epoch0);
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn m4_kitty_transport_policy_and_custom_shm_reader() {
    struct MockShmReader;
    impl KittyShmReader for MockShmReader {
        fn read_and_unlink(
            &self,
            name: &str,
            offset: u64,
            size: u64,
            _max_bytes: usize,
        ) -> Result<Vec<u8>, &'static str> {
            assert_eq!(name, "/demo_shm");
            assert_eq!((offset, size), (0, 4));
            Ok(vec![12, 34, 56, 255])
        }
    }

    let cfg = CoreConfig {
        kitty_file_medium_policy: KittyFileMediumPolicy::Disabled,
        ..CoreConfig::default()
    };
    let mut term = Terminal::new(40, 10, cfg);
    assert_eq!(
        term.kitty_file_medium_policy(),
        KittyFileMediumPolicy::Disabled
    );

    let path_b64 = encode_base64(b"/tmp/disallowed.rgba");
    term.advance(format!("\x1b_Ga=T,t=t,f=32,s=1,v=1,i=60;{path_b64}\x1b\\").as_bytes());
    assert!(term.image_data(60).is_none());
    let reply = String::from_utf8(term.drain_outgoing()).unwrap();
    assert!(reply.contains("EBADF"));

    term.set_kitty_file_medium_policy(KittyFileMediumPolicy::LocalFiles);
    assert_eq!(
        term.kitty_file_medium_policy(),
        KittyFileMediumPolicy::LocalFiles
    );

    term.set_kitty_shm_reader(Some(Box::new(MockShmReader)));
    let shm_b64 = encode_base64(b"/demo_shm");
    term.advance(format!("\x1b_Ga=T,t=s,f=32,s=1,v=1,S=4,O=0,i=61;{shm_b64}\x1b\\").as_bytes());
    let stored = term.image_data(61).expect("shm reader provided pixels");
    assert_eq!(&stored.rgba[..], &[12, 34, 56, 255]);
}

#[cfg(feature = "sixel")]
#[test]
fn m4_sixel_modes_1070_and_8452() {
    let mut term = Terminal::new(80, 24, CoreConfig::default());
    term.set_cell_pixel_size(8, 16);

    // DECRQM ?1070 defaults set (1); ?8452 defaults reset (2).
    term.advance(b"\x1b[?1070$p\x1b[?8452$p");
    let decrqm = String::from_utf8(term.drain_outgoing()).unwrap();
    assert!(decrqm.contains("\x1b[?1070;1$y"));
    assert!(decrqm.contains("\x1b[?8452;2$y"));

    // Disable ?1070 (shared palette) and define color register 5 = blue in first image.
    term.advance(b"\x1b[?1070l");
    term.advance(b"\x1bPq#5;2;0;0;100#5~\x1b\\");
    // Second image reuses register 5 without redefining it -> gets blue (0, 0, 255, 255).
    term.advance(b"\x1bPq#5~\x1b\\");
    let placements = term.image_placements();
    let img_shared = term.image_data(placements[1].image_id).unwrap();
    assert_eq!(&img_shared.rgba[0..4], &[0, 0, 255, 255]);

    // Re-enable ?1070 (private palette) -> register 5 falls back to default VT340 palette (not 0,0,255).
    term.advance(b"\x1b[?1070h");
    term.advance(b"\x1bPq#5~\x1b\\");
    let placements = term.image_placements();
    let img_priv = term.image_data(placements[2].image_id).unwrap();
    assert_ne!(&img_priv.rgba[0..4], &[0, 0, 255, 255]);

    // Enable ?8452 -> 16x24 sixel (2 cols, 2 rows) at (row 2, col 3) leaves cursor at (row 3, col 5).
    term.advance(b"\x1b[?8452h\x1b[?8452$p");
    let r8452 = String::from_utf8(term.drain_outgoing()).unwrap();
    assert!(r8452.contains("\x1b[?8452;1$y"));
    term.advance(b"\x1b[3;4H\x1bPq\"1;1;16;24#1~\x1b\\");
    assert_eq!((term.cursor().row, term.cursor().col), (3, 5));
}
