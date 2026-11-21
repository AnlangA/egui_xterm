//! Wave 3 opaque-box E2E tests for Inline Graphics completeness:
//! - Item #8: Kitty Graphics Completeness (`t=f/t/s`, concurrent chunked `m=1/0`, animations `a=f/a/c`,
//!   Unicode placeholders `U=1`/`U+10EEEE`, relative placements `P/Q/H/V`, PNG Adam7+16-bit+CRC-32,
//!   `abs_line` reflow, dynamic `set_cell_pixel_size`, delete targets `d=a..z/A..Z`)
//! - Item #9: Sixel Graphics Completeness & Perf (dynamic `set_cell_pixel_size`, `Pan`/`Pad` aspect
//!   ratio, `P2` transparency `0/1/2`, `DECSDM ?80`, `?1070`, `?8452`, growing canvas bounds)
//!
//! Test inventory (33 tests total, matching `TEST_INFRA.md` Wave 3 items #8 & #9):
//! - Item #8 Tier 1 (8): `t1_item8_01` .. `t1_item8_08`
//! - Item #9 Tier 1 (6): `t1_item9_01` .. `t1_item9_06`
//! - Item #8 Tier 2 (8): `t2_item8_01` .. `t2_item8_08`
//! - Item #9 Tier 2 (6): `t2_item9_01` .. `t2_item9_06`
//! - Item #8 Tier 3 (2): `t3_item8_01` .. `t3_item8_02`
//! - Item #9 Tier 3 (2): `t3_item9_01` .. `t3_item9_02`
//! - Tier 4 Workload (1): `w_gfx_01_terminal_image_viewer_animation_sixel_and_reflow`

#![forbid(unsafe_code)]

#[cfg(feature = "kitty-graphics")]
mod kitty_helpers {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

    pub(crate) struct TempFileGuard(pub PathBuf);

    impl Drop for TempFileGuard {
        fn drop(&mut self) {
            drop(std::fs::remove_file(&self.0));
        }
    }

    pub(crate) fn unique_temp_path(tag: &str, ext: &str) -> PathBuf {
        let seq = NEXT_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{tag}-{}-{seq}.{ext}", std::process::id()))
    }

    pub(crate) fn encode_base64(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let b0 = u32::from(chunk[0]);
            let b1 = chunk.get(1).copied().map_or(0, u32::from);
            let b2 = chunk.get(2).copied().map_or(0, u32::from);
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

    pub(crate) fn kitty_apc(params: &str, payload: &[u8]) -> Vec<u8> {
        let b64 = encode_base64(payload);
        format!("\x1b_G{params};{b64}\x1b\\").into_bytes()
    }

    pub(crate) fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(0x78);
        out.push(0x01);
        let mut pos = 0usize;
        while pos < data.len() || pos == 0 {
            let end = (pos + 65535).min(data.len());
            let is_final = end == data.len();
            out.push(u8::from(is_final));
            let len = u16::try_from(end - pos).unwrap_or(u16::MAX);
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

    pub(crate) fn ieee_crc32(kind: &[u8; 4], data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in kind.iter().chain(data.iter()) {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
            }
        }
        !crc
    }

    pub(crate) fn push_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&ieee_crc32(kind, data).to_be_bytes());
    }

    pub(crate) fn make_ihdr(
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

    pub(crate) fn build_png(
        ihdr: [u8; 13],
        plte: Option<&[u8]>,
        trns: Option<&[u8]>,
        raw_scanlines: &[u8],
    ) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        push_png_chunk(&mut out, b"IHDR", &ihdr);
        if let Some(p) = plte {
            push_png_chunk(&mut out, b"PLTE", p);
        }
        if let Some(t) = trns {
            push_png_chunk(&mut out, b"tRNS", t);
        }
        let idat = zlib_stored(raw_scanlines);
        push_png_chunk(&mut out, b"IDAT", &idat);
        push_png_chunk(&mut out, b"IEND", &[]);
        out
    }

    pub(crate) fn build_png_from_chunks(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        for &(kind, data) in chunks {
            push_png_chunk(&mut out, kind, data);
        }
        out
    }

    const ADAM7_PASSES: [(usize, usize, usize, usize); 7] = [
        (0, 0, 8, 8),
        (4, 0, 8, 8),
        (0, 4, 4, 8),
        (2, 0, 4, 4),
        (0, 2, 2, 4),
        (1, 0, 2, 2),
        (0, 1, 1, 2),
    ];

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

    pub(crate) fn encode_adam7_filtered(
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
            _ => 0,
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
                        _ => cur,
                    };
                    out.push(filt);
                }
            }
        }
        out
    }
}

// ===========================================================================
// Item #8 — Kitty Graphics Completeness: Tier 1 (8 Equivalence-Class Tests)
// ===========================================================================

#[cfg(feature = "kitty-graphics")]
mod item8_tier1 {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use egui_xterm_core::{
        AbsLine, Cell, CellExtra, CellFlags, CoreConfig, KITTY_DIACRITICS, KITTY_PLACEHOLDER_CHAR,
        KittyFileMediumPolicy, KittyPlaceholderCell, KittyShmReader, PackedColor, Terminal,
        decode_placeholder_cell, diacritic_index,
    };
    use egui_xterm_e2e::HeadlessTermHarness;

    use super::kitty_helpers::*;

    #[test]
    fn t1_item8_01_kitty_file_temp_and_shm_mediums_with_slicing_and_egui_render() {
        struct RecordingShmReader {
            unlinked: Arc<AtomicUsize>,
        }
        impl KittyShmReader for RecordingShmReader {
            fn read_and_unlink(
                &self,
                name: &str,
                offset: u64,
                size: u64,
                max_bytes: usize,
            ) -> Result<Vec<u8>, &'static str> {
                if name != "/wave3-kitty-shm" {
                    return Err("EBADF:Failed to read image file");
                }
                let raw = [0u8, 11, 22, 33, 44, 55, 66, 77, 88, 99];
                let start = usize::try_from(offset).map_err(|_| "EBADF:offset overflow")?;
                let len = usize::try_from(size).map_err(|_| "EBADF:size overflow")?;
                if len > max_bytes || start.saturating_add(len) > raw.len() {
                    return Err("EBADF:out of bounds");
                }
                self.unlinked.fetch_add(1, Ordering::Relaxed);
                Ok(raw[start..start + len].to_vec())
            }
        }

        let temp_path = unique_temp_path("tty-graphics-protocol-wave3", "rgba");
        let reg_path = unique_temp_path("wave3-regular-keep", "rgba");
        let _reg_guard = TempFileGuard(reg_path.clone());
        let _temp_guard = TempFileGuard(temp_path.clone());

        let file_bytes = [
            9, 9, // 2-byte prefix skipped via O=2
            10, 20, 30, 255, 40, 50, 60, 255, // 2x1 RGBA (8 bytes via S=8)
            8, 8,
        ];
        std::fs::write(&temp_path, file_bytes).expect("write temp file");
        std::fs::write(&reg_path, file_bytes).expect("write regular file");

        let mut h = HeadlessTermHarness::scripted();
        h.set_viewport_size(640.0, 360.0);
        drop(h.run_frame());

        let shm_unlinked = Arc::new(AtomicUsize::new(0));
        h.runtime
            .terminal_mut()
            .set_kitty_shm_reader(Some(Box::new(RecordingShmReader {
                unlinked: Arc::clone(&shm_unlinked),
            })));
        assert_eq!(
            h.runtime.terminal().kitty_file_medium_policy(),
            KittyFileMediumPolicy::LocalFiles
        );

        // 1. t=f regular file with O=2, S=8 stays on disk after load and renders in egui.
        let b64_reg = encode_base64(reg_path.to_string_lossy().as_bytes());
        h.runtime.input_mut().push_str(format!(
            "\x1b_Ga=T,i=101,t=f,f=32,s=2,v=1,c=4,r=2,O=2,S=8;{b64_reg}\x1b\\"
        ));
        let f1 = h.run_frame();
        assert_eq!(h.runtime.input().written_str(), "\x1b_Gi=101;OK\x1b\\");
        h.runtime.input_mut().clear_written();
        assert!(reg_path.exists(), "t=f must never unlink regular file");
        let img101 = h
            .runtime
            .terminal()
            .image_data(101)
            .expect("image 101 loaded from t=f");
        assert_eq!((img101.width, img101.height), (2, 1));
        assert_eq!(img101.active_rgba(), &[10, 20, 30, 255, 40, 50, 60, 255]);
        let tex_101 = f1
            .texture_deltas
            .set_entries
            .iter()
            .find(|e| e.size == [2, 1])
            .expect("t=f texture uploaded")
            .texture_id;
        let shapes1 = f1.inspect_shapes();
        assert!(shapes1.mesh_shape_count >= 1);
        assert!(shapes1.mesh_texture_ids.contains(&tex_101));

        // 2. t=t temp file under TempOnly policy reads O=2,S=8 and unlinks the temp file.
        h.runtime
            .terminal_mut()
            .set_kitty_file_medium_policy(KittyFileMediumPolicy::TempOnly);
        let b64_temp = encode_base64(temp_path.to_string_lossy().as_bytes());
        h.runtime.input_mut().push_str(format!(
            "\x1b_Ga=T,i=102,t=t,f=32,s=2,v=1,O=2,S=8;{b64_temp}\x1b\\"
        ));
        let f2 = h.run_frame();
        assert_eq!(h.runtime.input().written_str(), "\x1b_Gi=102;OK\x1b\\");
        h.runtime.input_mut().clear_written();
        assert!(
            !temp_path.exists(),
            "t=t under TempOnly must unlink tty-graphics-protocol temp file"
        );
        assert_eq!(
            h.runtime
                .terminal()
                .image_data(102)
                .expect("image 102 loaded from t=t")
                .active_rgba(),
            &[10, 20, 30, 255, 40, 50, 60, 255]
        );
        assert!(!f2.texture_deltas.set_entries.is_empty());

