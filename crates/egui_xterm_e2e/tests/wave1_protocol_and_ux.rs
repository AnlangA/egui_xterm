//! Wave 1 opaque-box E2E tests for implemented modern terminal protocol and UX features.

#![forbid(unsafe_code)]

use egui::{Color32, CursorIcon, Event, Modifiers, PointerButton, Pos2};
use egui_xterm::{ColorScheme, CursorShape, TerminalConfig, TerminalEvent};
use egui_xterm_core::{MouseEncoding, MouseProtocol, PackedColor};
use egui_xterm_e2e::HeadlessTermHarness;

fn rgb_to_c32(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0F)]));
    }
    out
}

fn hex_encode_upper(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0F)]));
    }
    out
}

fn format_x11_rgb(rgb: [u8; 3]) -> String {
    let r = u16::from(rgb[0]) * 0x0101;
    let g = u16::from(rgb[1]) * 0x0101;
    let b = u16::from(rgb[2]) * 0x0101;
    format!("rgb:{r:04x}/{g:04x}/{b:04x}")
}

#[test]
fn w1_osc_10_11_12_set_query_terminator_and_110_111_112_reset_e2e() {
    let mut h = HeadlessTermHarness::scripted();
    h.config.cursor_blink = false;
    h.focus_terminal();

    let default_fg = rgb_to_c32(h.config.theme.scheme.foreground);
    let default_bg = rgb_to_c32(h.config.theme.scheme.background);
    let default_cursor = rgb_to_c32(h.config.theme.scheme.cursor);

    // Set OSC 10 (fg), OSC 11 (bg via ST), OSC 12 (cursor) and render text.
    h.runtime
        .input_mut()
        .push_str("\x1b]10;#123456\x07\x1b]11;rgb:11/22/33\x1b\\\x1b]12;#abcdef\x07HELLO");
    let set_frame = h.run_frame();
    assert!(
        set_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("HELLO")
    );

    let set_fg = Color32::from_rgb(0x12, 0x34, 0x56);
    let set_bg = Color32::from_rgb(0x11, 0x22, 0x33);
    let set_cursor = Color32::from_rgb(0xab, 0xcd, 0xef);
    let set_shapes = set_frame.inspect_shapes();
    assert!(
        set_shapes.has_text_fg_color(set_fg) || set_shapes.has_rect_or_mesh_color(set_fg),
        "OSC 10 foreground color must propagate to rendered text"
    );
    assert!(
        set_shapes.has_rect_or_mesh_color(set_bg),
        "OSC 11 background color must propagate to rendered background mesh"
    );
    assert!(
        set_shapes.has_rect_or_mesh_color(set_cursor),
        "OSC 12 cursor color must propagate to rendered cursor quad"
    );

    // Chained query preserves BEL terminator; individual ST query preserves ST terminator.
    h.runtime
        .input_mut()
        .push_str("\x1b]10;?;?;?\x07\x1b]11;?\x1b\\");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        concat!(
            "\x1b]10;rgb:1212/3434/5656\x07",
            "\x1b]11;rgb:1111/2222/3333\x07",
            "\x1b]12;rgb:abab/cdcd/efef\x07",
            "\x1b]11;rgb:1111/2222/3333\x1b\\",
        )
    );
    h.runtime.input_mut().clear_written();

    // Unrecognized named color spec is ignored without altering palette.
    h.runtime
        .input_mut()
        .push_str("\x1b]10;notacolor\x07\x1b]10;?\x07");
    let invalid_frame = h.run_frame();
    assert!(
        !invalid_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b]10;rgb:1212/3434/5656\x07"
    );
    h.runtime.input_mut().clear_written();

    // Reset OSC 110, 111, 112 restores theme defaults in both queries and rendered shapes.
    h.runtime
        .input_mut()
        .push_str("\x1b]110\x07\x1b]111\x07\x1b]112\x07\x1b]10;?;?;?\x07");
    let reset_frame = h.run_frame();
    assert!(
        reset_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    let expected_reset_reply = format!(
        "\x1b]10;{}\x07\x1b]11;{}\x07\x1b]12;{}\x07",
        format_x11_rgb(h.config.theme.scheme.foreground),
        format_x11_rgb(h.config.theme.scheme.background),
        format_x11_rgb(h.config.theme.scheme.cursor),
    );
    assert_eq!(h.runtime.input().written_str(), expected_reset_reply);

    let reset_shapes = reset_frame.inspect_shapes();
    assert!(
        reset_shapes.has_text_fg_color(default_fg)
            || reset_shapes.has_rect_or_mesh_color(default_fg)
    );
    assert!(reset_shapes.has_rect_or_mesh_color(default_bg));
    assert!(reset_shapes.has_rect_or_mesh_color(default_cursor));
    assert!(!reset_shapes.has_rect_or_mesh_color(set_bg));
    assert!(!reset_shapes.has_rect_or_mesh_color(set_cursor));
}

#[test]
fn w1_osc_4_chained_and_104_reset_all_and_selective() {
    let mut h = HeadlessTermHarness::scripted();
    h.focus_terminal();
    let base_scheme = h.config.theme.scheme.clone();

    // Chained OSC 4 sets ANSI slot 1 and extended 256-color slot 200, then renders both.
    h.runtime
        .input_mut()
        .push_str("\x1b]4;1;#ff0000;200;rgb:00/ff/00\x07\x1b[31mR\x1b[48;5;200mG\x1b[0m");
    let set_frame = h.run_frame();
    assert!(
        set_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    let shapes = set_frame.inspect_shapes();
    let red = Color32::from_rgb(255, 0, 0);
    let green = Color32::from_rgb(0, 255, 0);
    assert!(shapes.has_text_fg_color(red) || shapes.has_rect_or_mesh_color(red));
    assert!(shapes.has_rect_or_mesh_color(green));

    // Chained query with ST terminator.
    h.runtime.input_mut().push_str("\x1b]4;1;?;200;?\x1b\\");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b]4;1;rgb:ffff/0000/0000\x1b\\\x1b]4;200;rgb:0000/ffff/0000\x1b\\"
    );
    h.runtime.input_mut().clear_written();

    // Selective reset OSC 104;1 resets slot 1 while preserving slot 200.
    h.runtime
        .input_mut()
        .push_str("\x1b]104;1\x07\x1b]4;1;?;200;?\x07");
    h.run_frame();
    let default_slot1 = format_x11_rgb(base_scheme.ansi[1]);
    assert_eq!(
        h.runtime.input().written_str(),
        format!("\x1b]4;1;{default_slot1}\x07\x1b]4;200;rgb:0000/ffff/0000\x07")
    );
    h.runtime.input_mut().clear_written();

    // Set additional slots then bare OSC 104 resets all ANSI and extended overrides.
    h.runtime
        .input_mut()
        .push_str("\x1b]4;2;#102030;201;#405060\x07\x1b]104\x07\x1b]4;1;?;2;?;200;?;201;?\x07");
    let reset_all_frame = h.run_frame();
    assert!(
        reset_all_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    let default_slot2 = format_x11_rgb(base_scheme.ansi[2]);
    let default_200 = format_x11_rgb(ColorScheme::xterm_256_color(200, &base_scheme.ansi));
    let default_201 = format_x11_rgb(ColorScheme::xterm_256_color(201, &base_scheme.ansi));
    assert_eq!(
        h.runtime.input().written_str(),
        format!(
            "\x1b]4;1;{default_slot1}\x07\x1b]4;2;{default_slot2}\x07\x1b]4;200;{default_200}\x07\x1b]4;201;{default_201}\x07"
        )
    );
}

