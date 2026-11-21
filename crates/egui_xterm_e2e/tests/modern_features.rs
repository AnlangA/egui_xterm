//! End-to-end integration tests for modern terminal features:
//! - Kitty Graphics Protocol (`APC G ...`)
//! - Sixel Graphics (`DCS ... q ... ST`)
//! - DCS queries (`XTGETTCAP`, `DECRQSS`) & `XTVERSION`
//! - `DECSLRM` horizontal margins & `XTWINOPS` (`14t/16t/18t/22t/23t`)
//! - `Mouse 1016` (SGR-Pixels), `DEC ? 2027` (`DECRQM`), `DEC ? 2031` / `DSR ? 996 n`
//! - `OSC 9;4` progress, `OSC 9/99/777` notifications, `OSC 22` mouse shape
//! - `SGR Blink`, `Visual Bell`, `IME Preedit active range`, `Search bar error/whole-word`,
//!   and `Hyperlink Ctrl+Click`

#![forbid(unsafe_code)]

#[cfg(feature = "hyperlinks")]
use egui::pos2;
use egui::{Event, ImeEvent, Modifiers, PointerButton};
use egui_xterm::{TerminalConfig, TerminalEvent, Theme};
use egui_xterm_core::{
    CoreConfig, MouseButton, MouseEncoding, MouseEventKind, MouseInputEvent, MouseModifiers,
    Terminal,
};
use egui_xterm_e2e::HeadlessTermHarness;

#[cfg(feature = "kitty-graphics")]
mod kitty_graphics_e2e {
    use super::*;

    fn encode_base64(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
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

    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(0x78);
        out.push(0x01);
        let mut pos = 0;
        while pos < data.len() || pos == 0 {
            let end = (pos + 65535).min(data.len());
            let is_final = end == data.len();
            out.push(u8::from(is_final));
            #[expect(
                clippy::expect_used,
                reason = "test helper: bounded by min(..., 65535)"
            )]
            let len = u16::try_from(end - pos).expect("chunk fits in u16");
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