        // 3. t=s shared memory via host KittyShmReader under LocalFiles.
        h.runtime
            .terminal_mut()
            .set_kitty_file_medium_policy(KittyFileMediumPolicy::LocalFiles);
        let b64_shm = encode_base64(b"/wave3-kitty-shm");
        h.runtime.input_mut().push_str(format!(
            "\x1b_Ga=T,i=103,t=s,f=32,s=2,v=1,O=1,S=8;{b64_shm}\x1b\\"
        ));
        let f3 = h.run_frame();
        assert_eq!(h.runtime.input().written_str(), "\x1b_Gi=103;OK\x1b\\");
        assert_eq!(shm_unlinked.load(Ordering::Relaxed), 1);
        assert_eq!(
            h.runtime
                .terminal()
                .image_data(103)
                .expect("image 103 loaded from t=s")
                .active_rgba(),
            &[11, 22, 33, 44, 55, 66, 77, 88]
        );
        assert!(!f3.texture_deltas.set_entries.is_empty());
        assert!(!f3.inspect_shapes().mesh_vertex_positions().is_empty());
    }

    #[test]
    fn t1_item8_02_kitty_concurrent_multi_id_chunked_transfers() {
        let mut h = HeadlessTermHarness::scripted();
        drop(h.run_frame());

        // Start 3 concurrent chunked transfers: ImageId(10), ImageNumber(77), Anonymous (0,0).
        let px_id = vec![1, 2, 3, 255, 4, 5, 6, 255];
        let b64_id = encode_base64(&px_id);
        let (id_c1, id_c2) = b64_id.split_at(5);

        let px_num = vec![70, 80, 90, 255];
        let b64_num = encode_base64(&px_num);
        let (num_c1, num_c2) = b64_num.split_at(4);

        let px_anon = vec![200, 150, 100, 255];
        let b64_anon = encode_base64(&px_anon);
        let (anon_c1, anon_c2) = b64_anon.split_at(4);

        h.runtime.input_mut().push_str(format!(
            "\x1b_Ga=T,i=10,f=32,s=2,v=1,c=2,r=1,m=1;{id_c1}\x1b\\\
             \x1b_Ga=t,I=77,f=32,s=1,v=1,m=1;{num_c1}\x1b\\\
             \x1b_Ga=t,f=32,s=1,v=1,m=1;{anon_c1}\x1b\\"
        ));
        drop(h.run_frame());
        assert!(h.runtime.terminal().image_data(10).is_none());
        assert!(h.runtime.input().written_bytes().is_empty());

        // Mid-transfer query (a=q) and feature query must run immediately without getting swallowed.
        let q_b64 = encode_base64(&[9, 9, 9, 255]);
        h.runtime.input_mut().push_str(format!(
            "\x1b_Ga=q,i=99,f=32,s=1,v=1;{q_b64}\x1b\\\
             \x1b_Ga=q,i=98,e=T,U=1,C=1;\x1b\\"
        ));
        drop(h.run_frame());
        let mid_reply = h.runtime.input().written_str();
        assert_eq!(
            mid_reply,
            "\x1b_Gi=99;OK\x1b\\\x1b_Gi=98,e=T,C=1,U=1;OK\x1b\\"
        );
        h.runtime.input_mut().clear_written();

        // Finish transfers in interleaved order (I=77, i=10, anonymous).
        h.runtime.input_mut().push_str(format!(
            "\x1b_GI=77,m=0;{num_c2}\x1b\\\
             \x1b_Gi=10,m=0;{id_c2}\x1b\\\
             \x1b_Gm=0;{anon_c2}\x1b\\"
        ));
        let done_frame = h.run_frame();
        let reply = h.runtime.input().written_str();
        assert!(reply.contains("\x1b_Gi=10;OK\x1b\\"), "reply={reply:?}");
        assert_eq!(reply.matches(";OK\x1b\\").count(), 3, "reply={reply:?}");

        let term = h.runtime.terminal();
        assert_eq!(term.image_data(10).expect("id 10").active_rgba(), px_id);
        let id_from_num = term
            .image_placements()
            .iter()
            .find(|p| p.image_id == 10)
            .map(|p| p.image_id);
        assert_eq!(id_from_num, Some(10));

        // Put the I=77 image via a=p,I=77 to prove ImageNumber resolved to a live stored image.
        h.runtime.input_mut().clear_written();
        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=p,I=77,p=3,c=1,r=1,C=1;\x1b\\");
        let put_frame = h.run_frame();
        let put_reply = h.runtime.input().written_str();
        assert!(
            put_reply.starts_with("\x1b_Gi=") && put_reply.ends_with(",p=3;OK\x1b\\"),
            "put_reply={put_reply:?}"
        );
        assert!(!done_frame.texture_deltas.set_entries.is_empty());
        assert!(
            !put_frame
                .inspect_shapes()
                .mesh_vertex_positions()
                .is_empty()
        );
        let p_num = h
            .runtime
            .terminal()
            .image_placements()
            .iter()
            .find(|p| p.placement_id == 3)
            .expect("I=77 placement");
        assert_eq!(
            h.runtime
                .terminal()
                .image_data(p_num.image_id)
                .expect("I=77 image data")
                .active_rgba(),
            px_num
        );
        // Anonymous image was also allocated and stored (distinct from 10 and p_num.image_id).
        assert_ne!(p_num.image_id, 10);
        let anon_id = (1..=100u32)
            .find(|&cand| {
                cand != 10
                    && cand != p_num.image_id
                    && h.runtime
                        .terminal()
                        .image_data(cand)
                        .is_some_and(|img| img.active_rgba() == px_anon.as_slice())
            })
            .expect("anonymous image stored");
        assert_eq!(
            h.runtime
                .terminal()
                .image_data(anon_id)
                .expect("anon")
                .active_rgba(),
            px_anon
        );
    }

    #[test]
    fn t1_item8_03_kitty_animations_frames_compose_tick_and_revision() {
        let mut h = HeadlessTermHarness::scripted();
        drop(h.run_frame());

        // Frame 1: 2x1 opaque red.
        h.runtime.input_mut().push_bytes(kitty_apc(
            "a=T,i=20,f=32,s=2,v=1,c=2,r=1",
            &[255, 0, 0, 255, 255, 0, 0, 255],
        ));
        let init_frame = h.run_frame();
        assert!(!init_frame.texture_deltas.set_entries.is_empty());
        let rev1 = h
            .runtime
            .terminal()
            .image_data(20)
            .expect("frame 1")
            .revision;
        assert!(rev1 >= 1);

        // Frame 2: copy frame 1 (c=1), overwrite px(1,0) with green (X=1), gap=80ms.
        h.runtime.input_mut().push_bytes(kitty_apc(
            "a=f,i=20,c=1,x=1,y=0,f=32,s=1,v=1,X=1,z=80",
            &[0, 255, 0, 255],
        ));
        // Frame 3: solid blue fill (Y=0x0000FFFF), gap=60ms, then compose (0,0) of frame 1 onto (1,0) of frame 3.
        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=f,i=20,Y=65535,z=60;\x1b\\");
        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=c,i=20,r=1,c=3,w=1,h=1,X=0,Y=0,x=1,y=0,C=1;\x1b\\");
        // Set frame 1 gap to 50ms and start Playing (s=3, v=2 => 1 loop).
        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=a,i=20,r=1,z=50,s=3,v=2;\x1b\\");
        drop(h.run_frame());

        let term = h.runtime.terminal_mut();
        assert!(term.next_animation_deadline().is_some());
        let epoch0 = term.damage_epoch();

        // First tick at t=1.000 anchors deadline at 1.050 without switching frames.
        assert!(!term.tick_animations(1.000));
        let d1 = term.next_animation_deadline().expect("deadline armed");
        assert!((d1 - 1.050).abs() < 1e-6);

        // Tick at t=1.050 advances to frame 2 (red, green), bumps revision & damage_epoch.
        assert!(term.tick_animations(1.050));
        let img_f2 = term.image_data(20).expect("frame 2");
        assert!(img_f2.revision > rev1);
        let rev2 = img_f2.revision;
        assert_eq!(img_f2.active_rgba(), &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert!(term.damage_epoch() > epoch0);
        assert!((term.next_animation_deadline().expect("d2") - 1.130).abs() < 1e-6);

        // Tick at t=1.130 advances to frame 3 (blue, red) and bumps revision again.
        assert!(term.tick_animations(1.130));
        let img_f3 = term.image_data(20).expect("frame 3");
        assert!(img_f3.revision > rev2);
        let rev3 = img_f3.revision;
        assert_eq!(img_f3.active_rgba(), &[0, 0, 255, 255, 255, 0, 0, 255]);

        // Tick at t=1.200 completes the single loop and stops.
        assert!(!term.tick_animations(1.200));
        assert!(term.next_animation_deadline().is_none());

        // Manual frame selection a=a,c=2 switches active frame and bumps revision.
        h.runtime.input_mut().push_str("\x1b_Ga=a,i=20,c=2;\x1b\\");
        drop(h.run_frame());
        let img_manual = h.runtime.terminal().image_data(20).expect("manual frame 2");
        assert!(img_manual.revision > rev3);
        assert_eq!(img_manual.active_rgba(), &[255, 0, 0, 255, 0, 255, 0, 255]);

        // Delete extra frames d=f reverts back to root frame 1 and bumps revision.
        let rev4 = img_manual.revision;
        h.runtime.input_mut().push_str("\x1b_Ga=d,d=f,i=20;\x1b\\");
        drop(h.run_frame());
        let img_reset = h.runtime.terminal().image_data(20).expect("root frame");
        assert!(img_reset.revision > rev4);
        assert_eq!(img_reset.active_rgba(), &[255, 0, 0, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn t1_item8_04_kitty_unicode_placeholders_u1_diacritics_and_inheritance() {
        assert_eq!(KITTY_DIACRITICS.len(), 297);
        assert_eq!(diacritic_index('\u{0305}'), Some(0));
        assert_eq!(diacritic_index('\u{1D244}'), Some(296));

        let mut h = HeadlessTermHarness::scripted();
        h.set_viewport_size(640.0, 360.0);
        drop(h.run_frame());

        // Create virtual placement (i=42, p=7, U=1, c=3, r=2) and feature/query check.
        h.runtime.input_mut().push_str("\x1b_Ga=q,i=40,U=1;\x1b\\");
        h.runtime.input_mut().push_bytes(kitty_apc(
            "a=T,i=42,p=7,U=1,c=3,r=2,f=32,s=2,v=2",
            &[
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
            ],
        ));
        drop(h.run_frame());
        let out = h.runtime.input().written_str();
        assert!(out.contains("\x1b_Gi=40;OK\x1b\\"));
        assert!(out.contains("\x1b_Gi=42,p=7;OK\x1b\\"));

        // Print 3 placeholder cells on row 0:
        // col 0: explicit row=0, col=0, msb=1 (id = 0x01_00002A) + placement_id=7 (58;2;0;0;7)
        // col 1: bare U+10EEEE with same fg/underline -> inherits row=0, col=1, msb=1
        // col 2: explicit row=0, bare col -> inherits col=2, msb=1
        let d0 = KITTY_DIACRITICS[0];
        let d1 = KITTY_DIACRITICS[1];
        let seq = format!(
            "\x1b[1;1H\x1b[38;2;0;0;42m\x1b[58;2;0;0;7m\
             {KITTY_PLACEHOLDER_CHAR}{d0}{d0}{d1}\
             {KITTY_PLACEHOLDER_CHAR}\
             {KITTY_PLACEHOLDER_CHAR}{d0}\x1b[0m"
        );
        h.runtime.input_mut().push_str(seq);
        let frame = h.run_frame();
        assert!(frame.terminal_response.cols >= 10);

        let term = h.runtime.terminal();
        assert_eq!(term.image_placements().len(), 1);
        assert!(term.image_placements()[0].is_virtual);
        let snap = term.active_snapshot();
        assert!(
            snap.image_placements().is_empty(),
            "virtual U=1 placement must not appear in snapshot visible placements"
        );
        let vp = snap
            .virtual_placement(42, 7)
            .expect("virtual placement lookup");
        assert!(vp.is_virtual);
        assert_eq!((vp.cols, vp.rows), (3, 2));
        assert!(snap.virtual_placement(42, 0).is_some());

        let grid = term.grid();
        let row0 = &snap.visible_rows[0];
        let c0 = &row0.cells()[0];
        let c1 = &row0.cells()[1];
        let c2 = &row0.cells()[2];
        assert_eq!(c0.codepoint, KITTY_PLACEHOLDER_CHAR);

        let dec0 =
            decode_placeholder_cell(c0, grid.cell_extra(c0.extra_id)).expect("col 0 placeholder");
        assert_eq!(
            dec0,
            KittyPlaceholderCell {
                image_id_low24: 42,
                image_id: (1 << 24) | 42,
                placement_id: 7,
                row: Some(0),
                col: Some(0),
                id_msb: Some(1),
            }
        );

        let dec1 = decode_placeholder_cell(c1, grid.cell_extra(c1.extra_id))
            .expect("col 1 placeholder")
            .resolve_with_left(Some(dec0));
        assert_eq!(
            (dec1.image_id, dec1.placement_id, dec1.row, dec1.col),
            ((1 << 24) | 42, 7, Some(0), Some(1))
        );

        let dec2 = decode_placeholder_cell(c2, grid.cell_extra(c2.extra_id))
            .expect("col 2 placeholder")
            .resolve_with_left(Some(dec1));
        assert_eq!(
            (dec2.image_id, dec2.placement_id, dec2.row, dec2.col),
            ((1 << 24) | 42, 7, Some(0), Some(2))
        );

        // Also test indexed SGR 38;5;42 placeholder decoding directly.
        let idx_cell = Cell::new(
            KITTY_PLACEHOLDER_CHAR,
            PackedColor::indexed(42),
            PackedColor::Default,
            CellFlags::empty(),
        );
        let idx_extra = CellExtra {
            combining: vec![KITTY_DIACRITICS[2], KITTY_DIACRITICS[3]],
            underline_color: Some(PackedColor::indexed(9)),
            ..CellExtra::default()
        };
        let dec_idx = decode_placeholder_cell(&idx_cell, Some(&idx_extra)).expect("indexed cell");
        assert_eq!(
            (
                dec_idx.image_id,
                dec_idx.placement_id,
                dec_idx.row,
                dec_idx.col
            ),
            (42, 9, Some(2), Some(3))
        );
    }

    #[test]
    fn t1_item8_05_kitty_relative_placements_move_reflow_and_cascade_delete() {
        let mut term = Terminal::new(20, 12, CoreConfig::default());
        term.advance(b"wrap_line_0123456789abcde\r\n");

        // Upload 3 images and place root (1, 1), child (2, 2), grandchild (3, 3).
        term.advance(&kitty_apc(
            "a=T,i=1,p=1,f=32,s=1,v=1,c=2,r=1,q=2",
            &[1, 0, 0, 255],
        ));
        term.advance(&kitty_apc("a=t,i=2,f=32,s=1,v=1,q=2", &[0, 2, 0, 255]));
        term.advance(&kitty_apc("a=t,i=3,f=32,s=1,v=1,q=2", &[0, 0, 3, 255]));

        let root_line = term.image_placements()[0].abs_line;
        assert_eq!(root_line, AbsLine(2));

        term.advance(b"\x1b_Ga=p,i=2,p=2,P=1,Q=1,H=3,V=1,c=1,r=1,q=2\x1b\\");
        term.advance(b"\x1b_Ga=p,i=3,p=3,P=2,Q=2,H=-1,V=2,c=1,r=1,q=2\x1b\\");

        let placements = term.image_placements();
        assert_eq!(placements.len(), 3);
        assert_eq!((placements[1].abs_line, placements[1].col), (AbsLine(3), 3));
        assert_eq!((placements[2].abs_line, placements[2].col), (AbsLine(5), 2));

        // Replacing parent (1, 1) at row 1, col 4 re-resolves child & grandchild without deleting them!
        term.advance(b"\x1b[2;5H\x1b_Ga=p,i=1,p=1,c=2,r=1,C=1,q=2\x1b\\");
        let moved = term.image_placements();
        assert_eq!(moved.len(), 3);
        let root_pos = moved.iter().find(|p| p.image_id == 1).expect("root");
        let child_pos = moved.iter().find(|p| p.image_id == 2).expect("child");
        let grand_pos = moved.iter().find(|p| p.image_id == 3).expect("grandchild");
        assert_eq!((root_pos.abs_line, root_pos.col), (AbsLine(1), 4));
        assert_eq!((child_pos.abs_line, child_pos.col), (AbsLine(2), 7));
        assert_eq!((grand_pos.abs_line, grand_pos.col), (AbsLine(4), 6));

        // Narrow resize (20 -> 10 cols) wraps row 0 into 3 rows (+1 row) -> root + children stay aligned.
        term.resize(10, 12);
        let reflowed = term.image_placements();
        let r_ref = reflowed
            .iter()
            .find(|p| p.image_id == 1)
            .expect("root reflow");
        let c_ref = reflowed
            .iter()
            .find(|p| p.image_id == 2)
            .expect("child reflow");
        let g_ref = reflowed
            .iter()
            .find(|p| p.image_id == 3)
            .expect("grandchild reflow");
        assert_eq!(
            (c_ref.abs_line.0, c_ref.col),
            (r_ref.abs_line.0 + 1, r_ref.col + 3)
        );
        assert_eq!(
            (g_ref.abs_line.0, g_ref.col),
            (c_ref.abs_line.0 + 2, c_ref.col - 1)
        );

        // Deleting root (d=I,i=1,p=1) cascade-deletes child and grandchild and frees unreferenced images.
        term.advance(b"\x1b_Ga=d,d=I,i=1,p=1;\x1b\\");
        assert!(term.image_placements().is_empty());
        assert!(term.image_data(1).is_none());
        assert!(term.image_data(2).is_none());
        assert!(term.image_data(3).is_none());
    }

    #[test]
    fn t1_item8_06_kitty_png_adam7_16bit_subbyte_and_crc_end_to_end() {
        let mut h = HeadlessTermHarness::scripted();
        h.set_viewport_size(640.0, 360.0);
        drop(h.run_frame());

        // 1. 3x3 Adam7 RGBA8 with varied filters per pass row + empty passes 2 and 3.
        let expected_3x3 = |x: usize, y: usize| -> [u8; 4] {
            [
                u8::try_from(x * 40 + 10).unwrap_or(0),
                u8::try_from(y * 50 + 20).unwrap_or(0),
                u8::try_from((x + y) * 30).unwrap_or(0),
                255,
            ]
        };
        let raw_3x3 = encode_adam7_filtered(
            3,
            3,
            6,
            8,
            |x, y| expected_3x3(x, y).to_vec(),
            |pass_idx, py| u8::try_from((pass_idx + py) % 5).unwrap_or(0),
        );
        let png_adam7 = build_png(make_ihdr(3, 3, 8, 6, 1), None, None, &raw_3x3);
        let b64_adam7 = encode_base64(&png_adam7);
        h.runtime
            .input_mut()
            .push_str(format!("\x1b_Ga=T,i=201,f=100,c=2,r=2;{b64_adam7}\x1b\\"));
        let frame1 = h.run_frame();
        assert_eq!(h.runtime.input().written_str(), "\x1b_Gi=201;OK\x1b\\");
        h.runtime.input_mut().clear_written();
        let dec_3x3 = h
            .runtime
            .terminal()
            .image_data(201)
            .expect("Adam7 3x3 PNG stored");
        for y in 0..3 {
            for x in 0..3 {
                let off = (y * 3 + x) * 4;
                assert_eq!(&dec_3x3.active_rgba()[off..off + 4], &expected_3x3(x, y));
            }
        }
        let tex_201 = frame1
            .texture_deltas
            .set_entries
            .iter()
            .find(|e| e.size == [3, 3])
            .expect("Adam7 PNG uploaded to egui")
            .texture_id;
        assert!(frame1.inspect_shapes().mesh_texture_ids.contains(&tex_201));

        // 2. 16-bit RGB (color_type=2, bit_depth=16) with exact 16-bit tRNS matching only px 0.
        let rgb16_scan = [
            0, 0x11, 0x00, 0x22, 0x00, 0x33, 0x00, // px 0 matches tRNS -> alpha 0
            0x11, 0x00, 0x22, 0x00, 0x33, 0x01, // px 1 differs in LSB -> alpha 255
        ];
        let png_rgb16 = build_png(
            make_ihdr(2, 1, 16, 2, 0),
            None,
            Some(&[0x11, 0x00, 0x22, 0x00, 0x33, 0x00]),
            &rgb16_scan,
        );
        h.runtime
            .input_mut()
            .push_bytes(kitty_apc("a=t,i=202,f=100,q=2", &png_rgb16));

        // 3. 16-bit RGBA (color_type=6, bit_depth=16) Adam7 1x1 + zlib outer compression (o=z).
        let png_rgba16 = build_png(
            make_ihdr(1, 1, 16, 6, 1),
            None,
            None,
            &[0, 0xAA, 0x11, 0xBB, 0x22, 0xCC, 0x33, 0xDD, 0x44],
        );
        h.runtime.input_mut().push_bytes(kitty_apc(
            "a=t,i=203,f=100,o=z,q=2",
            &zlib_stored(&png_rgba16),
        ));

        // 4. 4-bit indexed (color_type=3, bit_depth=4) with PLTE + partial tRNS + 1-bit grayscale with tRNS.
        let png_i4 = build_png(
            make_ihdr(3, 1, 4, 3, 0),
            Some(&[255, 0, 0, 0, 255, 0, 0, 0, 255]),
            Some(&[64]),
            &[0, 0x01, 0x20],
        );
        h.runtime
            .input_mut()
            .push_bytes(kitty_apc("a=t,i=204,f=100,q=2", &png_i4));

        let png_g1 = build_png(make_ihdr(2, 1, 1, 0, 0), None, Some(&[0, 0]), &[0, 0x80]);
        h.runtime
            .input_mut()
            .push_bytes(kitty_apc("a=t,i=205,f=100,q=2", &png_g1));

        drop(h.run_frame());
        let term = h.runtime.terminal();
        assert_eq!(
            term.image_data(202).expect("rgb16").active_rgba(),
            &[0x11, 0x22, 0x33, 0, 0x11, 0x22, 0x33, 255]
        );
        assert_eq!(
            term.image_data(203).expect("rgba16").active_rgba(),
            &[0xAA, 0xBB, 0xCC, 0xDD]
        );
        assert_eq!(
            term.image_data(204).expect("idx4").active_rgba(),
            &[255, 0, 0, 64, 0, 255, 0, 255, 0, 0, 255, 255]
        );
        assert_eq!(
            term.image_data(205).expect("gray1").active_rgba(),
            &[255, 255, 255, 255, 0, 0, 0, 0]
        );
    }

    #[test]
    fn t1_item8_07_kitty_abs_line_reflow_scroll_and_dynamic_cell_pixel_size() {
        // 1. Dynamic set_cell_pixel_size controls Kitty cursor advance when c/r omitted.
        let mut term = Terminal::new(40, 12, CoreConfig::default());
        term.set_cell_pixel_size(12, 24);
        assert_eq!(term.cell_pixel_size(), (12, 24));
        let rgba_25x49 = vec![0x77u8; 25 * 49 * 4];
        term.advance(&kitty_apc("a=T,i=1,f=32,s=25,v=49,q=2", &rgba_25x49));
        // ceil(25/12)=3 cols, ceil(49/24)=3 rows -> 2 linefeeds + col 3.
        assert_eq!(term.cursor_pos(), (3, 2));

        term.set_cell_pixel_size(8, 16);
        term.advance(b"\x1b[1;1H");
        term.advance(&kitty_apc("a=T,i=2,f=32,s=25,v=49,q=2", &rgba_25x49));
        // ceil(25/8)=4 cols, ceil(49/16)=4 rows -> 3 linefeeds + col 4.
        assert_eq!(term.cursor_pos(), (4, 3));

        // 2. Primary reflow + DECSTBM scroll + ED 2 / DECCOLM / ED 3 / RIS.
        let cfg = CoreConfig {
            scrollback_limit: 8,
            ..CoreConfig::default()
        };
        let mut t2 = Terminal::new(16, 6, cfg);
        t2.advance(b"wrapped_0123456789abcdef\r\n");
        t2.advance(&kitty_apc("a=T,i=10,f=32,s=1,v=1,q=2", &[1, 1, 1, 255]));
        assert_eq!(t2.image_placements()[0].abs_line, AbsLine(2));

        // Resize narrower (16 -> 8): 24-char line goes from 2 rows to 3 rows (+1).
        t2.resize(8, 6);
        assert_eq!(t2.image_placements()[0].abs_line, AbsLine(3));

        // DECSTBM [2..=5] (0-based rows 1..=4): place img 11 inside margins (row 2) and img 12 at bottom (row 5).
        t2.advance(b"\x1b[2J\x1b[1;1H");
        t2.advance(b"\x1b[3;1H");
        t2.advance(&kitty_apc("a=T,i=11,f=32,s=1,v=1,q=2", &[2, 2, 2, 255]));
        t2.advance(b"\x1b[6;1H");
        t2.advance(&kitty_apc("a=T,i=12,f=32,s=1,v=1,q=2", &[3, 3, 3, 255]));
        let row_11_before = t2
            .image_placements()
            .iter()
            .find(|p| p.image_id == 11)
            .expect("img 11")
            .abs_line;
        let row_12_before = t2
            .image_placements()
            .iter()
            .find(|p| p.image_id == 12)
            .expect("img 12")
            .abs_line;
        t2.advance(b"\x1b[2;5r\x1b[1S");
        let row_11_after = t2
            .image_placements()
            .iter()
            .find(|p| p.image_id == 11)
            .expect("img 11 shifted")
            .abs_line;
        let row_12_after = t2
            .image_placements()
            .iter()
            .find(|p| p.image_id == 12)
            .expect("img 12 outside margin")
            .abs_line;
        assert_eq!(row_11_after.0, row_11_before.0 - 1);
        assert_eq!(row_12_after, row_12_before);

        // Reset margins, push img 13 into scrollback, add viewport img 14 + virtual 15.
        t2.advance(b"\x1b[r\x1b[2J\x1b[1;1H");
        t2.advance(&kitty_apc("a=T,i=13,f=32,s=1,v=1,q=2", &[4, 4, 4, 255]));
        t2.advance(b"\x1b[6;1H\n\n\n");
        t2.advance(b"\x1b[2;1H");
        t2.advance(&kitty_apc("a=T,i=14,f=32,s=1,v=1,q=2", &[5, 5, 5, 255]));
        t2.advance(&kitty_apc(
            "a=T,i=15,p=9,U=1,c=2,r=2,f=32,s=1,v=1,q=2",
            &[6, 6, 6, 255],
        ));

        // ED 2 clears viewport img 14 while keeping scrollback img 13 and virtual img 15.
        t2.advance(b"\x1b[2J");
        assert!(t2.image_data(13).is_some());
        assert!(t2.image_data(14).is_none());
        assert!(t2.active_snapshot().virtual_placement(15, 9).is_some());

        // ED 3 clears scrollback img 13 while keeping virtual img 15; RIS clears everything.
        t2.advance(b"\x1b[3J");
        assert!(t2.image_data(13).is_none());
        assert!(t2.active_snapshot().virtual_placement(15, 9).is_some());
        t2.advance(b"\x1bc");
        assert!(t2.active_snapshot().virtual_placement(15, 9).is_none());
        assert!(t2.image_placements().is_empty());
    }

    #[test]
    fn t1_item8_08_kitty_delete_targets_and_egui_texture_eviction() {
        let mut h = HeadlessTermHarness::scripted();
        h.set_viewport_size(640.0, 360.0);
        drop(h.run_frame());

        // Place images across specific rows/cols/z-indices and upload virtual placement.
        h.runtime.input_mut().push_str("\x1b[2;3H");
        h.runtime.input_mut().push_bytes(kitty_apc(
            "a=T,i=1,p=11,f=32,s=1,v=1,c=2,r=1,z=5,C=1,q=2",
            &[10, 0, 0, 255],
        ));
        h.runtime.input_mut().push_str("\x1b[4;6H");
        h.runtime.input_mut().push_bytes(kitty_apc(
            "a=T,i=2,p=22,f=32,s=1,v=1,c=2,r=2,z=-3,C=1,q=2",
            &[0, 20, 0, 255],
        ));
        h.runtime.input_mut().push_str("\x1b[7;2H");
        h.runtime.input_mut().push_bytes(kitty_apc(
            "a=T,i=3,p=33,f=32,s=1,v=1,c=2,r=1,z=1,C=1,q=2",
            &[0, 0, 30, 255],
        ));
        h.runtime.input_mut().push_bytes(kitty_apc(
            "a=T,i=4,p=44,U=1,c=1,r=1,f=32,s=1,v=1,q=2",
            &[40, 40, 40, 255],
        ));
        let f_up = h.run_frame();
        assert_eq!(
            h.runtime
                .terminal()
                .active_snapshot()
                .image_placements()
                .len(),
            3
        );
        let shapes_up = f_up.inspect_shapes();
        assert!(shapes_up.mesh_shape_count >= 3);
        let non_default_tex_count = shapes_up
            .mesh_texture_ids
            .iter()
            .filter(|id| **id != egui::TextureId::default())
            .count();
        assert_eq!(non_default_tex_count, 3);

        // d=q with mismatched z=0 keeps (6, 4) placement (z=-3); d=q with z=-3 deletes placement only.
        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=d,d=q,x=6,y=4,z=0;\x1b\\");
        drop(h.run_frame());
        assert_eq!(
            h.runtime
                .terminal()
                .active_snapshot()
                .image_placements()
                .len(),
            3
        );

        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=d,d=q,x=6,y=4,z=-3;\x1b\\");
        drop(h.run_frame());
        assert_eq!(
            h.runtime
                .terminal()
                .active_snapshot()
                .image_placements()
                .len(),
            2
        );
        // Lowercase d=q preserves stored image data 2, allowing a=p to re-place it.
        assert!(h.runtime.terminal().image_data(2).is_some());
        h.runtime.input_mut().push_str("\x1b[4;6H");
        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=p,i=2,p=23,c=2,r=1,C=1,q=2;\x1b\\");
        drop(h.run_frame());
        assert_eq!(
            h.runtime
                .terminal()
                .active_snapshot()
                .image_placements()
                .len(),
            3
        );

        // d=P (uppercase) at x=3, y=2 deletes placement 1 AND frees unreferenced image 1 + egui texture!
        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=d,d=P,x=3,y=2;\x1b\\");
        let f_del_p = h.run_frame();
        assert!(h.runtime.terminal().image_data(1).is_none());
        assert!(
            f_del_p.texture_deltas.freed_count >= 1,
            "d=P freeing image 1 must free its egui texture"
        );
        assert_eq!(
            h.runtime
                .terminal()
                .active_snapshot()
                .image_placements()
                .len(),
            2
        );

        // Test d=x (col 6), d=Y (row 7), d=a vs d=A, and d=r/R.
        h.runtime.input_mut().push_str("\x1b_Ga=d,d=x,x=6;\x1b\\");
        drop(h.run_frame());
        assert_eq!(
            h.runtime
                .terminal()
                .active_snapshot()
                .image_placements()
                .len(),
            1
        );
        assert!(h.runtime.terminal().image_data(2).is_some());

        // Place i=2 at row 1, col 1 so it stays referenced while d=Y at y=7 frees img 3.
        h.runtime.input_mut().push_str("\x1b[1;1H");
        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=p,i=2,p=24,c=1,r=1,C=1,q=2;\x1b\\");
        drop(h.run_frame());

        // d=Y at y=7 removes img 3 placement and frees img 3, leaving img 2 at row 1 intact.
        h.runtime.input_mut().push_str("\x1b_Ga=d,d=Y,y=7;\x1b\\");
        let f_del_y = h.run_frame();
        assert_eq!(
            h.runtime
                .terminal()
                .active_snapshot()
                .image_placements()
                .len(),
            1
        );
        assert!(h.runtime.terminal().image_data(3).is_none());
        assert!(f_del_y.texture_deltas.freed_count >= 1);

        // d=a keeps stored images & virtual placement; d=A frees unreferenced viewport placements;
        // d=R frees virtual placement (4, 44).
        h.runtime.input_mut().push_str("\x1b_Ga=d,d=a;\x1b\\");
        drop(h.run_frame());
        assert!(
            h.runtime
                .terminal()
                .active_snapshot()
                .image_placements()
                .is_empty()
        );
        assert!(h.runtime.terminal().image_data(2).is_some());
        assert!(
            h.runtime
                .terminal()
                .active_snapshot()
                .virtual_placement(4, 44)
                .is_some()
        );

        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=p,i=2,p=25,c=1,r=1,C=1,q=2;\x1b\\");
        drop(h.run_frame());
        h.runtime.input_mut().push_str("\x1b_Ga=d,d=A;\x1b\\");
        drop(h.run_frame());
        assert!(h.runtime.terminal().image_data(2).is_none());
        assert!(
            h.runtime
                .terminal()
                .active_snapshot()
                .virtual_placement(4, 44)
                .is_some()
        );

        // d=R range 4..4 deletes virtual placement 4 and frees image 4.
        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=d,d=R,x=4,y=4;\x1b\\");
        drop(h.run_frame());
        assert!(h.runtime.terminal().image_data(4).is_none());
    }
}

// ===========================================================================
// Item #9 — Sixel Graphics Completeness & Perf: Tier 1 (6 Equivalence Tests)
// ===========================================================================

#[cfg(feature = "sixel")]
mod item9_tier1 {
    use egui_xterm_core::{CoreConfig, Terminal};
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn t1_item9_01_sixel_dynamic_cell_pixel_size_and_egui_render() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.set_cell_pixel_size(10, 20);
        assert_eq!(term.cell_pixel_size(), (10, 20));

        // 25x45 sixel (1:1 aspect) at 10x20 cell size -> cols=ceil(25/10)=3, rows=ceil(45/20)=3.
        term.advance(b"\x1bPq\"1;1;25;45#1;2;100;0;0#1!25~-#1!25~\x1b\\");
        let p1 = term.image_placements()[0];
        assert_eq!((p1.cols, p1.rows), (3, 3));
        assert_eq!(term.cursor_pos(), (0, 3));

        // Change cell pixel size to 13x15 -> same 25x45 sixel becomes 2 cols x 3 rows.
        term.set_cell_pixel_size(13, 15);
        term.advance(b"\x1b[1;1H\x1bPq\"1;1;25;45#1~\x1b\\");
        let p2 = term.image_placements()[1];
        assert_eq!((p2.cols, p2.rows), (2, 3));

        // Render Sixel through HeadlessTermHarness and verify egui texture + mesh placement.
        let mut h = HeadlessTermHarness::scripted();
        h.set_viewport_size(640.0, 360.0);
        drop(h.run_frame());
        h.runtime
            .input_mut()
            .push_str("\x1bPq\"1;1;16;12#1;2;0;100;0#1!16~-#1!16~\x1b\\");
        let frame = h.run_frame();
        let entry = frame
            .texture_deltas
            .set_entries
            .iter()
            .find(|e| e.size == [16, 12])
            .expect("sixel 16x12 texture uploaded");
        let shapes = frame.inspect_shapes();
        assert!(shapes.mesh_texture_ids.contains(&entry.texture_id));
        assert!(shapes.mesh_vertex_positions().len() >= 4);
    }

    #[test]
    fn t1_item9_02_sixel_p1_and_raster_pan_pad_aspect_ratio_scaling() {
        // Test P1 macro aspect ratios and raster "Pan;Pad;Ph;Pv override at cell 8x16.
        let mut term = Terminal::new(80, 30, CoreConfig::default());
        term.set_cell_pixel_size(8, 16);

        // P1=2 -> Pan=5, Pad=1: 8x6 image -> rows = ceil((6*5)/(1*16)) = ceil(30/16) = 2.
        term.advance(b"\x1bP2;1;0q#1~\x1b\\");
        assert_eq!(term.image_placements()[0].rows, 2);
        assert_eq!(term.cursor_pos(), (0, 2));

        // P1=3 -> Pan=3, Pad=1: 8x6 image -> rows = ceil((6*3)/(1*16)) = ceil(18/16) = 2.
        term.advance(b"\x1b[1;1H\x1bP3;1;0q#1~\x1b\\");
        assert_eq!(term.image_placements()[1].rows, 2);

        // P1=7 -> Pan=1, Pad=1: 8x6 image -> rows = ceil(6/16) = 1.
        term.advance(b"\x1b[1;1H\x1bP7;1;0q#1~\x1b\\");
        assert_eq!(term.image_placements()[2].rows, 1);

        // Raster "2;1;8;16 overrides P1=7 -> 16px tall with 2:1 aspect => 2 rows.
        term.advance(b"\x1b[1;1H\x1bP7;1;0q\"2;1;8;16#1~\x1b\\");
        assert_eq!(term.image_placements()[3].rows, 2);

        // Raster "1;2;16;32 -> 32px tall with 1:2 aspect => 1 row (32*1/(2*16) = 1).
        term.advance(b"\x1b[1;1H\x1bPq\"1;2;16;32#1~\x1b\\");
        assert_eq!(term.image_placements()[4].rows, 1);
    }

    #[test]
    fn t1_item9_03_sixel_p2_transparency_modes_0_1_2() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());

        // 1. P2 omitted (defaults to 0): gap '?' & trailing raster Ph;Pv filled with redefined #0.
        term.advance(b"\x1bPq\"1;1;4;6#0;2;0;100;0#1;2;100;0;0#1?!~\x1b\\");
        let img0 = term
            .image_data(term.image_placements()[0].image_id)
            .expect("p2 default");
        assert_eq!((img0.width, img0.height), (4, 6));
        assert_eq!(&img0.active_rgba()[0..4], &[0, 255, 0, 255]); // gap filled with reg 0
        assert_eq!(&img0.active_rgba()[4..8], &[255, 0, 0, 255]); // reg 1 drawn
        assert_eq!(&img0.active_rgba()[12..16], &[0, 255, 0, 255]); // right pad filled with reg 0

        // 2. Explicit P2=2: unset pixels filled with reg 0 (blue).
        term.advance(b"\x1bP0;2;0q\"1;1;3;6#0;2;0;0;100#1;2;100;0;0#1@\x1b\\");
        let img2 = term
            .image_data(term.image_placements()[1].image_id)
            .expect("p2=2");
        assert_eq!(&img2.active_rgba()[0..4], &[255, 0, 0, 255]); // (0,0) red
        assert_eq!(&img2.active_rgba()[4..8], &[0, 0, 255, 255]); // (1,0) reg 0 blue
        let row1_off = 3 * 4;
        assert_eq!(
            &img2.active_rgba()[row1_off..row1_off + 4],
            &[0, 0, 255, 255]
        ); // (0,1) reg 0 blue

        // 3. Explicit P2=1: unset pixels remain transparent [0,0,0,0], explicit #0 draws opaque reg 0.
        term.advance(b"\x1bP0;1;0q\"1;1;4;6#0;2;0;100;0#1;2;100;0;0#1?!~#0@\x1b\\");
        let img1 = term
            .image_data(term.image_placements()[2].image_id)
            .expect("p2=1");
        assert_eq!(&img1.active_rgba()[0..4], &[0, 0, 0, 0]); // gap transparent
        assert_eq!(&img1.active_rgba()[4..8], &[255, 0, 0, 255]); // reg 1 red
        assert_eq!(&img1.active_rgba()[8..12], &[0, 255, 0, 255]); // explicit #0 green
        assert_eq!(&img1.active_rgba()[12..16], &[0, 0, 0, 0]); // trailing pad transparent
    }

    #[test]
    fn t1_item9_04_sixel_decsdm_80_scroll_and_placement_origin() {
        let mut term = Terminal::new(80, 10, CoreConfig::default());
        term.set_cell_pixel_size(8, 16);

        // DECRQM ?80 defaults to 2 (reset: sixel scrolling enabled).
        term.advance(b"\x1b[?80$p");
        assert_eq!(term.drain_outgoing(), b"\x1b[?80;2$y");
        assert!(!term.modes().sixel_display_mode);

        // Enable DECSDM (?80h): places at (0, 0) regardless of cursor and does NOT move cursor.
        term.advance(b"\x1b[?80h\x1b[?80$p\x1b[6;12H\x1bPq\"1;1;8;32#1~\x1b\\");
        assert_eq!(term.drain_outgoing(), b"\x1b[?80;1$y");
        assert!(term.modes().sixel_display_mode);
        assert_eq!(term.cursor_pos(), (11, 5));
        let p_decsdm = term.image_placements()[0];
        assert_eq!((p_decsdm.abs_line.0, p_decsdm.col), (0, 0));

        // Disable DECSDM (?80l): places at cursor (row 5, col 11) and advances cursor by rows=2.
        term.advance(b"\x1b[?80l\x1bPq\"1;1;8;32#1~\x1b\\");
        assert!(!term.modes().sixel_display_mode);
        let p_scroll = term.image_placements()[1];
        assert_eq!((p_scroll.abs_line.0, p_scroll.col), (5, 11));
        assert_eq!(term.cursor_pos(), (0, 7));
    }

    #[test]
    fn t1_item9_05_sixel_mode_1070_private_vs_shared_palette() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());

        // DECRQM ?1070 defaults to 1 (set: private color registers per image).
        term.advance(b"\x1b[?1070$p");
        assert_eq!(term.drain_outgoing(), b"\x1b[?1070;1$y");
        assert!(term.modes().sixel_private_color_registers);

        // Disable ?1070 (shared palette) and define register 5 = pure cyan (0, 100, 100) in image 1.
        term.advance(b"\x1b[?1070l\x1b[?1070$p");
        assert_eq!(term.drain_outgoing(), b"\x1b[?1070;2$y");
        term.advance(b"\x1bPq#5;2;0;100;100#5~\x1b\\");

        // Palette-only stream updates register 6 = pure yellow (100, 100, 0) without adding a placement.
        term.advance(b"\x1bPq#6;2;100;100;0\x1b\\");
        assert_eq!(term.image_placements().len(), 1);

        // Next sixel reuses #5 and #6 without redefining them.
        term.advance(b"\x1bPq#5~#6~\x1b\\");
        assert_eq!(term.image_placements().len(), 2);
        let shared_img = term
            .image_data(term.image_placements()[1].image_id)
            .expect("shared palette image");
        assert_eq!(&shared_img.active_rgba()[0..4], &[0, 255, 255, 255]);
        assert_eq!(&shared_img.active_rgba()[4..8], &[255, 255, 0, 255]);

        // Re-enable ?1070h: private VT340 palette restored (reg 5 is not [0, 255, 255, 255]).
        term.advance(b"\x1b[?1070h\x1bPq#5~\x1b\\");
        let priv_img = term
            .image_data(term.image_placements()[2].image_id)
            .expect("private palette image");
        assert_ne!(&priv_img.active_rgba()[0..4], &[0, 255, 255, 255]);
    }

    #[test]
    fn t1_item9_06_sixel_mode_8452_cursor_right_and_growing_canvas() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.set_cell_pixel_size(8, 16);

        // DECRQM ?8452 defaults to 2 (reset).
        term.advance(b"\x1b[?8452$p");
        assert_eq!(term.drain_outgoing(), b"\x1b[?8452;2$y");

        // Enable ?8452h and stream a growing canvas WITHOUT raster dimensions ("Ph;Pv):
        // band 0: 10px wide via !10~ (cols = ceil(10/8) = 2)
        // '$' returns x=0 and overlays bit 1 with reg 2
        // '-' advances y to 6 and draws 12px wide via !12~ (rows = ceil(12/16) = 1, cols = ceil(12/8) = 2)
        // '-' advances y to 12 and draws 4px wide at bit 5 ('_') -> height grows to 18 (rows = ceil(18*2/16) = 3 with default P1=0 2:1!).
        // Use P1=7 (1:1 aspect) so height=18 => rows=ceil(18/16)=2!
        term.advance(b"\x1b[?8452h\x1b[?8452$p");
        assert_eq!(term.drain_outgoing(), b"\x1b[?8452;1$y");

        term.advance(b"\x1b[4;5H\x1bP7;1;0q#1;2;100;0;0#2;2;0;100;0#1!10~$-#1!12~-#2!4_\x1b\\");
        let p = term.image_placements()[0];
        let img = term.image_data(p.image_id).expect("growing sixel stored");
        assert_eq!((img.width, img.height), (12, 18));
        assert_eq!((p.cols, p.rows), (2, 2));
        // Cursor starts at 0-based (row=3, col=4). With ?8452h and rows=2, cols=2:
        // advances rows-1=1 line down to row 4, col 4+2=6!
        assert_eq!(term.cursor_pos(), (6, 4));

        // Verify bottom-right pixel at (x=3, y=17) is green from bit 5 ('_' = 0x5F - 0x3F = 32 = 1<<5).
        let idx = (17 * 12 + 3) * 4;
        assert_eq!(&img.active_rgba()[idx..idx + 4], &[0, 255, 0, 255]);
    }
}