#[test]
fn w1_decrqss_sgr_margins_all_decscusr_and_overflow() {
    let mut h = HeadlessTermHarness::scripted();
    let first = h.run_frame();
    let rows = first.terminal_response.rows;

    // Full SGR attributes with colon subparameters normalize to semicolon format on DECRQSS m.
    h.runtime
        .input_mut()
        .push_str("\x1b[1;2;3;4:5;7;8;9;53;38:5:196;48;2;10;20;30;58:2::40:50:60m\x1bP$qm\x1b\\");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1bP1$r0;1;2;3;4:5;7;8;9;53;38;5;196;48;2;10;20;30;58;2;40;50;60m\x1b\\"
    );
    h.runtime.input_mut().clear_written();

    // SGR 0 resets DECRQSS m to 0m.
    h.runtime.input_mut().push_str("\x1b[0m\x1bP$qm\x1b\\");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1bP1$r0m\x1b\\");
    h.runtime.input_mut().clear_written();

    // DECSCUSR 1..=6 and 0 (reset to config default Block+blink -> 1 q).
    for (param, expected_code) in [(1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (6, 6), (0, 1)] {
        h.runtime
            .input_mut()
            .push_str(format!("\x1b[{param} q\x1bP$q q\x1b\\"));
        h.run_frame();
        assert_eq!(
            h.runtime.input().written_str(),
            format!("\x1bP1$r{expected_code} q\x1b\\"),
            "DECSCUSR {param} query mismatch"
        );
        h.runtime.input_mut().clear_written();
    }

    // DECSTBM set and reset via CSI r reflected in DECRQSS r.
    h.runtime
        .input_mut()
        .push_str("\x1b[2;8r\x1bP$qr\x1b\\\x1b[r\x1bP$qr\x1b\\");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        format!("\x1bP1$r2;8r\x1b\\\x1bP1$r1;{rows}r\x1b\\")
    );
    h.runtime.input_mut().clear_written();

    // DECRQSS s returns 0$r when ?69 is disabled, 1$r{l};{r}s when enabled.
    h.runtime
        .input_mut()
        .push_str("\x1bP$qs\x1b\\\x1b[?69h\x1b[4;12s\x1bP$qs\x1b\\\x1b[?69l\x1bP$qs\x1b\\");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1bP0$r\x1b\\\x1bP1$r4;12s\x1b\\\x1bP0$r\x1b\\"
    );
    h.runtime.input_mut().clear_written();

    // DCS payload > MAX_DCS_BYTES (4096) drops cleanly without crashing or replying, and next DCS works.
    let mut overflow = String::from("\x1bP$q");
    overflow.push_str(&"x".repeat(5000));
    overflow.push_str("\x1b\\\x1bP$qm\x1b\\");
    h.runtime.input_mut().push_str(overflow);
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1bP1$r0m\x1b\\");
}

#[test]
fn w1_xtgettcap_all_caps_empty_segments_and_odd_hex() {
    let mut h = HeadlessTermHarness::scripted();
    h.run_frame();

    let caps: [(&[u8], &[u8]); 12] = [
        (b"TN", b"egui_xterm"),
        (b"name", b"egui_xterm"),
        (b"RGB", b"8/8/8"),
        (b"Tc", b"1"),
        (b"Su", b"1"),
        (b"colors", b"256"),
        (b"Co", b"256"),
        (b"Smulx", b"\x1b[4:%p1%dm"),
        (
            b"Setulc",
            b"\x1b[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m",
        ),
        (b"Ms", b"\x1b]52;%p1%s;%p2%s\x07"),
        (b"Ss", b"\x1b[%p1%d q"),
        (b"Se", b"\x1b[2 q"),
    ];

    let query_joined: String = caps
        .iter()
        .map(|(k, _)| hex_encode(k))
        .collect::<Vec<_>>()
        .join(";");
    h.runtime
        .input_mut()
        .push_str(format!("\x1bP+q{query_joined}\x1b\\"));
    h.run_frame();

    let expected_all: String = caps
        .iter()
        .map(|(k, v)| format!("\x1bP1+r{}={}\x1b\\", hex_encode(k), hex_encode_upper(v)))
        .collect();
    assert_eq!(h.runtime.input().written_str(), expected_all);
    h.runtime.input_mut().clear_written();

    // Empty segments are skipped; odd-length hex, non-hex, and unknown keys reply with 0+r.
    h.runtime
        .input_mut()
        .push_str("\x1bP+q544e;;524742;\x1b\\\x1bP+q544;ZZZZ;464f4f\x1b\\");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        concat!(
            "\x1bP1+r544e=656775695F787465726D\x1b\\",
            "\x1bP1+r524742=382F382F38\x1b\\",
            "\x1bP0+r544\x1b\\",
            "\x1bP0+rZZZZ\x1b\\",
            "\x1bP0+r464f4f\x1b\\",
        )
    );
}