    fn build_minimal_rgba_png(width: u32, height: u32, rgba_pixels: &[u8]) -> Vec<u8> {
        fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            #[expect(clippy::expect_used, reason = "test helper: small test PNG chunks")]
            let len = u32::try_from(data.len()).expect("png chunk fits in u32");
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let mut crc = !0u32;
            for &b in kind.iter().chain(data.iter()) {
                crc ^= u32::from(b);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
                }
            }
            out.extend_from_slice(&(!crc).to_be_bytes());
        }

        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.push(8); // bit depth
        ihdr.push(6); // RGBA
        ihdr.push(0); // compression
        ihdr.push(0); // filter
        ihdr.push(0); // interlace
        push_chunk(&mut out, b"IHDR", &ihdr);

        #[expect(clippy::expect_used, reason = "test helper: small test PNG width")]
        let stride = usize::try_from(width).expect("width fits") * 4;
        let mut scanlines = Vec::new();
        for row in rgba_pixels.chunks_exact(stride) {
            scanlines.push(0); // filter None
            scanlines.extend_from_slice(row);
        }
        let idat = zlib_stored(&scanlines);
        push_chunk(&mut out, b"IDAT", &idat);
        push_chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn kitty_graphics_rgba_rgb_png_chunked_query_and_delete_e2e() {
        let mut h = HeadlessTermHarness::scripted();

        // 1. Query (a=q) replies OK without storing image.
        let q_b64 = encode_base64(&[255, 0, 0, 255]);
        h.runtime
            .input_mut()
            .push_str(format!("\x1b_Ga=q,i=41,s=1,v=1,f=32;{q_b64}\x1b\\"));
        h.run_frame();
        assert_eq!(h.runtime.input().written_str(), "\x1b_Gi=41;OK\x1b\\");
        assert!(h.runtime.terminal().image_data(41).is_none());
        h.runtime.input_mut().clear_written();

        // 2. Chunked RGBA (a=T, f=32, m=1 -> m=0).
        let rgba = vec![10, 20, 30, 255, 40, 50, 60, 255];
        let full_b64 = encode_base64(&rgba);
        let (c1, c2) = full_b64.split_at(5);
        h.runtime.input_mut().push_str(format!(
            "\x1b_Ga=T,i=11,p=3,f=32,s=2,v=1,c=2,r=1,m=1;{c1}\x1b\\"
        ));
        h.run_frame();
        assert!(h.runtime.terminal().image_data(11).is_none());

        h.runtime
            .input_mut()
            .push_str(format!("\x1b_Gm=0;{c2}\x1b\\"));
        h.run_frame();
        assert_eq!(h.runtime.input().written_str(), "\x1b_Gi=11,p=3;OK\x1b\\");
        h.runtime.input_mut().clear_written();
        let img11 = h
            .runtime
            .terminal()
            .image_data(11)
            .expect("image 11 stored");
        assert_eq!((img11.width, img11.height), (2, 1));
        assert_eq!(img11.rgba, rgba);
        assert_eq!(h.runtime.terminal().image_placements().len(), 1);

        // 3. Compressed RGB (a=t, f=24, o=z) + Put (a=p) with q=1.
        let rgb = vec![1, 2, 3, 4, 5, 6];
        let rgb_z = encode_base64(&zlib_stored(&rgb));
        h.runtime
            .input_mut()
            .push_str(format!("\x1b_Ga=t,i=12,f=24,o=z,s=2,v=1,q=1;{rgb_z}\x1b\\"));
        h.run_frame();
        assert!(h.runtime.input().written_bytes().is_empty());
        assert_eq!(
            h.runtime
                .terminal()
                .image_data(12)
                .expect("image 12 stored")
                .rgba,
            vec![1, 2, 3, 255, 4, 5, 6, 255]
        );
        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=p,i=12,p=1,c=1,r=1,C=1,z=-2;\x1b\\");
        h.run_frame();
        assert_eq!(h.runtime.terminal().image_placements().len(), 2);

        // 4. PNG (a=T, f=100).
        let png = build_minimal_rgba_png(1, 1, &[9, 8, 7, 255]);
        let png_b64 = encode_base64(&png);
        h.runtime
            .input_mut()
            .push_str(format!("\x1b_Ga=T,i=13,f=100,q=2;{png_b64}\x1b\\"));
        h.run_frame();
        assert_eq!(
            h.runtime
                .terminal()
                .image_data(13)
                .expect("png stored")
                .rgba,
            vec![9, 8, 7, 255]
        );
        assert_eq!(h.runtime.terminal().image_placements().len(), 3);

        // 5. Delete by z-index (d=Z, z=-2) and by ID (d=I, i=11).
        h.runtime
            .input_mut()
            .push_str("\x1b_Ga=d,d=Z,z=-2;\x1b\\\x1b_Ga=d,d=I,i=11;\x1b\\");
        h.run_frame();
        assert!(h.runtime.terminal().image_data(12).is_none());
        assert!(h.runtime.terminal().image_data(11).is_none());
        assert_eq!(h.runtime.terminal().image_placements().len(), 1);
    }
}

#[cfg(feature = "sixel")]
#[test]
fn sixel_raster_colors_repeat_and_decsdm_e2e() {
    let mut h = HeadlessTermHarness::scripted();

    // Raster 4x12, reg 1 = RGB(100,0,0), reg 2 = HLS(120,50,100), repeat !2~, newline -, reg 2 @
    h.runtime
        .input_mut()
        .push_str("\x1bPq\"1;1;4;12#1;2;100;0;0#2;1;120;50;100#1!2~-#2@\x1b\\");
    h.run_frame();

    let placements = h.runtime.terminal().image_placements();
    assert_eq!(placements.len(), 1);
    let img = h
        .runtime
        .terminal()
        .image_data(placements[0].image_id)
        .expect("sixel image stored");
    assert_eq!((img.width, img.height), (4, 12));
    assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
    assert_eq!(&img.rgba[12..16], &[0, 0, 0, 255]);
    let row6 = 6 * 4 * 4;
    assert_eq!(&img.rgba[row6..row6 + 4], &[0, 255, 0, 255]);

    // Enable DECSDM (?80h): places at (0, 0) without advancing cursor.
    h.runtime
        .input_mut()
        .push_str("\x1b[?80h\x1b[5;10H\x1bP0;1;0q#1?~\x1b\\");
    h.run_frame();
    assert_eq!(h.runtime.terminal().cursor_pos(), (9, 4));
    let placements = h.runtime.terminal().image_placements();
    assert_eq!(placements.len(), 2);
    assert_eq!(placements[1].col, 0);
}