// ===========================================================================
// Item #8 — Kitty Graphics Completeness: Tier 2 (8 Boundary/Error Tests)
// ===========================================================================

#[cfg(feature = "kitty-graphics")]
mod item8_tier2 {
    use egui_xterm_core::{
        AbsLine, Cell, CellExtra, CellFlags, CoreConfig, KITTY_DIACRITICS, KITTY_PLACEHOLDER_CHAR,
        KittyFileMediumPolicy, KittyPlaceholderCell, KittyShmReader, PackedColor, Terminal,
        decode_placeholder_cell, diacritic_index,
    };
    use egui_xterm_e2e::HeadlessTermHarness;

    use super::kitty_helpers::*;

    #[test]
    fn t2_item8_01_kitty_sandbox_policy_rejects_traversal_proc_and_disabled() {
        struct RejectUnexpectedShm;
        impl KittyShmReader for RejectUnexpectedShm {
            fn read_and_unlink(
                &self,
                _name: &str,
                _offset: u64,
                _size: u64,
                _max_bytes: usize,
            ) -> Result<Vec<u8>, &'static str> {
                Err("EBADF:unexpected shm call")
            }
        }

        let temp_marked = unique_temp_path("tty-graphics-protocol-t2", "rgba");
        let temp_unmarked = unique_temp_path("plain-file-t2", "rgba");
        let _g1 = TempFileGuard(temp_marked.clone());
        let _g2 = TempFileGuard(temp_unmarked.clone());
        std::fs::write(&temp_marked, [1u8, 2, 3, 255]).expect("write marked temp");
        std::fs::write(&temp_unmarked, [4u8, 5, 6, 255]).expect("write unmarked temp");