#[test]
fn w1_csi_s_scosc_vs_decslrm_disambiguation_and_invalid_ranges() {
    let mut h = HeadlessTermHarness::scripted();
    let first = h.run_frame();
    let cols = first.terminal_response.cols;
    assert!(cols >= 20);

    // With ?69 disabled (default), CSI s / CSI u saves and restores cursor (SCOSC / SCORC).
    h.runtime
        .input_mut()
        .push_str("\x1b[?69$p\x1b[4;8H\x1b[s\x1b[10;15H\x1b[u");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b[?69;2$y");
    assert_eq!(h.runtime.terminal().cursor_pos(), (7, 3));
    h.runtime.input_mut().clear_written();

    // Enable ?69: CSI 3;7 s sets horizontal margins and homes cursor; invalid ranges are ignored.
    h.runtime
        .input_mut()
        .push_str("\x1b[?69h\x1b[?69$p\x1b[5;5H\x1b[3;7s\x1bP$qs\x1b\\");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b[?69;1$y\x1bP1$r3;7s\x1b\\"
    );
    assert_eq!(h.runtime.terminal().cursor_pos(), (0, 0));
    h.runtime.input_mut().clear_written();

    // Inverted (7;3) and equal (5;5) ranges are ignored.
    h.runtime
        .input_mut()
        .push_str("\x1b[7;3s\x1b[5;5s\x1bP$qs\x1b\\");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1bP1$r3;7s\x1b\\");
    h.runtime.input_mut().clear_written();

    // Parameterless CSI s under ?69h resets horizontal margins to 1..=cols.
    h.runtime.input_mut().push_str("\x1b[s\x1bP$qs\x1b\\");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        format!("\x1bP1$r1;{cols}s\x1b\\")
    );
    h.runtime.input_mut().clear_written();

    // DECSTR resets ?69 back to disabled so CSI s acts as SCOSC again.
    h.runtime
        .input_mut()
        .push_str("\x1b[3;7s\x1b[!p\x1b[?69$p\x1b[3;9H\x1b[s\x1b[6;12H\x1b[u");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b[?69;2$y");
    assert_eq!(h.runtime.terminal().cursor_pos(), (8, 2));
}

#[test]
fn w1_declrmm_interaction_with_cup_decom_cpr_ich_dch_il_dl_sd() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(400.0, 220.0);
    h.run_frame();

    // Configure box margins: rows 2..=5 (0-based 1..=4), cols 3..=7 (0-based 2..=6) + DECOM (?6h).
    h.runtime
        .input_mut()
        .push_str("\x1b[?69h\x1b[2;5r\x1b[3;7s\x1b[?6h");
    // CUP(1,1) -> (col=2, row=1) -> CPR/DECXCPR report (1,1); CUP(99,99) clamps to (4,5).
    h.runtime
        .input_mut()
        .push_str("\x1b[1;1H\x1b[6n\x1b[?6n\x1b[99;99H\x1b[6n");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b[1;1R\x1b[?1;1R\x1b[4;5R"
    );
    assert_eq!(h.runtime.terminal().cursor_pos(), (6, 4));
    h.runtime.input_mut().clear_written();

    // Disable DECOM and ?69h while seeding full rows 0..=5 across cols 0..9, then re-enable margins.
    h.runtime.input_mut().push_str(concat!(
        "\x1b[?6l\x1b[?69l\x1b[2J",
        "\x1b[1;1HTOP0000000",
        "\x1b[2;1HaaABCDEbb",
        "\x1b[3;1HccFGHIJdd",
        "\x1b[4;1HeeKLMNOff",
        "\x1b[5;1HggPQRSTuu",
        "\x1b[6;1HBOT0000000",
        "\x1b[?69h\x1b[2;5r\x1b[3;7s",
    ));
    h.run_frame();

    // ICH (@) and DCH (P) at row 2 col 4 (0-based row 1, col 3 -> 'B') stay inside cols 2..=6.
    h.runtime.input_mut().push_str("\x1b[2;4H\x1b[@");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("aaA BCDbb")
    );
    h.runtime.input_mut().push_str("\x1b[2;4H\x1b[P");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("aaABCD bb")
    );

    // IL (L) / DL (M) outside horizontal margins (col 1 -> 0-based col 0) are no-ops.
    h.runtime.input_mut().push_str("\x1b[2;1H\x1b[L\x1b[M");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("aaABCD bb")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(4).as_deref(),
        Some("ggPQRSTuu")
    );

    // IL (L) inside margins at row 2 col 3 (0-based row 1, col 2) scrolls only cols 2..=6 down.
    h.runtime.input_mut().push_str("\x1b[2;3H\x1b[L");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("TOP0000000")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("aa     bb")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(2).as_deref(),
        Some("ccABCD dd")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(3).as_deref(),
        Some("eeFGHIJff")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(4).as_deref(),
        Some("ggKLMNOuu")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(5).as_deref(),
        Some("BOT0000000")
    );

    // DL (M) at row 2 col 3 shifts cols 2..=6 in rows 1..=4 upward, inserting blank at row 4.
    h.runtime.input_mut().push_str("\x1b[2;3H\x1b[M");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("TOP0000000")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("aaABCD bb")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(2).as_deref(),
        Some("ccFGHIJdd")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(3).as_deref(),
        Some("eeKLMNOff")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(4).as_deref(),
        Some("gg     uu")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(5).as_deref(),
        Some("BOT0000000")
    );

    // SD (T) scrolls box rows 1..=4 down 1 line without touching outer columns or top/bottom rows.
    h.runtime.input_mut().push_str("\x1b[1T");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("TOP0000000")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("aa     bb")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(2).as_deref(),
        Some("ccABCD dd")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(3).as_deref(),
        Some("eeFGHIJff")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(4).as_deref(),
        Some("ggKLMNOuu")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(5).as_deref(),
        Some("BOT0000000")
    );
}