#[test]
fn dcs_xtgettcap_decrqss_and_xtversion_e2e() {
    let mut h = HeadlessTermHarness::scripted();

    // XTGETTCAP for TN (544e), RGB (524742), colors (636f6c6f7273), unknown (464f4f), invalid hex
    h.runtime
        .input_mut()
        .push_str("\x1bP+q544e;524742;636f6c6f7273;464f4f\x1b\\\x1bP+qZZZZ\x1b\\");
    h.run_frame();
    let reply = h.runtime.input().written_str();
    assert!(reply.contains("\x1bP1+r544e=656775695F787465726D\x1b\\"));
    assert!(reply.contains("\x1bP1+r524742=382F382F38\x1b\\"));
    assert!(reply.contains("\x1bP1+r636f6c6f7273=323536\x1b\\"));
    assert!(reply.contains("\x1bP0+r464f4f\x1b\\"));
    assert!(reply.contains("\x1bP0+rZZZZ\x1b\\"));
    h.runtime.input_mut().clear_written();

    // DECRQSS for SGR (m), DECSTBM (r), DECSCUSR (q), DECSCA ("q), DECSLRM (s), unsupported
    h.runtime
        .input_mut()
        .push_str("\x1b[1;4:3;38;2;10;20;30m\x1bP$qm\x1b\\");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1bP1$r0;1;4:3;38;2;10;20;30m\x1b\\"
    );
    h.runtime.input_mut().clear_written();

    h.runtime
        .input_mut()
        .push_str("\x1b[3;10r\x1bP$qr\x1b\\\x1b[4 q\x1bP$q q\x1b\\\x1bP$q\"q\x1b\\\x1bP$qs\x1b\\\x1bP$qxyz\x1b\\");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1bP1$r3;10r\x1b\\\x1bP1$r4 q\x1b\\\x1bP1$r0\"q\x1b\\\x1bP0$r\x1b\\\x1bP0$r\x1b\\"
    );
    h.runtime.input_mut().clear_written();

    // DECRQSS s succeeds when ?69 is enabled
    h.runtime
        .input_mut()
        .push_str("\x1b[?69h\x1b[5;20s\x1bP$qs\x1b\\");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1bP1$r5;20s\x1b\\");
    h.runtime.input_mut().clear_written();

    // XTVERSION (CSI > q)
    h.runtime.input_mut().push_str("\x1b[>q");
    h.run_frame();
    let ver = h.runtime.input().written_str();
    assert!(ver.starts_with("\x1bP>|egui_xterm "));
    assert!(ver.ends_with("\x1b\\"));
}