        // 1. Disabled rejects t=f, t=t, t=s with EBADF and preserves files on disk.
        let mut term = Terminal::new(
            40,
            10,
            CoreConfig {
                kitty_file_medium_policy: KittyFileMediumPolicy::Disabled,
                ..CoreConfig::default()
            },
        );
        term.set_kitty_shm_reader(Some(Box::new(RejectUnexpectedShm)));
        let b64_marked = encode_base64(temp_marked.to_string_lossy().as_bytes());
        let b64_unmarked = encode_base64(temp_unmarked.to_string_lossy().as_bytes());
        let b64_shm = encode_base64(b"/ok-shm");
        term.advance(format!("\x1b_Ga=t,i=1,t=f,f=32,s=1,v=1;{b64_unmarked}\x1b\\").as_bytes());
        term.advance(format!("\x1b_Ga=t,i=2,t=t,f=32,s=1,v=1;{b64_marked}\x1b\\").as_bytes());
        term.advance(format!("\x1b_Ga=t,i=3,t=s,f=32,s=1,v=1;{b64_shm}\x1b\\").as_bytes());
        let out_dis = String::from_utf8_lossy(&term.drain_outgoing()).into_owned();
        assert!(out_dis.contains("i=1;EBADF"));
        assert!(out_dis.contains("i=2;EBADF"));
        assert!(out_dis.contains("i=3;EBADF"));
        assert!(temp_marked.exists() && temp_unmarked.exists());