#[test]
fn w1_bracketed_paste_toggle_c1_del_sanitization_and_middle_click() {
    let mut h = HeadlessTermHarness::scripted();
    h.focus_terminal();
    let center = h
        .cell_center(2, 2)
        .expect("cell (2,2) inside default viewport");

    // Default ?2004l passes raw text verbatim without bracket wrappers.
    h.runtime.input_mut().push_str("\x1b[?2004$p");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b[?2004;2$y");
    h.runtime.input_mut().clear_written();

    h.inject_paste("raw\x1b[201~x");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "raw\x1b[201~x");
    h.runtime.input_mut().clear_written();

    // Enable ?2004h: strips ESC, BEL, DEL, C1 controls, and nested \x1b[201~ while keeping \t\n\r and UTF-8.
    h.runtime.input_mut().push_str("\x1b[?2004h\x1b[?2004$p");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b[?2004;1$y");
    h.runtime.input_mut().clear_written();

    h.inject_paste("a\x1b[20\x1b[201~1~b\x07c\x7fd\u{0085}\u{009B}e\t\n\r🚀");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b[200~abcde\t\n\r🚀\x1b[201~"
    );
    h.runtime.input_mut().clear_written();

    // MiddleClickPaste honors ?2004h sanitization when mouse tracking is off.
    h.queue_event(Event::MiddleClickPaste {
        text: "mid\x1b[201~!".to_owned(),
        pos: center,
    });
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b[200~mid!\x1b[201~");
    h.runtime.input_mut().clear_written();

    // Under ?1000h mouse tracking, MiddleClickPaste is blocked without Shift and allowed with Shift.
    h.runtime.input_mut().push_str("\x1b[?1000h");
    h.run_frame();
    h.queue_event(Event::MiddleClickPaste {
        text: "blocked".to_owned(),
        pos: center,
    });
    h.run_frame();
    assert!(h.runtime.input().written_bytes().is_empty());

    h.queue_event(Event::ModifiersChanged(Modifiers::SHIFT));
    h.queue_event(Event::MiddleClickPaste {
        text: "shifted".to_owned(),
        pos: center,
    });
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b[200~shifted\x1b[201~");
    h.queue_event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_frame();
}

#[test]
fn w1_dec_2026_deadline_repaint_palette_latch_and_decstr_recovery() {
    let cfg = TerminalConfig {
        cursor_blink: false,
        idle_poll_interval_ms: 0,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.focus_terminal();
    let default_bg = rgb_to_c32(h.config.theme.scheme.background);
    let latched_bg = Color32::from_rgb(0x22, 0x44, 0x66);

    h.runtime.input_mut().push_str("BEFORE\x1b[1;1H");
    let base = h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("BEFORE")
    );
    assert!(base.inspect_shapes().has_rect_or_mesh_color(default_bg));

    // Enter ?2026h with palette change, text on row 2, and cursor move.
    h.runtime
        .input_mut()
        .push_str("\x1b[?2026h\x1b]11;#224466\x07\x1b[2;1HAFTER\x1b[3;5H");
    h.advance_time(0.05);
    let latched = h.run_frame();
    assert!(h.runtime.terminal().is_synchronized_output());
    assert!(
        latched.repaint_requested,
        "sync_output_deadline must request a wake-up repaint while latched"
    );
    let snap_latched = h.runtime.terminal().active_snapshot();
    assert_eq!((snap_latched.cursor.col, snap_latched.cursor.row), (0, 0));
    assert_eq!(snap_latched.visible_rows[0].trimmed_len(), 6);
    assert_eq!(snap_latched.visible_rows[1].trimmed_len(), 0);
    assert_eq!(snap_latched.line_text(0).as_deref(), Some("BEFORE"));
    assert_eq!(snap_latched.line_text(1).as_deref(), Some(""));
    let latched_shapes = latched.inspect_shapes();
    assert!(latched_shapes.has_rect_or_mesh_color(default_bg));
    assert!(
        !latched_shapes.has_rect_or_mesh_color(latched_bg),
        "OSC 11 palette change must stay latched while ?2026h is active"
    );

    // At dt = 0.20s (before the 250ms deadline), active_snapshot().visible_rows still hides "AFTER".
    h.advance_time(0.20);
    let pre_deadline = h.run_frame();
    assert!(h.runtime.terminal().is_synchronized_output());
    assert!(pre_deadline.repaint_requested);
    let snap_pre = h.runtime.terminal().active_snapshot();
    assert_eq!(snap_pre.visible_rows[1].trimmed_len(), 0);
    assert_eq!(snap_pre.line_text(1).as_deref(), Some(""));
    assert!(
        !pre_deadline
            .inspect_shapes()
            .has_rect_or_mesh_color(latched_bg)
    );

    // Advance past 250ms watchdog deadline (0.20 + 0.06 = 0.26s) -> auto-unlatches and renders updated palette + cursor + text.
    h.advance_time(0.06);
    let flushed = h.run_frame();
    assert!(!h.runtime.terminal().is_synchronized_output());
    let snap_flushed = h.runtime.terminal().active_snapshot();
    assert_eq!((snap_flushed.cursor.col, snap_flushed.cursor.row), (4, 2));
    assert_eq!(snap_flushed.line_text(1).as_deref(), Some("AFTER"));
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("AFTER")
    );
    assert!(flushed.inspect_shapes().has_rect_or_mesh_color(latched_bg));

    // Re-latch ?2026h: active_snapshot().visible_rows hides "LATCHED2" prior to DECSTR, then DECSTR recovers immediately.
    h.runtime
        .input_mut()
        .push_str("\x1b[?2026h\x1b[4;1HLATCHED2");
    let relatch = h.run_frame();
    assert!(h.runtime.terminal().is_synchronized_output());
    assert!(relatch.repaint_requested);
    {
        let snap_relatch = h.runtime.terminal().active_snapshot();
        assert_eq!(snap_relatch.visible_rows[3].trimmed_len(), 0);
        assert_eq!(snap_relatch.line_text(3).as_deref(), Some(""));
    }

    h.runtime.input_mut().push_str("\x1b[!p");
    h.run_frame();
    assert!(!h.runtime.terminal().is_synchronized_output());
    assert_eq!(
        h.runtime
            .terminal()
            .active_snapshot()
            .line_text(3)
            .as_deref(),
        Some("LATCHED2")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(3).as_deref(),
        Some("LATCHED2")
    );
}

#[cfg(feature = "hyperlinks")]
#[test]
fn w1_osc8_wrapped_id_hover_spans_click_and_clear() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(320.0, 200.0);
    h.focus_terminal();
    let warm = h.last_outcome().expect("warm-up frame exists");
    let cols = warm.terminal_response.cols;
    let origin = warm.terminal_response.response.rect.min;
    let cell_size = warm.terminal_response.cell_size;
    assert!(cols >= 12);

    let first_half = "A".repeat(usize::from(cols) - 2);
    let second_half = "B".repeat(6);
    // Wraps across rows 0 and 1, preserves active_hyperlink across SGR 0, plus non-contiguous span with same id.
    // Hide cursor (?25l) so bg_mesh cursor quads cannot mask underline overlay geometry.
    let stream = format!(
        "\x1b]8;id=w1;https://example.org/wrapped\x1b\\{first_half}\x1b[0m{second_half}\x1b]8;;\x1b\\  \x1b]8;id=w1;https://example.org/wrapped\x1b\\TAG\x1b]8;;\x1b\\\x1b[?25l"
    );
    h.runtime.input_mut().push_str(stream);
    let blank_pos = h.cell_center(0, 4).expect("row 4 exists");
    h.inject_pointer_move(blank_pos);
    let base_frame = h.run_frame();
    let base_insp = base_frame.inspect_shapes();
    let base_mesh_verts = base_insp.non_text_mesh_vertex_positions().len();

    let row0_bot = origin.y + cell_size.y;
    let row1_bot = origin.y + 2.0 * cell_size.y;
    let is_row0_ul_y = |y: f32| y >= row0_bot - 4.0 && y <= row0_bot - 0.25;
    let is_row1_ul_y = |y: f32| y >= row1_bot - 4.0 && y <= row1_bot - 0.25;

    assert!(
        !base_insp
            .non_text_mesh_vertex_positions()
            .iter()
            .any(|p| is_row0_ul_y(p.y) || is_row1_ul_y(p.y)),
        "base frame without hover must emit zero underline quads on rows 0 and 1"
    );

    // Hovering row 0 highlights underline quads on row 0 (0..cols), row 1 (0..4), and row 1 "TAG" (6..9).
    let row0_pos = h.cell_center(2, 0).expect("row 0 link cell");
    h.inject_pointer_move(row0_pos);
    let hover_frame = h.run_frame();
    assert_eq!(
        hover_frame.platform_output.cursor_icon,
        CursorIcon::PointingHand
    );
    let hover_insp = hover_frame.inspect_shapes();
    let hover_verts = hover_insp.non_text_mesh_vertex_positions();
    assert_eq!(
        hover_verts.len(),
        base_mesh_verts + 12,
        "hover must add exactly 3 underline quads (12 vertices)"
    );
    let added_verts = &hover_verts[base_mesh_verts..];

    let has_row0_start = added_verts
        .iter()
        .any(|p| is_row0_ul_y(p.y) && p.x < origin.x + 0.5 * cell_size.x);
    let has_row0_end = added_verts
        .iter()
        .any(|p| is_row0_ul_y(p.y) && p.x > origin.x + (f32::from(cols) - 0.5) * cell_size.x);
    let has_row1_cont_start = added_verts
        .iter()
        .any(|p| is_row1_ul_y(p.y) && p.x < origin.x + 0.5 * cell_size.x);
    let has_row1_cont_end = added_verts.iter().any(|p| {
        is_row1_ul_y(p.y)
            && p.x >= origin.x + 3.5 * cell_size.x
            && p.x < origin.x + 4.5 * cell_size.x
    });
    let has_row1_tag_start = added_verts.iter().any(|p| {
        is_row1_ul_y(p.y)
            && p.x >= origin.x + 5.5 * cell_size.x
            && p.x < origin.x + 6.5 * cell_size.x
    });
    let has_row1_tag_end = added_verts.iter().any(|p| {
        is_row1_ul_y(p.y)
            && p.x >= origin.x + 8.5 * cell_size.x
            && p.x < origin.x + 9.5 * cell_size.x
    });
    assert!(
        has_row0_start && has_row0_end,
        "row 0 link segment (0..{cols}) must be underlined; added={added_verts:?}"
    );
    assert!(
        has_row1_cont_start && has_row1_cont_end,
        "wrapped row 1 segment (0..4) with shared id= must be underlined; added={added_verts:?}"
    );
    assert!(
        has_row1_tag_start && has_row1_tag_end,
        "non-contiguous row 1 \"TAG\" segment (6..9) with shared id= must be underlined; added={added_verts:?}"
    );

    // Clicking the wrapped continuation on row 1 activates the link with its id.
    let row1_pos = h.cell_center(1, 1).expect("row 1 continuation cell");
    h.advance_time(1.0);
    h.inject_click(row1_pos, PointerButton::Primary, Modifiers::NONE);
    let click_frame = h.last_outcome().expect("click frame captured");
    assert_eq!(
        click_frame.opened_urls(),
        vec!["https://example.org/wrapped".to_owned()]
    );
    assert!(
        click_frame
            .terminal_response
            .events
            .iter()
            .any(|ev| matches!(
                ev,
                TerminalEvent::HyperlinkActivated { uri, id }
                    if uri == "https://example.org/wrapped" && id.as_deref() == Some("w1")
            ))
    );

    // Moving pointer off link to blank cell clears underline quads and CursorIcon::PointingHand.
    h.inject_pointer_move(blank_pos);
    let clear_frame = h.run_frame();
    assert_ne!(
        clear_frame.platform_output.cursor_icon,
        CursorIcon::PointingHand
    );
    assert_eq!(
        clear_frame
            .inspect_shapes()
            .non_text_mesh_vertex_positions()
            .len(),
        base_mesh_verts
    );
}

#[test]
fn w1_alt_drag_rectangular_block_selection_copy_and_shift_bypass() {
    let mut h = HeadlessTermHarness::scripted();
    h.focus_terminal();
    h.runtime
        .input_mut()
        .push_str("ABCD1234\r\nEFGH5678\r\nIJKL9012\r\n甲乙丙丁\r\n");
    h.run_frame();

    // Forward Alt+drag rectangular selection (cols 1..=3, rows 0..=2).
    let p_start = h.cell_center(1, 0).expect("start cell");
    let p_end = h.cell_center(3, 2).expect("end cell");
    h.advance_time(0.5);
    h.inject_drag_steps(p_start, p_end, PointerButton::Primary, Modifiers::ALT, 4);
    let settled = h
        .last_outcome()
        .expect("drag release outcome")
        .settled_selections();
    assert_eq!(settled, vec!["BCD\nFGH\nJKL".to_owned()]);
    assert_eq!(
        h.runtime.terminal().selection_text().as_deref(),
        Some("BCD\nFGH\nJKL")
    );
    h.inject_copy();
    let copy_frame = h.run_frame();
    assert_eq!(copy_frame.copied_texts(), vec!["BCD\nFGH\nJKL".to_owned()]);

    // Reverse Alt+drag (bottom-right -> top-left: cols 3..=5, rows 0..=2).
    let rev_from = h.cell_center(5, 2).expect("rev start");
    let rev_to = h.cell_center(3, 0).expect("rev end");
    h.advance_time(0.5);
    h.inject_drag_steps(rev_from, rev_to, PointerButton::Primary, Modifiers::ALT, 4);
    assert_eq!(
        h.runtime.terminal().selection_text().as_deref(),
        Some("D12\nH56\nL90")
    );

    // Wide CJK boundary normalization: drag from spacer col 1 (甲) to lead col 4 (丙) expands cleanly.
    let cjk_from = h.cell_center(1, 3).expect("cjk spacer col");
    let cjk_to = h.cell_center(4, 3).expect("cjk lead col");
    h.advance_time(0.5);
    h.inject_drag_steps(cjk_from, cjk_to, PointerButton::Primary, Modifiers::ALT, 3);
    assert_eq!(
        h.runtime.terminal().selection_text().as_deref(),
        Some("甲乙丙")
    );

    // Under ?1000h mouse tracking, Shift+Alt+drag bypasses mouse reporting and selects block.
    h.runtime.input_mut().push_str("\x1b[?1000h");
    h.run_frame();
    h.advance_time(0.5);
    h.inject_drag_steps(
        p_start,
        p_end,
        PointerButton::Primary,
        Modifiers::SHIFT | Modifiers::ALT,
        3,
    );
    assert!(
        h.runtime.input().written_bytes().is_empty(),
        "Shift+Alt+drag must bypass mouse tracking"
    );
    assert_eq!(
        h.runtime.terminal().selection_text().as_deref(),
        Some("BCD\nFGH\nJKL")
    );
}

#[test]
fn w1_mouse_1016_sgr_pixels_and_double_triple_click_selection_e2e() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_pixels_per_point(2.0);
    h.focus_terminal();
    let origin = h
        .last_outcome()
        .expect("warm frame")
        .terminal_response
        .response
        .rect
        .min;

    // Enable button-event (?1002h) + SGR-Pixels (?1016h).
    h.runtime.input_mut().push_str("\x1b[?1002h\x1b[?1016h");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().modes().mouse_protocol,
        MouseProtocol::ButtonEvent
    );
    assert_eq!(
        h.runtime.terminal().modes().mouse_encoding,
        MouseEncoding::SgrPixels
    );

    let p1 = h.cell_center(4, 2).expect("p1");
    let p2 = h.cell_center(7, 3).expect("p2");
    let to_px = |p: Pos2| -> (u32, u32) {
        (
            ((p.x - origin.x).max(0.0) * 2.0).round() as u32,
            ((p.y - origin.y).max(0.0) * 2.0).round() as u32,
        )
    };
    let (px1, py1) = to_px(p1);
    let (px2, py2) = to_px(p2);

    h.queue_event(Event::ModifiersChanged(Modifiers::CTRL));
    h.inject_pointer_move(p1);
    h.inject_pointer_button(p1, PointerButton::Primary, true, Modifiers::CTRL);
    h.run_frame();
    h.inject_pointer_move(p2);
    h.run_frame();
    h.inject_pointer_button(p2, PointerButton::Primary, false, Modifiers::CTRL);
    h.run_frame();
    h.queue_event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_frame();

    assert_eq!(
        h.runtime.input().written_str(),
        format!("\x1b[<16;{px1};{py1}M\x1b[<48;{px2};{py2}M\x1b[<16;{px2};{py2}m")
    );
    h.runtime.input_mut().clear_written();

    // Disable mouse tracking and test double-click word + triple-click line selection via UI.
    h.runtime
        .input_mut()
        .push_str("\x1b[?1002l\x1b[?1016l\x1b[2J\x1b[1;1Halpha beta gamma\r\nsecond line");
    h.run_frame();

    let word_pos = h.cell_center(7, 0).expect("on 'beta'");
    h.advance_time(0.5);
    h.inject_click(word_pos, PointerButton::Primary, Modifiers::NONE);
    h.advance_time(0.05);
    h.inject_click(word_pos, PointerButton::Primary, Modifiers::NONE);
    assert_eq!(
        h.runtime.terminal().selection_text().as_deref(),
        Some("beta")
    );

    h.advance_time(0.5);
    h.inject_click(word_pos, PointerButton::Primary, Modifiers::NONE);
    h.advance_time(0.05);
    h.inject_click(word_pos, PointerButton::Primary, Modifiers::NONE);
    h.advance_time(0.05);
    h.inject_click(word_pos, PointerButton::Primary, Modifiers::NONE);
    assert_eq!(
        h.runtime.terminal().selection_text().as_deref(),
        Some("alpha beta gamma")
    );
    h.inject_copy();
    let copied = h.run_frame().copied_texts();
    assert_eq!(copied, vec!["alpha beta gamma".to_owned()]);
}

#[test]
fn w1_da1_da2_xtversion_nonzero_param_handling_and_decscusr_e2e() {
    let cfg = TerminalConfig {
        cursor_blink: false,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.focus_terminal();

    // DA1 (0/omitted vs 1), DA2 (>0/omitted vs >1), XTVERSION (>0/omitted vs >1).
    h.runtime
        .input_mut()
        .push_str("\x1b[c\x1b[0c\x1b[1c\x1b[>c\x1b[>0c\x1b[>1c\x1b[>q\x1b[>0q\x1b[>1q");
    h.run_frame();
    let xtver = format!("\x1bP>|egui_xterm {}\x1b\\", env!("CARGO_PKG_VERSION"));
    assert_eq!(
        h.runtime.input().written_str(),
        format!("\x1b[?62;4;22c\x1b[?62;4;22c\x1b[>0;100;0c\x1b[>0;100;0c{xtver}{xtver}")
    );

    // DECSCUSR 1..=6 + 0 updates CursorStyleChanged events, cursor state, and rendered cursor overlay.
    let origin = h
        .last_outcome()
        .expect("warm")
        .terminal_response
        .response
        .rect
        .min;
    let cell_size = h.last_outcome().expect("warm").terminal_response.cell_size;
    let cursor_c32 = rgb_to_c32(h.config.theme.scheme.cursor);

    let cell_zero_rect = egui::Rect::from_min_size(origin, cell_size);
    let cursor_rects_in_cell0 = |insp: &egui_xterm_e2e::ShapeInspection| -> Vec<egui::Rect> {
        insp.rect_fills_in_rect(cell_zero_rect, cursor_c32)
            .into_iter()
            .map(|(rect, _)| rect)
            .collect()
    };

    for (code, expected_shape, expected_blink) in [
        (2, CursorShape::Block, false),
        (3, CursorShape::Underline, true),
        (4, CursorShape::Underline, false),
        (5, CursorShape::Bar, true),
        (6, CursorShape::Bar, false),
        (1, CursorShape::Block, true),
        (0, CursorShape::Block, false),
    ] {
        h.runtime.input_mut().push_str(format!("\x1b[{code} q"));
        let frame = h.run_frame();
        assert_eq!(h.runtime.terminal().cursor_shape(), expected_shape);
        assert_eq!(h.runtime.terminal().cursor_blink(), expected_blink);
        assert!(frame.terminal_response.events.iter().any(|ev| matches!(
            ev,
            TerminalEvent::CursorStyleChanged(style)
                if style.shape == expected_shape && style.blink == expected_blink
        )));
        if matches!(expected_shape, CursorShape::Underline | CursorShape::Bar) && !expected_blink {
            let insp = frame.inspect_shapes();
            assert!(insp.has_rect_or_mesh_color(cursor_c32));
            let rects = cursor_rects_in_cell0(&insp);
            assert_eq!(
                rects.len(),
                1,
                "DECSCUSR {code} must render 1 cursor overlay rect in cell (0,0)"
            );
            let r = rects[0];
            match expected_shape {
                CursorShape::Underline => {
                    assert!(
                        r.min.y > origin.y + cell_size.y * 0.5
                            && r.max.y <= origin.y + cell_size.y + 0.5
                            && r.min.x <= origin.x + 0.5
                            && r.max.x >= origin.x + cell_size.x * 0.5
                            && r.height() < cell_size.y * 0.5,
                        "DECSCUSR {code} (Underline) must render bottom strip across cell width, not left bar; rect={r:?}"
                    );
                }
                CursorShape::Bar => {
                    assert!(
                        r.max.x < origin.x + cell_size.x * 0.5
                            && r.min.x >= origin.x - 0.5
                            && r.min.y <= origin.y + 0.5
                            && r.max.y >= origin.y + cell_size.y * 0.5
                            && r.width() < cell_size.x * 0.5,
                        "DECSCUSR {code} (Bar) must render left strip across cell height, not bottom underline; rect={r:?}"
                    );
                }
                CursorShape::Block => {}
            }
        }
    }

    // Confirm cursor-geometry checks are non-vacuous: hiding cursor (?25l) removes all cursor rects.
    h.runtime.input_mut().push_str("\x1b[4 q\x1b[?25l");
    let hidden_frame = h.run_frame();
    let hidden_insp = hidden_frame.inspect_shapes();
    assert!(
        cursor_rects_in_cell0(&hidden_insp).is_empty(),
        "hiding cursor with ?25l must emit zero cursor-colored rects in cell (0,0)"
    );
}

#[test]
fn w1_sgr_58_59_underline_rgb_and_vs15_vs16_e2e() {
    let mut h = HeadlessTermHarness::scripted();
    let warm = h.run_frame();
    let cols = warm.terminal_response.cols;

    // SGR 58:2::255:0:128 across all 5 underline styles (4:1..4:5).
    h.runtime
        .input_mut()
        .push_str("\x1b[58:2::255:0:128m\x1b[4:1mA\x1b[4:2mB\x1b[4:3mC\x1b[4:4mD\x1b[4:5mE\x1b[0m");
    let ul_rgb_frame = h.run_frame();
    let ul_insp = ul_rgb_frame.inspect_shapes();
    assert!(
        ul_insp.has_rect_or_mesh_color(Color32::from_rgb(255, 0, 128)),
        "SGR 58 RGB underline color must render into overlay mesh"
    );
    assert!(
        ul_insp.path_count >= 1,
        "SGR 4:3 curly underline must emit PathShape"
    );

    // SGR 58;5;4 tracks OSC 4 palette override; SGR 59 reverts underline color to fg.
    h.runtime
        .input_mut()
        .push_str("\x1b]4;4;#00dd88\x07\x1b[58;5;4;4mF\x1b[59;38;2;12;200;45mG\x1b[0m");
    let ul_pal_frame = h.run_frame();
    let pal_insp = ul_pal_frame.inspect_shapes();
    assert!(pal_insp.has_rect_or_mesh_color(Color32::from_rgb(0x00, 0xdd, 0x88)));
    assert!(pal_insp.has_rect_or_mesh_color(Color32::from_rgb(12, 200, 45)));

    // DEC ?2027 DECRQM toggle + VS16 widening (including right-margin wrap) + VS15 narrowing + ZWJ.
    h.runtime
        .input_mut()
        .push_str("\x1b[?2027$p\x1b[?2027l\x1b[?2027$p\x1b[?2027h\x1b[?2027$p");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b[?2027;1$y\x1b[?2027;2$y\x1b[?2027;1$y"
    );

    h.runtime.input_mut().push_str(format!(
        "\x1b[2J\x1b[1;{cols}H\u{2764}\u{FE0F}\x1b[3;1H\u{231B}\u{FE0E}X\x1b[4;1H\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}Y"
    ));
    h.run_frame();
    let term = h.runtime.terminal();
    let wrapped_lead = term.cell_at(0, 1).expect("wrapped VS16 lead");
    let wrapped_spacer = term.cell_at(1, 1).expect("wrapped VS16 spacer");
    assert!(wrapped_lead.grapheme.contains('\u{FE0F}'));
    assert!(wrapped_spacer.is_wide_spacer);

    let demoted = term.cell_at(0, 2).expect("VS15 cell");
    let after_vs15 = term.cell_at(1, 2).expect("cell after VS15");
    assert!(demoted.grapheme.contains('\u{FE0E}'));
    assert_eq!(after_vs15.ch, 'X');

    let zwj_spacer = term.cell_at(1, 3).expect("ZWJ spacer");
    let after_zwj = term.cell_at(2, 3).expect("cell after ZWJ");
    assert!(zwj_spacer.is_wide_spacer);
    assert_eq!(after_zwj.ch, 'Y');
}

#[test]
fn w1_xtwinops_22_23_selective_title_icon_stack_and_cap() {
    let mut h = HeadlessTermHarness::scripted();
    h.runtime
        .input_mut()
        .push_str("\x1b]1;Icon0\x07\x1b]2;Title0\x07");
    h.run_frame();
    assert_eq!(h.runtime.terminal().icon_name(), "Icon0");
    assert_eq!(h.runtime.terminal().title(), "Title0");

    // 22;1t pushes only icon; 23;1t restores only icon without touching title.
    h.runtime
        .input_mut()
        .push_str("\x1b[22;1t\x1b]1;Icon1\x07\x1b]2;Title1\x07");
    h.run_frame();
    h.runtime.input_mut().push_str("\x1b[23;1t");
    let pop_icon = h.run_frame();
    assert_eq!(h.runtime.terminal().icon_name(), "Icon0");
    assert_eq!(h.runtime.terminal().title(), "Title1");
    assert!(
        pop_icon
            .terminal_response
            .events
            .contains(&TerminalEvent::IconNameChanged("Icon0".to_owned()))
    );
    assert!(
        !pop_icon
            .terminal_response
            .events
            .iter()
            .any(|ev| matches!(ev, TerminalEvent::TitleChanged(_)))
    );

    // 22;2t pushes only title; 23;2t restores only title without touching icon.
    h.runtime
        .input_mut()
        .push_str("\x1b[22;2t\x1b]1;Icon2\x07\x1b]2;Title2\x07");
    h.run_frame();
    h.runtime.input_mut().push_str("\x1b[23;2t");
    let pop_title = h.run_frame();
    assert_eq!(h.runtime.terminal().title(), "Title1");
    assert_eq!(h.runtime.terminal().icon_name(), "Icon2");
    assert!(
        pop_title
            .terminal_response
            .events
            .contains(&TerminalEvent::TitleChanged("Title1".to_owned()))
    );
    assert!(
        !pop_title
            .terminal_response
            .events
            .iter()
            .any(|ev| matches!(ev, TerminalEvent::IconNameChanged(_)))
    );

    // Popping identical title emits no redundant TitleChanged event.
    h.runtime.input_mut().push_str("\x1b[22;2t\x1b[23;2t");
    let noop_pop = h.run_frame();
    assert!(
        !noop_pop
            .terminal_response
            .events
            .iter()
            .any(|ev| matches!(ev, TerminalEvent::TitleChanged(_)))
    );

    // Stack cap = 16: pushing 20 frames evicts oldest 4 (T0..T3), keeping T4..T19.
    let mut push_seq = String::new();
    for i in 0..20 {
        push_seq.push_str(&format!("\x1b]2;T{i}\x07\x1b[22;2t"));
    }
    push_seq.push_str("\x1b]2;T20\x07");
    h.runtime.input_mut().push_str(push_seq);
    h.run_frame();
    assert_eq!(h.runtime.terminal().title(), "T20");

    for expected in (4..20).rev() {
        h.runtime.input_mut().push_str("\x1b[23;2t");
        h.run_frame();
        assert_eq!(h.runtime.terminal().title(), format!("T{expected}"));
    }

    // Popping an empty stack is a safe no-op.
    h.runtime.input_mut().push_str("\x1b[23;2t\x1b[23;0t");
    let empty_pop = h.run_frame();
    assert_eq!(h.runtime.terminal().title(), "T4");
    assert!(empty_pop.terminal_response.events.is_empty());
}

#[test]
fn w1_irm_resets_editing_ops_and_powerline_vector_glyphs_e2e() {
    let mut h = HeadlessTermHarness::scripted();
    h.focus_terminal();

    // 1. IRM (CSI 4 h / CSI 4 l) + ANSI DECRQM (CSI 4 $ p).
    h.runtime
        .input_mut()
        .push_str("\x1b[4$pABCDE\x1b[1;3H\x1b[4h\x1b[4$pXY\x1b[4l\x1b[4$pZ");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b[4;2$y\x1b[4;1$y\x1b[4;2$y"
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("ABXYZDE")
    );
    h.runtime.input_mut().clear_written();

    // 2. Editing ops (ICH, DCH, ECH, IL, DL, EL 0..2, ED 0..2) + BCE.
    h.runtime.input_mut().push_str(concat!(
        "\x1b[2J\x1b[1;1H0123456789\r\nABCDEFGHIJ\r\nKLMNOPQRST\r\n",
        "\x1b[48;2;30;60;90m",
        "\x1b[1;3H\x1b[2@",
        "\x1b[1;8H\x1b[2P",
        "\x1b[2;3H\x1b[2X",
        "\x1b[3;1H\x1b[0K",
    ));
    h.run_frame();
    {
        let term = h.runtime.terminal();
        let bce = PackedColor::Rgb(30, 60, 90);
        // Inserted blanks at row 0 cols 2..4 have BCE bg.
        assert_eq!(term.cell_at(2, 0).expect("ich cell").bg, bce);
        assert_eq!(term.cell_at(3, 0).expect("ich cell").bg, bce);
        // ECH at row 1 cols 2..4 erased 'C','D' with BCE bg.
        assert_eq!(term.cell_at(2, 1).expect("ech cell").ch, ' ');
        assert_eq!(term.cell_at(2, 1).expect("ech cell").bg, bce);
        assert_eq!(term.cell_at(4, 1).expect("kept cell").ch, 'E');
        // EL 0 cleared row 2 from col 0 with BCE bg.
        assert_eq!(term.cell_at(0, 2).expect("el0 cell").bg, bce);
    }

    // EL 1, EL 2, then IL (separate frame) and DL (separate frame).
    h.runtime.input_mut().push_str(concat!(
        "\x1b[0m\x1b[2J",
        "\x1b[1;1HROW0\r\nROW1\r\nROW2\r\n",
        "\x1b[2;3H\x1b[1K",
        "\x1b[3;2H\x1b[2K",
        "\x1b[1;1H\x1b[L",
    ));
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("ROW0")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(2).as_deref(),
        Some("   1")
    );

    h.runtime.input_mut().push_str("\x1b[1;1H\x1b[M");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("ROW0")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("   1")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(2).as_deref(),
        Some("")
    );

    h.runtime
        .input_mut()
        .push_str("\x1b[2J\x1b[1;1HTOP\r\nMID\r\nBOT\x1b[2;2H\x1b[0J");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("TOP")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("M")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(2).as_deref(),
        Some("")
    );

    h.runtime
        .input_mut()
        .push_str("\x1b[1;1HTOP\r\nMID\x1b[2;2H\x1b[1J");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("  D")
    );

    // 3. DECSTR (CSI ! p) resets modes/margins/SGR/cursor style while preserving text; RIS (ESC c) wipes screen.
    h.runtime
        .input_mut()
        .push_str("\x1b[1;1HKEEP\x1b[4h\x1b[?69h\x1b[2;5s\x1b[4 q\x1b[!p\x1b[4$p\x1b[?69$p");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b[4;2$y\x1b[?69;2$y");
    assert_eq!(h.runtime.terminal().cursor_shape(), CursorShape::Block);
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("KEEP")
    );

    h.runtime.input_mut().push_str("\x1bc");
    h.run_frame();
    assert_eq!(h.runtime.terminal().cursor_pos(), (0, 0));
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("")
    );

    // 4. Powerline vector glyphs (U+E0B0..=U+E0D4) render via vector mesh rather than Shape::Text.
    #[cfg(feature = "vector-glyphs")]
    {
        h.runtime
            .input_mut()
            .push_str("\x1b[38;2;220;120;40m\u{E0B0}\u{E0B1}\u{E0B2}\u{E0B3}\u{E0B4}\u{E0B6}\u{E0B8}\u{E0BA}\x1b[0m");
        let pl_frame = h.run_frame();
        let pl_insp = pl_frame.inspect_shapes();
        assert!(
            pl_insp.has_rect_or_mesh_color(Color32::from_rgb(220, 120, 40)),
            "Powerline glyphs must tessellate into vector mesh with active fg color"
        );
        assert!(
            pl_insp.text_entries.is_empty() && pl_insp.text_mesh_vertices().is_empty(),
            "Powerline glyphs must bypass Shape::Text and font-atlas text_mesh"
        );
    }
}

#[test]
fn w1_osc22_pointer_cursor_icon_integration_e2e() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(400.0, 240.0);
    h.focus_terminal();
    let hover_pos = h.cell_center(2, 2).expect("hover cell inside grid");

    h.inject_pointer_move(hover_pos);
    let base_frame = h.run_frame();
    assert_eq!(base_frame.platform_output.cursor_icon, CursorIcon::Default);

    // OSC 22;crosshair -> TerminalEvent::MouseShape("crosshair") + CursorIcon::Crosshair on hover.
    h.runtime.input_mut().push_str("\x1b]22;crosshair\x07");
    let crosshair_frame = h.run_frame();
    assert!(
        crosshair_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::MouseShape("crosshair".to_owned()))
    );
    assert_eq!(
        crosshair_frame.platform_output.cursor_icon,
        CursorIcon::Crosshair
    );

    // OSC 22;pointer -> CursorIcon::PointingHand.
    h.runtime.input_mut().push_str("\x1b]22;pointer\x07");
    let pointer_frame = h.run_frame();
    assert!(
        pointer_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::MouseShape("pointer".to_owned()))
    );
    assert_eq!(
        pointer_frame.platform_output.cursor_icon,
        CursorIcon::PointingHand
    );

    // OSC 22;text -> CursorIcon::Text.
    h.runtime.input_mut().push_str("\x1b]22;text\x07");
    let text_frame = h.run_frame();
    assert!(
        text_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::MouseShape("text".to_owned()))
    );
    assert_eq!(text_frame.platform_output.cursor_icon, CursorIcon::Text);

    // OSC 22;<empty> resets custom pointer shape back to baseline default.
    h.runtime.input_mut().push_str("\x1b]22;\x07");
    let reset_frame = h.run_frame();
    assert!(
        reset_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::MouseShape(String::new()))
    );
    assert_eq!(reset_frame.platform_output.cursor_icon, CursorIcon::Default);

    // Hyperlink hover takes precedence over OSC 22 shape and restores it when leaving the link.
    #[cfg(feature = "hyperlinks")]
    {
        h.runtime.input_mut().push_str(
            "\x1b]22;crosshair\x07\x1b[1;1H\x1b]8;;https://example.org\x1b\\LINK\x1b]8;;\x1b\\",
        );
        h.run_frame();

        let link_pos = h.cell_center(1, 0).expect("link cell");
        h.inject_pointer_move(link_pos);
        let over_link = h.run_frame();
        assert_eq!(
            over_link.platform_output.cursor_icon,
            CursorIcon::PointingHand
        );

        h.inject_pointer_move(hover_pos);
        let off_link = h.run_frame();
        assert_eq!(off_link.platform_output.cursor_icon, CursorIcon::Crosshair);
    }
}