#[test]
fn decslrm_and_xtwinops_e2e() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(800.0, 480.0);
    let first = h.run_frame();
    let cols = first.terminal_response.cols;
    let rows = first.terminal_response.rows;

    // XTWINOPS 14t, 16t, 18t
    h.runtime.input_mut().push_str("\x1b[14t\x1b[16t\x1b[18t");
    h.run_frame();
    let replies = h.runtime.input().written_str();
    assert!(replies.contains("\x1b[4;"));
    assert!(replies.contains("\x1b[6;"));
    assert!(replies.contains(&format!("\x1b[8;{rows};{cols}t")));
    h.runtime.input_mut().clear_written();

    // XTWINOPS 22t / 23t title stack
    h.runtime
        .input_mut()
        .push_str("\x1b]0;BaseTitle\x07\x1b[22;0t\x1b]0;InnerTitle\x07");
    let pushed = h.run_frame();
    assert_eq!(h.runtime.terminal().title(), "InnerTitle");
    assert!(
        pushed
            .terminal_response
            .events
            .contains(&TerminalEvent::TitleChanged("InnerTitle".to_owned()))
    );

    h.runtime.input_mut().push_str("\x1b[23;0t");
    let popped = h.run_frame();
    assert_eq!(h.runtime.terminal().title(), "BaseTitle");
    assert!(
        popped
            .terminal_response
            .events
            .contains(&TerminalEvent::TitleChanged("BaseTitle".to_owned()))
    );

    // DECSLRM: ?69h + CSI 3;6 s wraps & scrolls inside horizontal margins
    h.runtime
        .input_mut()
        .push_str("\x1b[2J\x1b[?69h\x1b[3;6s\x1b[1;3HABCDEFGH\rX");
    let margined = h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("  ABCD")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("  XFGH")
    );
    let shapes = margined.inspect_shapes();
    assert!(shapes.contains_text("ABCD"));
    assert!(shapes.contains_text("XFGH"));

    // ?69l resets horizontal margins and disables DECSLRM.
    h.runtime
        .input_mut()
        .push_str("\x1b[?69l\x1b[?69$p\x1b[2J\x1b[1;1H0123456789");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b[?69;2$y");
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("0123456789")
    );
}