        // 2. TempOnly rejects t=f, t=s, and t=t missing "tty-graphics-protocol".
        term.set_kitty_file_medium_policy(KittyFileMediumPolicy::TempOnly);
        term.advance(format!("\x1b_Ga=t,i=4,t=f,f=32,s=1,v=1;{b64_marked}\x1b\\").as_bytes());
        term.advance(format!("\x1b_Ga=t,i=5,t=t,f=32,s=1,v=1;{b64_unmarked}\x1b\\").as_bytes());
        term.advance(format!("\x1b_Ga=t,i=6,t=s,f=32,s=1,v=1;{b64_shm}\x1b\\").as_bytes());
        let out_tmp = String::from_utf8_lossy(&term.drain_outgoing()).into_owned();
        assert!(out_tmp.contains("i=4;EBADF"));
        assert!(out_tmp.contains("i=5;EBADF"));
        assert!(out_tmp.contains("i=6;EBADF"));
        assert!(temp_marked.exists() && temp_unmarked.exists());

        // 3. LocalFiles rejects /proc, /sys, /dev/null, directories, NUL bytes, OOB slice, and '..' shm.
        term.set_kitty_file_medium_policy(KittyFileMediumPolicy::LocalFiles);
        let dir_b64 = encode_base64(std::env::temp_dir().to_string_lossy().as_bytes());
        let bad_paths = [
            encode_base64(b"/proc/self/environ"),
            encode_base64(b"/sys/kernel/notes"),
            encode_base64(b"/dev/null"),
            encode_base64(b"bad\0path"),
            dir_b64,
        ];
        for (idx, p) in bad_paths.iter().enumerate() {
            let id = 10 + idx;
            term.advance(format!("\x1b_Ga=t,i={id},t=f,f=32,s=1,v=1;{p}\x1b\\").as_bytes());
        }
        // Out-of-bounds S/O slice on real file.
        term.advance(
            format!("\x1b_Ga=t,i=20,t=f,f=32,s=1,v=1,O=100,S=4;{b64_unmarked}\x1b\\").as_bytes(),
        );
        // Path traversal / oversized name on t=s.
        let trav_shm = encode_base64(b"../escape");
        let long_shm = encode_base64(&vec![b'a'; 256]);
        term.advance(format!("\x1b_Ga=t,i=21,t=s,f=32,s=1,v=1;{trav_shm}\x1b\\").as_bytes());
        term.advance(format!("\x1b_Ga=t,i=22,t=s,f=32,s=1,v=1;{long_shm}\x1b\\").as_bytes());