#[test]
fn mouse_1016_dec_2027_dec_2031_and_dsr_996_e2e() {
    let mut h = HeadlessTermHarness::scripted();

    // DEC ?2027 (grapheme clustering) DECRQM
    h.runtime
        .input_mut()
        .push_str("\x1b[?2027$p\x1b[?2027l\x1b[?2027$p\x1b[?2027h");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b[?2027;1$y\x1b[?2027;2$y"
    );
    h.runtime.input_mut().clear_written();

    // DEC ?2031 + DSR ?996 n (dark -> 1, light -> 2)
    h.runtime
        .input_mut()
        .push_str("\x1b[?996n\x1b[?2031$p\x1b[?2031h\x1b[?2031$p");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b[?997;1n\x1b[?2031;2$y\x1b[?2031;1$y"
    );
    h.runtime.input_mut().clear_written();

    h.config.theme = Theme::light();
    h.run_frame();
    assert!(
        h.runtime.input().written_str().contains("\x1b[?997;2n"),
        "switching to light theme with ?2031h must emit unsolicited DSR ?997;2n"
    );
    h.runtime.input_mut().clear_written();

    // Mouse 1016 (SGR-Pixels) mode query and pixel encoding
    h.runtime
        .input_mut()
        .push_str("\x1b[?1002h\x1b[?1016h\x1b[?1016$p");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b[?1016;1$y");
    assert_eq!(
        h.runtime.terminal().modes().mouse_encoding,
        MouseEncoding::SgrPixels
    );

    let mut term = Terminal::new(80, 24, CoreConfig::default());
    term.advance(b"\x1b[?1002h\x1b[?1016h");
    let press = MouseInputEvent {
        kind: MouseEventKind::Press,
        button: Some(MouseButton::Left),
        col: 4,
        row: 2,
        modifiers: MouseModifiers {
            shift: false,
            alt: false,
            ctrl: true,
        },
    };
    assert_eq!(
        term.encode_mouse_event_pixels(press, 321, 654).as_deref(),
        Some(b"\x1b[<16;321;654M".as_slice())
    );
    let release = MouseInputEvent {
        kind: MouseEventKind::Release,
        ..press
    };
    assert_eq!(
        term.encode_mouse_event_pixels(release, 321, 654).as_deref(),
        Some(b"\x1b[<16;321;654m".as_slice())
    );
}

#[test]
fn osc_progress_notifications_and_mouse_shape_e2e() {
    let mut h = HeadlessTermHarness::scripted();

    h.runtime.input_mut().push_str(concat!(
        "\x1b]9;4;1;75\x07",
        "\x1b]9;Task finished\x07",
        "\x1b]99;p=title:d=0;CI\x1b\\",
        "\x1b]99;p=body:e=1;cGFzcw==\x1b\\",
        "\x1b]777;notify;Deploy;Green\x07",
        "\x1b]22;crosshair\x07",
    ));
    let outcome = h.run_frame();
    assert_eq!(
        outcome.terminal_response.events,
        vec![
            TerminalEvent::Progress {
                state: 1,
                progress: 75,
            },
            TerminalEvent::Notification {
                title: String::new(),
                body: "Task finished".to_owned(),
            },
            TerminalEvent::Notification {
                title: "CI".to_owned(),
                body: "pass".to_owned(),
            },
            TerminalEvent::Notification {
                title: "Deploy".to_owned(),
                body: "Green".to_owned(),
            },
            TerminalEvent::MouseShape("crosshair".to_owned()),
        ]
    );
}

#[test]
fn sgr_blink_and_visual_bell_e2e() {
    let cfg = TerminalConfig {
        cursor_blink: false,
        idle_poll_interval_ms: 0,
        enable_blink: true,
        visual_bell_duration_ms: 150,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.runtime.input_mut().push_str("\x1b[5mBLINKME\x1b[0m");

    // At t=0.0 (on phase), BLINKME text has opaque vertices.
    let on_outcome = h.run_frame();
    let on_visible = on_outcome.inspect_shapes().text_entries.iter().any(|e| {
        e.text.contains("BLINKME")
            && e.galley
                .rows
                .iter()
                .any(|r| r.row.visuals.mesh.vertices.iter().any(|v| v.color.a() > 0))
    });
    assert!(on_visible, "SGR 5 text must be visible in on phase");

    // At t=0.6 (off phase), BLINKME text vertices are transparent.
    h.advance_time(0.6);
    let off_outcome = h.run_frame();
    let off_opaque = off_outcome.inspect_shapes().text_entries.iter().any(|e| {
        e.text.contains("BLINKME")
            && e.galley
                .rows
                .iter()
                .any(|r| r.row.visuals.mesh.vertices.iter().any(|v| v.color.a() > 0))
    });
    assert!(!off_opaque, "SGR 5 text must be hidden in off phase");

    // Visual bell: BEL triggers a translucent overlay rect that decays and disappears.
    h.advance_time(0.4); // t=1.0
    h.runtime.input_mut().push_str("\x07");
    let bell_on = h.run_frame();
    assert!(
        bell_on
            .terminal_response
            .events
            .contains(&TerminalEvent::Bell)
    );
    let flash_alphas: Vec<u8> = bell_on
        .inspect_shapes()
        .rect_fills
        .iter()
        .filter_map(|(r, c)| (c.a() > 0 && c.a() < 100 && r.width() > 300.0).then_some(c.a()))
        .collect();
    assert_eq!(flash_alphas.len(), 1);

    h.advance_time(0.2);
    let bell_expired = h.run_frame();
    let expired_flashes = bell_expired
        .inspect_shapes()
        .rect_fills
        .iter()
        .filter(|(r, c)| c.a() > 0 && c.a() < 100 && r.width() > 300.0)
        .count();
    assert_eq!(expired_flashes, 0);
}

#[test]
fn ime_preedit_active_range_e2e() {
    let mut h = HeadlessTermHarness::scripted();
    let first = h.run_frame();
    let center = first.terminal_response.response.rect.center();
    h.inject_pointer_button(center, PointerButton::Primary, true, Modifiers::NONE);
    h.inject_pointer_button(center, PointerButton::Primary, false, Modifiers::NONE);
    h.run_frame();

    h.queue_event(Event::Ime(ImeEvent::Preedit {
        text: "nihon".to_owned(),
        active_range_chars: Some(1..3),
    }));
    let preedit_frame = h.run_frame();
    let shapes = preedit_frame.inspect_shapes();
    assert!(
        shapes.contains_text("nihon"),
        "IME preedit text must be rendered inline"
    );
    let has_active_highlight = shapes.rect_fills.iter().any(|(_, color)| color.a() == 36);
    assert!(
        has_active_highlight,
        "IME preedit active_range_chars must render a translucent highlight rect"
    );
    assert!(preedit_frame.platform_output.ime.is_some());

    // Empty preedit clears preedit text.
    h.queue_event(Event::Ime(ImeEvent::Preedit {
        text: String::new(),
        active_range_chars: None,
    }));
    let cleared_frame = h.run_frame();
    assert!(!cleared_frame.inspect_shapes().contains_text("nihon"));
}

#[cfg(feature = "search")]
#[test]
fn search_bar_error_and_whole_word_e2e() {
    let mut h = HeadlessTermHarness::scripted();
    h.runtime.input_mut().push_str("dog hotdog bulldog\r\n");
    h.run_frame();

    h.runtime.open_search();
    h.runtime.search_bar_mut().is_regex = true;
    h.run_frame();

    // Invalid regex produces inline error message.
    h.inject_text("[bad");
    let err_frame = h.run_frame();
    assert!(h.runtime.search_bar_mut().last_error.is_some());
    assert!(
        err_frame
            .inspect_shapes()
            .contains_text("invalid search regex")
    );

    // Whole-word literal search matches only "dog" (1 match).
    h.runtime.search_bar_mut().is_regex = false;
    h.runtime.search_bar_mut().whole_word = true;
    h.runtime.search_bar_mut().query_text = "do".to_owned();
    h.inject_text("g");
    let ww_frame = h.run_frame();
    assert!(h.runtime.search_bar_mut().last_error.is_none());
    assert_eq!(h.runtime.terminal().search_matches().len(), 1);
    assert!(ww_frame.inspect_shapes().contains_text("0/1"));
}

#[cfg(feature = "hyperlinks")]
#[test]
fn hyperlink_ctrl_click_e2e() {
    let cfg = TerminalConfig {
        hyperlink_require_ctrl: true,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.runtime
        .input_mut()
        .push_str("\x1b]8;;https://example.org/e2e\x07clickme\x1b]8;;\x07");
    let first = h.run_frame();
    let r = &first.terminal_response;
    let link_pos = pos2(
        r.response.rect.min.x + 1.5 * r.cell_size.x,
        r.response.rect.min.y + 0.5 * r.cell_size.y,
    );

    // Plain click without Ctrl must not open URL.
    h.inject_pointer_move(link_pos);
    h.inject_pointer_button(link_pos, PointerButton::Primary, true, Modifiers::NONE);
    h.inject_pointer_button(link_pos, PointerButton::Primary, false, Modifiers::NONE);
    let plain = h.run_frame();
    assert!(plain.opened_urls().is_empty());

    // Ctrl+Click after double-click window opens URL and emits HyperlinkActivated.
    h.advance_time(1.0);
    h.inject_pointer_button(link_pos, PointerButton::Primary, true, Modifiers::CTRL);
    h.inject_pointer_button(link_pos, PointerButton::Primary, false, Modifiers::CTRL);
    let ctrl = h.run_frame();
    assert!(
        ctrl.opened_urls()
            .iter()
            .any(|u| u == "https://example.org/e2e")
    );
    assert!(ctrl.terminal_response.events.iter().any(|ev| matches!(
        ev,
        TerminalEvent::HyperlinkActivated { uri, .. } if uri == "https://example.org/e2e"
    )));
    // Also verify Shape::Rect/Text inspection works cleanly on hyperlink frame.
    assert!(
        ctrl.inspect_shapes()
            .text_entries
            .iter()
            .any(|e| matches!(e.text.as_str(), t if t.contains("clickme")))
    );
}