        let out_loc = String::from_utf8_lossy(&term.drain_outgoing()).into_owned();
        for id in [10, 11, 12, 13, 14, 20, 21, 22] {
            assert!(
                out_loc.contains(&format!("i={id};EBADF")),
                "expected EBADF for i={id}, got {out_loc:?}"
            );
            assert!(term.image_data(id).is_none());
        }
    }

    #[test]
    fn t2_item8_02_kitty_chunked_limits_stale_overwrite_and_mutual_exclusion() {
        let mut term = Terminal::new(40, 10, CoreConfig::default());

        // 1. i + I mutual exclusion -> EINVAL.
        term.advance(b"\x1b_Ga=q,i=1,I=1;\x1b\\");
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("EINVAL"));

        // 2. Max 16 concurrent chunked transfers -> 17th returns ENOMEM.
        for id in 1..=16u32 {
            term.advance(format!("\x1b_Ga=t,i={id},f=32,s=1,v=1,m=1;AA\x1b\\").as_bytes());
        }
        assert!(term.drain_outgoing().is_empty());
        term.advance(b"\x1b_Ga=t,i=17,f=32,s=1,v=1,m=1;AA\x1b\\");
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("ENOMEM"));

        // 3. Re-starting incomplete i=1 replaces the stale chunk buffer cleanly, and d=i aborts i=2.
        let ok_b64 = encode_base64(&[11, 22, 33, 255]);
        let (c1, c2) = ok_b64.split_at(4);
        term.advance(format!("\x1b_Ga=t,i=1,f=32,s=1,v=1,m=1;{c1}\x1b\\").as_bytes());
        term.advance(format!("\x1b_Gi=1,m=0;{c2}\x1b\\").as_bytes());
        assert_eq!(
            term.image_data(1).expect("restarted i=1").active_rgba(),
            &[11, 22, 33, 255]
        );
        term.advance(b"\x1b_Ga=d,d=i,i=2;\x1b\\");
        term.advance(b"\x1b_Ga=d,d=A;\x1b\\");
        drop(term.drain_outgoing());

        // Clear all pending transfers via RIS before testing 32 MiB chunk byte cap.
        term.advance(b"\x1bc");
        drop(term.drain_outgoing());

        // 4. Exceeding MAX_KITTY_PENDING_BYTES (32 MiB) across two <32 MiB APC chunks triggers ENOMEM.
        let chunk_17mib = vec![b'A'; 17 * 1024 * 1024];
        let mut apc1 = b"\x1b_Ga=t,i=55,f=32,s=1,v=1,m=1;".to_vec();
        apc1.extend_from_slice(&chunk_17mib);
        apc1.extend_from_slice(b"\x1b\\");
        term.advance(&apc1);
        assert!(term.drain_outgoing().is_empty());

        let mut apc2 = b"\x1b_Gi=55,m=1;".to_vec();
        apc2.extend_from_slice(&chunk_17mib);
        apc2.extend_from_slice(b"\x1b\\");
        term.advance(&apc2);
        let reply_cap = String::from_utf8_lossy(&term.drain_outgoing()).into_owned();
        assert!(reply_cap.contains("i=55;ENOMEM"), "reply={reply_cap:?}");
    }

    #[test]
    fn t2_item8_03_kitty_animation_loading_pause_resume_errors_and_compose_oob() {
        let mut term = Terminal::new(40, 10, CoreConfig::default());
        term.advance(&kitty_apc(
            "a=T,i=30,f=32,s=2,v=1",
            &[10, 20, 30, 255, 40, 50, 60, 255],
        ));
        drop(term.drain_outgoing());

        // Deleting frame on single-frame image has no extra frames to drop.
        term.advance(b"\x1b_Ga=d,d=f,i=30,r=1;\x1b\\");
        assert!(term.image_data(30).is_some());

        // Invalid animation state s=9 -> EINVAL; missing frame r=99 -> ENOENT.
        term.advance(b"\x1b_Ga=a,i=30,s=9;\x1b\\");
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("EINVAL"));
        term.advance(b"\x1b_Ga=a,i=30,r=99,z=50;\x1b\\");
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("ENOENT"));

        // Out-of-bounds frame rect (x=2 on width=2) -> EINVAL.
        term.advance(&kitty_apc("a=f,i=30,x=2,y=0,f=32,s=1,v=1", &[1, 2, 3, 255]));
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("EINVAL"));

        // Add frame 2 (chunked) + start Loading mode (s=2).
        let f2 = encode_base64(&[100, 110, 120, 255, 130, 140, 150, 255]);
        let (a, b) = f2.split_at(5);
        term.advance(format!("\x1b_Ga=f,i=30,f=32,s=2,v=1,z=50,m=1;{a}\x1b\\").as_bytes());
        term.advance(format!("\x1b_Ga=f,i=30,m=0;{b}\x1b\\").as_bytes());
        term.advance(b"\x1b_Ga=a,i=30,r=1,z=50,s=2;\x1b\\");
        drop(term.drain_outgoing());

        // Tick anchors at 0.0 -> transitions to frame 2 at 0.050 and pauses waiting for more frames!
        assert!(!term.tick_animations(0.0));
        assert!(term.tick_animations(0.050));
        assert!(term.next_animation_deadline().is_none());

        // Appending frame 3 wakes Loading mode and arms next deadline.
        term.advance(&kitty_apc(
            "a=f,i=30,f=32,s=2,v=1,z=50",
            &[200, 210, 220, 255, 230, 240, 250, 255],
        ));
        assert!(term.next_animation_deadline().is_some());

        // Out-of-bounds compose & overlapping same-frame compose both return EINVAL.
        drop(term.drain_outgoing());
        term.advance(b"\x1b_Ga=c,i=30,r=1,c=2,w=5,h=1,X=0,Y=0,x=0,y=0;\x1b\\");
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("EINVAL"));
        term.advance(b"\x1b_Ga=c,i=30,r=1,c=1,w=2,h=1,X=0,Y=0,x=0,y=0;\x1b\\");
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("EINVAL"));
    }

    #[test]
    fn t2_item8_04_kitty_unicode_placeholder_boundaries_and_diacritic_limits() {
        let mut term = Terminal::new(40, 10, CoreConfig::default());

        // Virtual placement combined with relative placement (U=1, P=1) -> EINVAL.
        term.advance(&kitty_apc(
            "a=T,i=1,p=1,U=1,P=1,f=32,s=1,v=1",
            &[1, 2, 3, 255],
        ));
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("EINVAL"));

        // Diacritic boundary checks: first, last, and out-of-table scalar.
        assert_eq!(diacritic_index(KITTY_DIACRITICS[0]), Some(0));
        assert_eq!(diacritic_index(KITTY_DIACRITICS[296]), Some(296));
        assert_eq!(diacritic_index('Z'), None);

        // decode_placeholder_cell returns None for non-placeholder char or PackedColor::Default fg.
        let regular_cell = Cell::new(
            'A',
            PackedColor::indexed(1),
            PackedColor::Default,
            CellFlags::empty(),
        );
        assert!(decode_placeholder_cell(&regular_cell, None).is_none());

        let default_fg_cell = Cell::new(
            KITTY_PLACEHOLDER_CHAR,
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::empty(),
        );
        assert!(decode_placeholder_cell(&default_fg_cell, None).is_none());

        // resolve_with_left does NOT inherit across mismatched image_id_low24 or placement_id.
        let prev = KittyPlaceholderCell {
            image_id_low24: 10,
            image_id: (1 << 24) | 10,
            placement_id: 2,
            row: Some(0),
            col: Some(0),
            id_msb: Some(1),
        };
        let mismatched_id = Cell::new(
            KITTY_PLACEHOLDER_CHAR,
            PackedColor::indexed(11),
            PackedColor::Default,
            CellFlags::empty(),
        );
        let resolved_diff_id = decode_placeholder_cell(&mismatched_id, None)
            .expect("valid cell")
            .resolve_with_left(Some(prev));
        assert_eq!(resolved_diff_id.row, None);
        assert_eq!(resolved_diff_id.col, None);
        assert_eq!(resolved_diff_id.id_msb, None);

        let mismatched_pid = Cell::new(
            KITTY_PLACEHOLDER_CHAR,
            PackedColor::indexed(10),
            PackedColor::Default,
            CellFlags::empty(),
        );
        let pid_extra = CellExtra {
            underline_color: Some(PackedColor::indexed(99)),
            ..CellExtra::default()
        };
        let resolved_diff_pid = decode_placeholder_cell(&mismatched_pid, Some(&pid_extra))
            .expect("valid cell")
            .resolve_with_left(Some(prev));
        assert_eq!(resolved_diff_pid.row, None);
        assert_eq!(resolved_diff_pid.col, None);
    }

    #[test]
    fn t2_item8_05_kitty_relative_enoparent_ecycle_etoodeep_and_negative_clamping() {
        let mut term = Terminal::new(40, 12, CoreConfig::default());
        term.advance(&kitty_apc("a=t,i=1,f=32,s=1,v=1,q=2", &[1, 2, 3, 255]));

        // 1. Missing parent -> ENOPARENT.
        term.advance(b"\x1b_Ga=p,i=1,p=2,P=999,Q=1;\x1b\\");
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("ENOPARENT"));

        // 2. Two-node cycle -> ECYCLE.
        term.advance(b"\x1b_Ga=p,i=1,p=10,c=1,r=1,q=2;\x1b\\");
        term.advance(b"\x1b_Ga=p,i=1,p=11,P=1,Q=10,H=1,V=1,c=1,r=1,q=2;\x1b\\");
        term.advance(b"\x1b_Ga=p,i=1,p=10,P=1,Q=11;\x1b\\");
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("ECYCLE"));

        // 3. Chain depth exceeding MAX_RELATIVE_PLACEMENT_DEPTH (32) -> ETOODEEP.
        // Build chain p=100 (root) -> p=101 -> ... -> p=132 (32 hops -> accepted), then p=133 (rejected).
        term.advance(b"\x1b_Ga=p,i=1,p=100,c=1,r=1,q=2;\x1b\\");
        for step in 1..=32u32 {
            let pid = 100 + step;
            let parent_pid = pid - 1;
            term.advance(
                format!("\x1b_Ga=p,i=1,p={pid},P=1,Q={parent_pid},c=1,r=1,q=2;\x1b\\").as_bytes(),
            );
        }
        assert!(term.drain_outgoing().is_empty());
        term.advance(b"\x1b_Ga=p,i=1,p=133,P=1,Q=132,c=1,r=1;\x1b\\");
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("ETOODEEP"));

        // 4. Negative V/H pushing relative placement above line 0 clamps safely in egui render.
        let mut h = HeadlessTermHarness::scripted();
        h.set_viewport_size(640.0, 360.0);
        drop(h.run_frame());
        h.runtime.input_mut().push_str("\x1b[2;3H");
        h.runtime.input_mut().push_bytes(kitty_apc(
            "a=T,i=5,p=1,f=32,s=1,v=1,c=2,r=2,C=1,q=2",
            &[9, 9, 9, 255],
        ));
        // Relative child with V=-5, H=-10 -> negative resolved origin; must clamp without panic!
        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=p,i=5,p=2,P=1,Q=1,H=-10,V=-5,c=2,r=2,C=1,q=2;\x1b\\");
        let frame = h.run_frame();
        assert!(
            frame.terminal_response.cols >= 10,
            "clamped negative relative placement must render cleanly"
        );
    }

    #[test]
    fn t2_item8_06_kitty_png_crc_corruption_chunk_order_and_bomb_caps() {
        let mut term = Terminal::new(40, 10, CoreConfig::default());

        let ihdr_rgba = make_ihdr(1, 1, 8, 6, 0);
        let idat_rgba = zlib_stored(&[0, 10, 20, 30, 255]);
        let valid = build_png(ihdr_rgba, None, None, &[0, 10, 20, 30, 255]);

        // 1. Corrupted IHDR CRC & corrupted IDAT data without CRC update.
        let mut bad_crc = valid.clone();
        bad_crc[29] ^= 0x01;
        term.advance(&kitty_apc("a=T,i=1,f=100", &bad_crc));
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("EINVAL"));

        let mut bad_iend_crc = valid.clone();
        let last = bad_iend_crc.len() - 1;
        bad_iend_crc[last] ^= 0xFF;
        term.advance(&kitty_apc("a=T,i=2,f=100", &bad_iend_crc));
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("EINVAL"));

        // 2. Non-consecutive IDAT split by tEXt chunk.
        let (p1, p2) = idat_rgba.split_at(idat_rgba.len() / 2);
        let split_idat = build_png_from_chunks(&[
            (b"IHDR", &ihdr_rgba),
            (b"IDAT", p1),
            (b"tEXt", b"k\0v"),
            (b"IDAT", p2),
            (b"IEND", &[]),
        ]);
        term.advance(&kitty_apc("a=T,i=3,f=100", &split_idat));
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("EINVAL"));

        // 3. Unknown critical chunk (uppercase 1st byte) & non-empty IEND.
        let unk_crit = build_png_from_chunks(&[
            (b"IHDR", &ihdr_rgba),
            (b"AbCd", b"x"),
            (b"IDAT", &idat_rgba),
            (b"IEND", &[]),
        ]);
        term.advance(&kitty_apc("a=T,i=4,f=100", &unk_crit));
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("EINVAL"));

        let non_empty_iend = build_png_from_chunks(&[
            (b"IHDR", &ihdr_rgba),
            (b"IDAT", &idat_rgba),
            (b"IEND", &[0]),
        ]);
        term.advance(&kitty_apc("a=T,i=5,f=100", &non_empty_iend));
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("EINVAL"));

        // 4. Truncated Adam7 pass stream & oversized IHDR exceeding 64 MiB budget.
        let truncated_adam7 = build_png(make_ihdr(3, 3, 8, 6, 1), None, None, &[0, 1, 2, 3, 4]);
        term.advance(&kitty_apc("a=T,i=6,f=100", &truncated_adam7));
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("EINVAL"));

        let huge_ihdr = make_ihdr(8192, 8192, 8, 6, 0); // 256 MiB > 64 MiB default budget
        let bomb_png = build_png(huge_ihdr, None, None, &[0]);
        term.advance(&kitty_apc("a=T,i=7,f=100", &bomb_png));
        assert!(String::from_utf8_lossy(&term.drain_outgoing()).contains("EINVAL"));

        for id in 1..=7u32 {
            assert!(term.image_data(id).is_none());
        }
        assert!(term.image_placements().is_empty());
    }

    #[test]
    fn t2_item8_07_kitty_reflow_eviction_alt_crop_and_subcell_crop_boundaries() {
        // 1. Primary scrollback eviction during resize cascade-deletes children & frees images.
        let cfg = CoreConfig {
            scrollback_limit: 1,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(12, 4, cfg);
        term.advance(&kitty_apc("a=T,i=1,p=1,f=32,s=1,v=1,q=2", &[1, 1, 1, 255]));
        term.advance(b"\x1b_Ga=p,i=1,p=2,P=1,Q=1,H=1,V=0,q=2;\x1b\\");
        term.advance(b"\r\nA\r\nB\r\nC\r\n");
        term.resize(12, 2);
        assert!(term.image_placements().is_empty());
        assert!(term.image_data(1).is_none());

        // 2. Alternate screen resize crops top vs bottom depending on cursor row.
        let mut alt = Terminal::new(20, 6, CoreConfig::default());
        alt.advance(b"\x1b[?1049h\x1b[1;1H");
        alt.advance(&kitty_apc("a=T,i=20,f=32,s=1,v=1,q=2", &[2, 2, 2, 255]));
        alt.advance(b"\x1b[5;1H");
        alt.advance(&kitty_apc("a=T,i=21,f=32,s=1,v=1,q=2", &[3, 3, 3, 255]));
        alt.resize(20, 3);
        assert_eq!(alt.image_placements().len(), 1);
        assert_eq!(alt.image_placements()[0].image_id, 21);
        assert_eq!(alt.image_placements()[0].abs_line, AbsLine(2));
        assert!(alt.image_data(20).is_none());

        // 3. Sub-cell X/Y offsets stored and out-of-bounds src_x/src_y crop rendered safely in egui.
        let mut h = HeadlessTermHarness::scripted();
        h.set_viewport_size(400.0, 240.0);
        drop(h.run_frame());
        h.runtime.input_mut().push_bytes(kitty_apc(
            "a=T,i=30,p=1,X=3,Y=5,x=99,y=99,w=10,h=10,c=2,r=2,f=32,s=2,v=2,q=2",
            &[9u8; 16],
        ));
        let f = h.run_frame();
        let p30 = h.runtime.terminal().image_placements()[0];
        assert_eq!((p30.sub_x, p30.sub_y, p30.src_x, p30.src_y), (3, 5, 99, 99));
        assert!(
            f.inspect_shapes()
                .mesh_texture_ids
                .iter()
                .all(|id| *id == egui::TextureId::default())
        );
    }

    #[test]
    fn t2_item8_08_kitty_quiet_flags_and_sub_u16_grid_bounds() {
        let mut term = Terminal::new(40, 10, CoreConfig::default());

        // q=0 emits both OK and ENOENT.
        term.advance(&kitty_apc("a=t,i=1,q=0,f=32,s=1,v=1", &[1, 2, 3, 255]));
        term.advance(b"\x1b_Ga=p,i=999,q=0;\x1b\\");
        let r0 = String::from_utf8_lossy(&term.drain_outgoing()).into_owned();
        assert!(r0.contains("i=1;OK"));
        assert!(r0.contains("i=999;ENOENT"));

        // q=1 suppresses OK, still emits ENOENT.
        term.advance(&kitty_apc("a=t,i=2,q=1,f=32,s=1,v=1", &[1, 2, 3, 255]));
        term.advance(b"\x1b_Ga=p,i=999,q=1;\x1b\\");
        let r1 = String::from_utf8_lossy(&term.drain_outgoing()).into_owned();
        assert!(!r1.contains("i=2;OK"));
        assert!(r1.contains("i=999;ENOENT"));

        // q=2 suppresses both OK and ENOENT.
        term.advance(&kitty_apc("a=t,i=3,q=2,f=32,s=1,v=1", &[1, 2, 3, 255]));
        term.advance(b"\x1b_Ga=p,i=999,q=2;\x1b\\");
        assert!(term.drain_outgoing().is_empty());

        // Large c/r/X/Y values clamp safely to u16::MAX without overflow; C=1 preserves cursor.
        term.advance(b"\x1b[2;4H");
        let cursor_before = term.cursor_pos();
        term.advance(b"\x1b_Ga=p,i=1,c=99999,r=88888,X=77777,Y=66666,C=1,q=2;\x1b\\");
        assert_eq!(term.cursor_pos(), cursor_before);
        let p = term.image_placements()[0];
        assert_eq!(
            (p.cols, p.rows, p.sub_x, p.sub_y),
            (u16::MAX, u16::MAX, u16::MAX, u16::MAX)
        );
    }
}

// ===========================================================================
// Item #9 — Sixel Graphics Completeness & Perf: Tier 2 (6 Boundary Tests)
// ===========================================================================

#[cfg(feature = "sixel")]
mod item9_tier2 {
    use egui_xterm_core::{CoreConfig, Terminal};
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn t2_item9_01_sixel_empty_and_palette_only_streams() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());

        // Empty Sixel DCS stream emits no placement or image.
        term.advance(b"\x1bPq\x1b\\");
        assert!(term.image_placements().is_empty());

        // Palette-only stream with ?1070l emits no image yet persists palette changes.
        term.advance(b"\x1b[?1070l\x1bPq#10;2;100;0;100\x1b\\");
        assert!(term.image_placements().is_empty());
        term.advance(b"\x1bPq#10~\x1b\\");
        assert_eq!(term.image_placements().len(), 1);
        let img = term
            .image_data(term.image_placements()[0].image_id)
            .expect("uses reg 10 from palette-only stream");
        assert_eq!(&img.active_rgba()[0..4], &[255, 0, 255, 255]);

        // Raster-only P2=2 with no sixel data fills Ph;Pv canvas with register 0; P2=1 produces transparent canvas.
        term.advance(b"\x1b[?1070h\x1bP0;2;0q\"1;1;2;2#0;2;100;100;0\x1b\\");
        assert_eq!(term.image_placements().len(), 2);
        let img_r2 = term
            .image_data(term.image_placements()[1].image_id)
            .expect("raster-only p2=2");
        assert_eq!(img_r2.active_rgba(), &[255, 255, 0, 255].repeat(4));
    }

    #[test]
    fn t2_item9_02_sixel_raster_zero_aspect_and_pixel_bounds_cap() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.set_cell_pixel_size(8, 16);

        // Zero Pan;Pad ("0;0;8;16) clamps to 1:1 instead of dividing by zero -> 1 row.
        term.advance(b"\x1bPq\"0;0;8;16#1~\x1b\\");
        assert_eq!(term.image_placements().len(), 1);
        assert_eq!(term.image_placements()[0].rows, 1);

        // Oversized raster Ph;Pv (10000;10000 > MAX_IMAGE_DIMENSION) rejected cleanly without OOM/panic.
        term.advance(b"\x1bPq\"1;1;10000;10000#1~\x1b\\");
        assert_eq!(term.image_placements().len(), 1);

        // Extreme repeat !999999~ clamps safely to MAX_IMAGE_DIMENSION (8192) and stays bounded.
        term.advance(b"\x1bPq#1!999999~\x1b\\");
        assert_eq!(term.image_placements().len(), 2);
        let clamped_img = term
            .image_data(term.image_placements()[1].image_id)
            .expect("clamped repeat sixel");
        assert_eq!(clamped_img.width, 8192);
    }

    #[test]
    fn t2_item9_03_sixel_hls_rgb_extended_registers_and_decstr_reset() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());

        // HLS S=0 (achromatic gray), H=120 (green), H=480 (wrap 120), RGB >100% clamp, and reg #1500 -> #1023.
        term.advance(
            b"\x1bPq#1;1;0;50;0#2;1;480;50;100#3;2;150;50;0#1500;2;0;0;100#1@#2@#3@#1500@\x1b\\",
        );
        let img = term
            .image_data(term.image_placements()[0].image_id)
            .expect("colors");
        assert_eq!((img.width, img.height), (4, 1));
        assert_eq!(&img.active_rgba()[0..4], &[128, 128, 128, 255]);
        assert_eq!(&img.active_rgba()[4..8], &[0, 255, 0, 255]);
        assert_eq!(&img.active_rgba()[8..12], &[255, 128, 0, 255]);
        assert_eq!(&img.active_rgba()[12..16], &[0, 0, 255, 255]);

        // Set ?80h, ?1070l, ?8452h then execute DECSTR (CSI ! p) -> resets all three to defaults!
        term.advance(b"\x1b[?80h\x1b[?1070l\x1b[?8452h\x1b[!p");
        assert!(!term.modes().sixel_display_mode);
        assert!(term.modes().sixel_private_color_registers);
        assert!(!term.modes().sixel_cursor_right);
    }

    #[test]
    fn t2_item9_04_sixel_8452_right_edge_clamping_and_bottom_scroll() {
        let mut term = Terminal::new(10, 4, CoreConfig::default());
        term.set_cell_pixel_size(8, 16);

        // Enable ?8452h, place 32x32 sixel (4 cols, 2 rows) at col 8, row 2 -> clamps col to 9 (grid_cols-1)
        // and advances 1 row down to row 3.
        term.advance(b"\x1b[?8452h\x1b[3;9H\x1bPq\"1;1;32;32#1~\x1b\\");
        assert_eq!(term.cursor_pos(), (9, 3));

        // Near bottom row (row 3) with 32px height (2 rows) triggers scroll without panic.
        term.advance(b"\x1b[4;2H\x1bPq\"1;1;16;32#1~\x1b\\");
        assert_eq!(term.cursor_pos(), (3, 3));
        assert!(term.scrollback_len() >= 1);
    }

    #[test]
    fn t2_item9_05_sixel_reflow_scroll_ed2_alt_screen_and_ris() {
        let mut term = Terminal::new(16, 6, CoreConfig::default());
        term.set_cell_pixel_size(8, 16);

        // Long wrapped line followed by Sixel placement, then narrower resize (16 -> 8).
        term.advance(b"0123456789abcdef\r\n");
        term.advance(b"\x1bPq\"1;1;8;16#1~\x1b\\");
        let abs_before = term.image_placements()[0].abs_line;
        term.resize(8, 6);
        assert_eq!(term.image_placements()[0].abs_line.0, abs_before.0 + 1);

        // Scroll first sixel into scrollback, add viewport sixel, verify ED 2 preserves scrollback sixel.
        term.advance(b"\x1b[6;1H\n\n\n\n\n");
        term.advance(b"\x1b[2;1H\x1bPq\"1;1;8;16#2~\x1b\\");
        assert_eq!(term.image_placements().len(), 2);
        let sb_id = term.image_placements()[0].image_id;
        let vp_id = term.image_placements()[1].image_id;

        term.advance(b"\x1b[2J");
        assert!(term.image_data(sb_id).is_some());
        assert!(term.image_data(vp_id).is_none());

        // Alt-screen sixel isolated from primary; RIS clears all.
        term.advance(b"\x1b[?1049h\x1bPq\"1;1;8;16#3~\x1b\\");
        let alt_id = term.image_placements()[0].image_id;
        term.advance(b"\x1b[?1049l");
        assert_eq!(term.image_placements().len(), 1);
        assert_eq!(term.image_placements()[0].image_id, sb_id);
        term.advance(b"\x1bc");
        assert!(term.image_placements().is_empty());
        assert!(term.image_data(sb_id).is_none());
        assert!(term.image_data(alt_id).is_none());
    }

    #[test]
    fn t2_item9_06_sixel_memory_budget_eviction_and_egui_cleanup() {
        let mut h = HeadlessTermHarness::scripted();
        h.set_viewport_size(640.0, 360.0);
        drop(h.run_frame());

        h.runtime
            .input_mut()
            .push_str("\x1bPq\"1;1;16;16#1;2;100;0;0#1!16~\x1b\\");
        let f1 = h.run_frame();
        assert_eq!(h.runtime.terminal().image_placements().len(), 1);
        let id1 = h.runtime.terminal().image_placements()[0].image_id;
        let tex_id = f1
            .texture_deltas
            .set_entries
            .iter()
            .find(|e| e.size == [16, 16])
            .expect("sixel texture uploaded")
            .texture_id;
        assert!(f1.inspect_shapes().mesh_texture_ids.contains(&tex_id));

        // Clear viewport with ED 2 -> removes placement, frees StoredImage, and drops egui texture.
        h.runtime.input_mut().push_str("\x1b[2J");
        let f2 = h.run_frame();
        assert!(h.runtime.terminal().image_placements().is_empty());
        assert!(h.runtime.terminal().image_data(id1).is_none());
        assert!(f2.texture_deltas.freed_count >= 1);
        assert!(!f2.inspect_shapes().mesh_texture_ids.contains(&tex_id));
    }
}

// ===========================================================================
// Items #8 & #9 — Tier 3 Pairwise Combination Tests (2 + 2 = 4 Tests)
// ===========================================================================

#[cfg(feature = "kitty-graphics")]
#[test]
fn t3_item8_01_kitty_chunked_animation_unicode_placeholder_and_alt_screen() {
    use egui_xterm_core::{KITTY_DIACRITICS, KITTY_PLACEHOLDER_CHAR, decode_placeholder_cell};
    use egui_xterm_e2e::HeadlessTermHarness;
    use kitty_helpers::*;

    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(640.0, 360.0);
    drop(h.run_frame());

    // Chunked virtual placement + 2-frame animation (Playing s=3, v=1 infinite).
    let f1 = encode_base64(&[255, 0, 0, 255]);
    let (c1, c2) = f1.split_at(4);
    h.runtime.input_mut().push_str(format!(
        "\x1b_Ga=T,i=50,p=3,U=1,c=2,r=1,f=32,s=1,v=1,m=1;{c1}\x1b\\\
         \x1b_Gi=50,m=0;{c2}\x1b\\"
    ));
    h.runtime
        .input_mut()
        .push_bytes(kitty_apc("a=f,i=50,f=32,s=1,v=1,z=60", &[0, 255, 0, 255]));
    h.runtime
        .input_mut()
        .push_str("\x1b_Ga=a,i=50,r=1,z=40,s=3,v=1;\x1b\\");

    // Print primary U+10EEEE placeholders.
    let d0 = KITTY_DIACRITICS[0];
    h.runtime.input_mut().push_str(format!(
        "\x1b[1;1H\x1b[38;5;50m\x1b[58;5;3m{KITTY_PLACEHOLDER_CHAR}{d0}{d0}{KITTY_PLACEHOLDER_CHAR}\x1b[0m"
    ));
    drop(h.run_frame());

    let term = h.runtime.terminal_mut();
    assert!(!term.tick_animations(1.000));
    let rev0 = term.image_data(50).expect("anim").revision;
    assert!(term.tick_animations(1.040));
    assert!(term.image_data(50).expect("anim f2").revision > rev0);
    assert_eq!(
        term.image_data(50).expect("anim f2").active_rgba(),
        &[0, 255, 0, 255]
    );
    assert!(term.active_snapshot().virtual_placement(50, 3).is_some());

    // Enter alt screen: primary virtual placement hidden on alt screen, regular alt placement renders.
    h.runtime.input_mut().push_str("\x1b[?1049h\x1b[2;2H");
    h.runtime.input_mut().push_bytes(kitty_apc(
        "a=T,i=51,f=32,s=1,v=1,c=2,r=1,q=2",
        &[0, 0, 255, 255],
    ));
    let alt_frame = h.run_frame();
    assert!(
        h.runtime
            .terminal()
            .active_snapshot()
            .virtual_placement(50, 3)
            .is_none()
    );
    let alt_tex_id = alt_frame
        .texture_deltas
        .set_entries
        .iter()
        .find(|e| e.size == [1, 1])
        .expect("alt image texture uploaded")
        .texture_id;
    assert!(
        alt_frame
            .inspect_shapes()
            .mesh_texture_ids
            .contains(&alt_tex_id)
    );

    // Leave alt screen (?1049l): primary virtual placement and U+10EEEE cells return intact,
    // and alt placement is no longer drawn.
    h.runtime.input_mut().push_str("\x1b[?1049l");
    let pri_frame = h.run_frame();
    assert!(
        !pri_frame
            .inspect_shapes()
            .mesh_texture_ids
            .contains(&alt_tex_id)
    );
    let snap = h.runtime.terminal().active_snapshot();
    assert!(snap.virtual_placement(50, 3).is_some());
    let cell0 = &snap.visible_rows[0].cells()[0];
    let dec0 = decode_placeholder_cell(cell0, snap.cell_extra(cell0.extra_id)).expect("restored");
    assert_eq!((dec0.image_id, dec0.placement_id), (50, 3));

    // Re-entering ?1049h clears the previous alt screen image and frees its egui texture.
    h.runtime.input_mut().push_str("\x1b[?1049h");
    let alt_clear_frame = h.run_frame();
    assert!(h.runtime.terminal().image_data(51).is_none());
    assert!(alt_clear_frame.texture_deltas.freed_count >= 1);
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn t3_item8_02_kitty_png_adam7_relative_chain_decstbm_scroll_and_zoom() {
    use egui_xterm_e2e::HeadlessTermHarness;
    use kitty_helpers::*;

    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(640.0, 360.0);
    h.set_pixels_per_point(2.0);
    drop(h.run_frame());

    // 16-bit RGBA Adam7 2x2 PNG as parent + relative child.
    let raw_2x2 = encode_adam7_filtered(
        2,
        2,
        6,
        16,
        |x, y| {
            vec![
                u8::try_from(x * 100 + 20).unwrap_or(0),
                0x11,
                u8::try_from(y * 100 + 30).unwrap_or(0),
                0x22,
                0x80,
                0x33,
                0xFF,
                0x44,
            ]
        },
        |_p, _py| 0,
    );
    let png_2x2 = build_png(make_ihdr(2, 2, 16, 6, 1), None, None, &raw_2x2);
    h.runtime.input_mut().push_str("\x1b[4;3H");
    h.runtime
        .input_mut()
        .push_bytes(kitty_apc("a=T,i=80,p=1,f=100,c=2,r=2,C=1,q=2", &png_2x2));
    h.runtime.input_mut().push_bytes(kitty_apc(
        "a=T,i=81,p=2,P=80,Q=1,H=4,V=1,f=32,s=1,v=1,c=1,r=1,C=1,q=2",
        &[255, 255, 0, 255],
    ));
    let f1 = h.run_frame();
    assert!(f1.inspect_shapes().mesh_shape_count >= 2);
    assert_eq!(h.runtime.terminal().image_placements().len(), 2);

    // DECSTBM [2..=10] scroll up 1 shifts both parent & relative child up by 1.
    let parent_before = h.runtime.terminal().image_placements()[0].abs_line;
    let child_before = h.runtime.terminal().image_placements()[1].abs_line;
    h.runtime.input_mut().push_str("\x1b[2;10r\x1b[1S\x1b[r");
    drop(h.run_frame());
    assert_eq!(
        h.runtime.terminal().image_placements()[0].abs_line.0,
        parent_before.0 - 1
    );
    assert_eq!(
        h.runtime.terminal().image_placements()[1].abs_line.0,
        child_before.0 - 1
    );

    // Zoom in and verify rendered mesh vertices remain clipped cleanly inside terminal rect.
    h.set_zoom_factor(1.5);
    let f_zoom = h.run_frame();
    let term_rect = f_zoom.terminal_response.response.rect;
    let shapes = f_zoom.inspect_shapes();
    assert!(shapes.mesh_shape_count >= 2);
    assert!(
        shapes
            .mesh_vertex_positions()
            .iter()
            .all(|p| p.x >= term_rect.min.x - 1.0 && p.x <= term_rect.max.x + 1.0)
    );
}

#[cfg(feature = "sixel")]
#[test]
fn t3_item9_01_sixel_decsdm_1070_8452_pan_pad_and_dynamic_dpi() {
    use egui_xterm_e2e::HeadlessTermHarness;

    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(640.0, 360.0);
    h.set_pixels_per_point(1.5);
    drop(h.run_frame());

    // Combine ?1070l shared palette + ?8452h cursor right + Pan/Pad=2;1 + P2=1 transparency.
    h.runtime.input_mut().push_str(
        "\x1b[?1070l\x1b[?8452h\x1b[3;4H\
         \x1bP0;1;0q\"2;1;16;12#4;2;100;20;80#4!16~-#4!16~\x1b\\",
    );
    let f1 = h.run_frame();
    let p1 = h.runtime.terminal().image_placements()[0];
    assert_eq!(p1.col, 3);
    assert!(p1.cols >= 1 && p1.rows >= 1);
    assert!(h.runtime.terminal().cursor_pos().0 > 3);
    assert!(!f1.texture_deltas.set_entries.is_empty());

    // Switch to DECSDM ?80h and reuse shared reg #4 -> places at (0, 0) without moving cursor.
    let cursor_saved = h.runtime.terminal().cursor_pos();
    h.runtime
        .input_mut()
        .push_str("\x1b[?80h\x1bP0;1;0q\"1;1;8;6#4!8~\x1b\\");
    h.set_pixels_per_point(2.0);
    let f2 = h.run_frame();
    assert_eq!(h.runtime.terminal().cursor_pos(), cursor_saved);
    let p2 = h.runtime.terminal().image_placements()[1];
    assert_eq!((p2.abs_line.0, p2.col), (0, 0));
    let img2 = h
        .runtime
        .terminal()
        .image_data(p2.image_id)
        .expect("second sixel with shared reg 4");
    assert_eq!(&img2.active_rgba()[0..4], &[255, 51, 204, 255]);
    assert!(f2.inspect_shapes().mesh_shape_count >= 2);
}

#[cfg(all(feature = "kitty-graphics", feature = "sixel"))]
#[test]
fn t3_item9_02_sixel_and_kitty_z_layering_clipping_and_multi_pane() {
    use egui_xterm_e2e::{MultiPaneHarness, ScriptedInput, ShapeInspection};
    use kitty_helpers::*;

    let mut mp = MultiPaneHarness::new(ScriptedInput::new(), ScriptedInput::new());
    mp.set_viewport_size(800.0, 400.0);
    drop(mp.run_split_frame());

    // Left pane: Kitty behind-text (z=-2) + front (z=3) over text.
    mp.left.input_mut().push_str("LEFT_TEXT\r\n");
    mp.left.input_mut().push_bytes(kitty_apc(
        "a=T,i=1,f=32,s=2,v=2,c=3,r=2,z=-2,C=1,q=2",
        &[200, 10, 10, 255].repeat(4),
    ));
    mp.left.input_mut().push_bytes(kitty_apc(
        "a=T,i=2,f=32,s=2,v=2,c=2,r=1,z=3,C=1,q=2",
        &[10, 200, 10, 255].repeat(4),
    ));

    // Right pane: Sixel P2=1 transparent overlay over text.
    mp.right.input_mut().push_str("RIGHT_TEXT\r\n");
    mp.right
        .input_mut()
        .push_str("\x1bP0;1;0q\"1;1;12;12#3;2;0;100;100#3!12~-#3!12~\x1b\\");

    let (left_resp, right_resp, meta) = mp.run_split_frame();
    let left_rect = left_resp.response.rect;
    let right_rect = right_resp.response.rect;

    let left_insp =
        ShapeInspection::from_terminal_shapes(&meta.shapes, left_rect, meta.pixels_per_point);
    let right_insp =
        ShapeInspection::from_terminal_shapes(&meta.shapes, right_rect, meta.pixels_per_point);

    assert!(left_insp.mesh_shape_count >= 2);
    assert!(right_insp.mesh_shape_count >= 1);
    assert!(left_insp.contains_text("LEFT_TEXT"));
    assert!(!left_insp.contains_text("RIGHT_TEXT"));
    assert!(right_insp.contains_text("RIGHT_TEXT"));
    assert!(!right_insp.contains_text("LEFT_TEXT"));

    assert!(
        left_insp
            .mesh_vertex_positions()
            .iter()
            .all(|p| p.x <= left_rect.max.x + 1.0)
    );
    assert!(
        right_insp
            .mesh_vertex_positions()
            .iter()
            .all(|p| p.x >= right_rect.min.x - 1.0)
    );

    // Clear left pane images via d=A -> left texture freed while right Sixel remains intact.
    mp.left.input_mut().push_str("\x1b_Ga=d,d=A;\x1b\\");
    let (_, _, meta2) = mp.run_split_frame();
    assert!(meta2.texture_deltas.freed_count >= 2);
    let right_insp2 =
        ShapeInspection::from_terminal_shapes(&meta2.shapes, right_rect, meta2.pixels_per_point);
    assert!(right_insp2.mesh_shape_count >= 1);
}

// ===========================================================================
// Tier 4 — Shared Real-World Workload (Item #8 & #9)
// ===========================================================================

#[cfg(all(feature = "kitty-graphics", feature = "sixel"))]
#[test]
fn w_gfx_01_terminal_image_viewer_animation_sixel_and_reflow() {
    use egui_xterm_core::{KITTY_DIACRITICS, KITTY_PLACEHOLDER_CHAR, decode_placeholder_cell};
    use egui_xterm_e2e::HeadlessTermHarness;
    use kitty_helpers::*;

    let temp_preview = unique_temp_path("tty-graphics-protocol-viewer", "rgba");
    let _temp_guard = TempFileGuard(temp_preview.clone());
    std::fs::write(&temp_preview, [12, 34, 56, 255, 78, 90, 12, 255])
        .expect("write viewer temp preview");

    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(760.0, 460.0);
    drop(h.run_frame());

    // Phase 1: Print long wrapped caption (exercises reflow later) + concurrent Kitty uploads:
    // - i=1: Adam7 2x2 PNG (chunked m=1 -> m=0)
    // - i=2: t=t temp file preview ("tty-graphics-protocol")
    // - i=3: 3-frame animated Kitty sprite
    let caption = "VIEWER_CAPTION_LONG_LINE_FOR_REFLOW_TRACKING_0123456789ABCDEF";
    h.runtime.input_mut().push_str(format!("{caption}\r\n"));

    let raw_png = encode_adam7_filtered(
        2,
        2,
        6,
        8,
        |x, y| {
            vec![
                u8::try_from(x * 120 + 15).unwrap_or(0),
                u8::try_from(y * 120 + 25).unwrap_or(0),
                200,
                255,
            ]
        },
        |_p, _py| 0,
    );
    let png_bytes = build_png(make_ihdr(2, 2, 8, 6, 1), None, None, &raw_png);
    let png_b64 = encode_base64(&png_bytes);
    let (p_c1, p_c2) = png_b64.split_at(png_b64.len() / 2);

    let b64_temp = encode_base64(temp_preview.to_string_lossy().as_bytes());
    let f1_b64 = encode_base64(&[255, 0, 0, 255]);
    let (f1a, f1b) = f1_b64.split_at(4);

    h.runtime.input_mut().push_str(format!(
        "\x1b_Ga=T,i=1,p=1,f=100,c=4,r=2,C=1,m=1;{p_c1}\x1b\\\
         \x1b_Ga=T,i=3,p=1,f=32,s=1,v=1,c=2,r=2,C=1,m=1;{f1a}\x1b\\"
    ));
    drop(h.run_frame());

    h.runtime.input_mut().push_str(format!(
        "\x1b_Gi=1,m=0;{p_c2}\x1b\\\
         \x1b_Gi=3,m=0;{f1b}\x1b\\\
         \x1b[4;1H\x1b_Ga=T,i=2,p=1,t=t,f=32,s=2,v=1,c=3,r=1,C=1;{b64_temp}\x1b\\"
    ));
    h.runtime
        .input_mut()
        .push_bytes(kitty_apc("a=f,i=3,f=32,s=1,v=1,z=50", &[0, 255, 0, 255]));
    h.runtime
        .input_mut()
        .push_bytes(kitty_apc("a=f,i=3,f=32,s=1,v=1,z=50", &[0, 0, 255, 255]));
    h.runtime
        .input_mut()
        .push_str("\x1b_Ga=a,i=3,r=1,z=40,s=3,v=1;\x1b\\");

    let phase1 = h.run_frame();
    assert!(
        !temp_preview.exists(),
        "t=t viewer preview must be unlinked"
    );
    assert_eq!(h.runtime.terminal().image_placements().len(), 3);
    assert!(phase1.texture_deltas.set_entries.len() >= 3);
    assert!(phase1.inspect_shapes().mesh_shape_count >= 3);

    // Phase 2: Virtual placeholder U=1 + U+10EEEE cell row + relative badge (P=1,Q=1,H=5,V=0).
    let d0 = KITTY_DIACRITICS[0];
    let d1 = KITTY_DIACRITICS[1];
    h.runtime.input_mut().push_bytes(kitty_apc(
        "a=T,i=10,p=5,U=1,c=2,r=1,f=32,s=1,v=1,q=2",
        &[250, 128, 10, 255],
    ));
    h.runtime.input_mut().push_str(format!(
        "\x1b[6;1H\x1b[38;5;10m\x1b[58;5;5m{KITTY_PLACEHOLDER_CHAR}{d0}{d0}{KITTY_PLACEHOLDER_CHAR}{d0}{d1}\x1b[0m"
    ));
    h.runtime.input_mut().push_bytes(kitty_apc(
        "a=T,i=11,p=1,P=1,Q=1,H=5,V=0,f=32,s=1,v=1,c=1,r=1,C=1,q=2",
        &[255, 255, 255, 255],
    ));

    // Tick animation across frames and confirm revision monotonically increments.
    let rev_init = h.runtime.terminal().image_data(3).expect("sprite").revision;
    assert!(!h.runtime.terminal_mut().tick_animations(2.000));
    let next_dl = h
        .runtime
        .terminal()
        .next_animation_deadline()
        .expect("animation armed");
    assert!((next_dl - 2.040).abs() < 1e-6);
    assert!(h.runtime.terminal_mut().tick_animations(2.050));
    let rev_f2 = h.runtime.terminal().image_data(3).expect("f2").revision;
    assert!(rev_f2 > rev_init);
    assert_eq!(
        h.runtime
            .terminal()
            .image_data(3)
            .expect("f2")
            .active_rgba(),
        &[0, 255, 0, 255]
    );

    let phase2 = h.run_frame();
    let snap2 = h.runtime.terminal().active_snapshot();
    assert!(snap2.virtual_placement(10, 5).is_some());
    let ph_cell = &snap2.visible_rows[5].cells()[0];
    let dec =
        decode_placeholder_cell(ph_cell, snap2.cell_extra(ph_cell.extra_id)).expect("u+10eeee");
    assert_eq!((dec.image_id, dec.placement_id), (10, 5));
    assert!(phase2.inspect_shapes().mesh_shape_count >= 4);

    // Phase 3: Render Sixel strip with ?1070l shared palette + ?8452h + P2=1 + Pan/Pad 2:1.
    h.runtime.input_mut().push_str(
        "\x1b[8;1H\x1b[?1070l\x1b[?8452h\
         \x1bP0;1;0q\"2;1;16;12#6;2;20;90;40#6!16~-#6!16~\x1b\\\
         \x1bP0;1;0q\"2;1;16;12#6!16~-#6!16~\x1b\\",
    );
    let phase3 = h.run_frame();
    assert_eq!(
        h.runtime
            .terminal()
            .active_snapshot()
            .image_placements()
            .len(),
        6
    );
    assert_eq!(h.runtime.terminal().image_placements().len(), 7);
    assert!(phase3.inspect_shapes().mesh_shape_count >= 6);

    // Phase 4: Narrow HiDPI resize reflow remaps abs_line while keeping all meshes inside term_rect.
    let img1_abs_before = h
        .runtime
        .terminal()
        .image_placements()
        .iter()
        .find(|p| p.image_id == 1)
        .expect("img 1 before reflow")
        .abs_line;
    h.set_pixels_per_point(1.5);
    h.set_viewport_size(320.0, 460.0);
    let phase4 = h.run_frame();
    let img1_abs_after = h
        .runtime
        .terminal()
        .image_placements()
        .iter()
        .find(|p| p.image_id == 1)
        .expect("img 1 after reflow")
        .abs_line;
    assert!(
        img1_abs_after.0 > img1_abs_before.0,
        "narrower viewport wraps caption and pushes img 1 abs_line down: {img1_abs_before:?} -> {img1_abs_after:?}"
    );
    let term_rect = phase4.terminal_response.response.rect;
    let shapes4 = phase4.inspect_shapes();
    assert!(
        shapes4
            .mesh_vertex_positions()
            .iter()
            .all(|p| p.x >= term_rect.min.x - 1.0 && p.x <= term_rect.max.x + 1.0)
    );

    // Phase 5: Delete root i=1 (cascade-deletes relative child i=11) and verify egui texture eviction.
    h.runtime.input_mut().push_str("\x1b_Ga=d,d=I,i=1;\x1b\\");
    let phase5 = h.run_frame();
    assert!(h.runtime.terminal().image_data(1).is_none());
    assert!(h.runtime.terminal().image_data(11).is_none());
    assert!(phase5.texture_deltas.freed_count >= 2);
}
