//! Wave 2 opaque-box E2E tests for VT/CSI/ESC sequences (Item #3) and
//! modern OSC / input / security protocols (Item #4).

#![forbid(unsafe_code)]

use egui::{Color32, CursorIcon, Modifiers, MouseWheelUnit, PointerButton, Pos2, Vec2};
use egui_xterm::{TerminalConfig, TerminalEvent};
use egui_xterm_core::{
    CellFlags, ClipboardSelection, CoreConfig, MouseButton, MouseEncoding, MouseEventKind,
    MouseInputEvent, MouseModifiers, MouseProtocol, Osc52ReadPolicy, PackedColor, SelectionKind,
    Terminal, TextSizing, UnderlineStyle,
};
#[cfg(feature = "kitty-keyboard")]
use egui_xterm_core::{KittyEventType, KittyKey, KittyKeyEvent, KittyModifiers, ModifyOtherKeys};
use egui_xterm_e2e::HeadlessTermHarness;

fn rgb_to_c32(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

// ===========================================================================
// Item #3 — VT / CSI / ESC Sequences: Tier 1 (8 Equivalence-Class Tests)
// ===========================================================================

#[test]
fn t1_item3_01_sl_sr_full_page_and_bce() {
    let mut h = HeadlessTermHarness::scripted();
    h.focus_terminal();
    let warm = h.run_frame();
    let cols = usize::from(warm.terminal_response.cols);
    assert!(cols >= 20);

    // Populate two lines, set BCE bg, and shift left by 2 (`CSI 2 SP @`).
    h.runtime
        .input_mut()
        .push_str("ABCDEFGH\r\n12345678\x1b[48;2;10;20;30m\x1b[2;4H\x1b[2 @");
    let sl_frame = h.run_frame();
    let sl_row0 = h
        .runtime
        .terminal()
        .visible_line_text(0)
        .expect("row 0 exists");
    let sl_row1 = h
        .runtime
        .terminal()
        .visible_line_text(1)
        .expect("row 1 exists");
    assert_eq!(sl_row0.trim_end(), "CDEFGH");
    assert_eq!(sl_row1.trim_end(), "345678");
    // Cursor remains unchanged at 0-based (col=3, row=1).
    assert_eq!(h.runtime.terminal().cursor_pos(), (3, 1));
    let bce_sl = PackedColor::Rgb(10, 20, 30);
    for row in 0..=1u16 {
        for col in (cols - 2)..cols {
            let cell = h
                .runtime
                .terminal()
                .cell_at(col as u16, row)
                .expect("shifted right-edge cell");
            assert_eq!(cell.ch, ' ');
            assert_eq!(cell.bg, bce_sl);
        }
    }
    let sl_shapes = sl_frame.inspect_shapes();
    assert!(!sl_shapes.text_mesh_vertices().is_empty());
    assert!(sl_shapes.has_rect_or_mesh_color(Color32::from_rgb(10, 20, 30)));

    // Set a distinct BCE bg and shift right by 3 (`CSI 3 SP A`).
    h.runtime
        .input_mut()
        .push_str("\x1b[48;2;40;50;60m\x1b[3 A");
    let sr_frame = h.run_frame();
    assert_eq!(h.runtime.terminal().cursor_pos(), (3, 1));
    let line0 = h
        .runtime
        .terminal()
        .visible_line_text(0)
        .expect("row 0 exists");
    let line1 = h
        .runtime
        .terminal()
        .visible_line_text(1)
        .expect("row 1 exists");
    assert_eq!(line0.trim_end(), "   CDEFGH");
    assert_eq!(line1.trim_end(), "   345678");
    let bce_sr = PackedColor::Rgb(40, 50, 60);
    for row in 0..=1u16 {
        for col in 0..3u16 {
            let cell = h
                .runtime
                .terminal()
                .cell_at(col, row)
                .expect("shifted left-edge cell");
            assert_eq!(cell.ch, ' ');
            assert_eq!(cell.bg, bce_sr);
        }
    }
    assert!(
        sr_frame
            .inspect_shapes()
            .has_rect_or_mesh_color(Color32::from_rgb(40, 50, 60))
    );
}

#[test]
fn t1_item3_02_decsca_decsed_decsel_and_decrqss() {
    let mut h = HeadlessTermHarness::scripted();
    h.focus_terminal();
    let warm = h.run_frame();
    let rows = warm.terminal_response.rows;
    assert!(rows >= 4);

    // Seed scrollback so DECSED 3 can also be verified alongside 0/1/2.
    let mut seed = String::new();
    seed.push_str("SB_UNPROT\x1b[1\"qSB_PROT\x1b[0\"q\r\n");
    for _ in 0..rows {
        seed.push_str("\r\n");
    }
    h.runtime.input_mut().push_str(seed);
    h.run_frame();

    // DECSCA 1 persists across SGR 0; DECRQSS `"q` tracks active state dynamically.
    h.runtime.input_mut().push_str(concat!(
        "\x1b[2J\x1b[1;1H",
        "A\x1b[1\"q\x1b[31mB\x1b[0mC\x1bP$q\"q\x1b\\\x1b[0\"qD\x1bP$q\"q\x1b\\",
        "\r\nX\x1b[1\"qY\x1b[2\"qZ",
    ));
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1bP1$r1\"q\x1b\\\x1bP1$r0\"q\x1b\\"
    );
    h.runtime.input_mut().clear_written();

    let term = h.runtime.terminal();
    assert!(
        !term
            .cell_at(0, 0)
            .expect("A")
            .flags
            .contains(CellFlags::PROTECTED)
    );
    assert!(
        term.cell_at(1, 0)
            .expect("B")
            .flags
            .contains(CellFlags::PROTECTED)
    );
    assert!(
        term.cell_at(2, 0)
            .expect("C")
            .flags
            .contains(CellFlags::PROTECTED)
    );
    assert!(
        !term
            .cell_at(3, 0)
            .expect("D")
            .flags
            .contains(CellFlags::PROTECTED)
    );
    assert!(
        term.cell_at(1, 1)
            .expect("Y")
            .flags
            .contains(CellFlags::PROTECTED)
    );

    // DECSEL 1 (`CSI ? 1 K`) at (row 0, col 3 -> 1-based 1;4) erases unprotected A and D, keeps B and C.
    h.runtime.input_mut().push_str("\x1b[1;4H\x1b[?1K");
    h.run_frame();
    let term = h.runtime.terminal();
    assert_eq!(term.cell_at(0, 0).expect("A erased").ch, ' ');
    assert_eq!(term.cell_at(1, 0).expect("B kept").ch, 'B');
    assert_eq!(term.cell_at(2, 0).expect("C kept").ch, 'C');
    assert_eq!(term.cell_at(3, 0).expect("D erased").ch, ' ');

    // DECSED 0 (`CSI ? 0 J`) from (1,1) erases X and Z on row 1, preserving B, C, and Y.
    h.runtime.input_mut().push_str("\x1b[1;1H\x1b[?0J");
    h.run_frame();
    let term = h.runtime.terminal();
    assert_eq!(term.cell_at(1, 0).expect("B").ch, 'B');
    assert_eq!(term.cell_at(2, 0).expect("C").ch, 'C');
    assert_eq!(term.cell_at(0, 1).expect("X erased").ch, ' ');
    assert_eq!(term.cell_at(1, 1).expect("Y kept").ch, 'Y');
    assert_eq!(term.cell_at(2, 1).expect("Z erased").ch, ' ');

    // DECSED 3 (`CSI ? 3 J`) selectively erases scrollback unprotected text while keeping protected SB_PROT.
    h.runtime.input_mut().push_str("\x1b[?3J");
    h.run_frame();
    let sb = h
        .runtime
        .terminal()
        .scrollback_line_text(0)
        .expect("scrollback line 0 exists");
    assert!(!sb.contains("SB_UNPROT"));
    assert!(sb.contains("SB_PROT"));

    // Standard ED 2 (`CSI 2 J`) erases protected cells as well.
    h.runtime.input_mut().push_str("\x1b[2J");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("")
    );
}

#[test]
fn t1_item3_03_rect_ops_decfra_decera_decsera_deccra() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(120.0, 140.0);
    h.run_frame();

    // DECFRA (`CSI 35;2;3;4;6 $ x`) fills rows 2..=4 (0-based 1..=3), cols 3..=6 (0-based 2..=5)
    // with '#' + green fg + blue bg + DECSCA PROTECTED.
    h.runtime
        .input_mut()
        .push_str("\x1b[1\"q\x1b[32;44m\x1b[35;2;3;4;6$x\x1b[0\"q\x1b[0m");
    let fill_frame = h.run_frame();
    {
        let term = h.runtime.terminal();
        for row in 1..=3u16 {
            for col in 2..=5u16 {
                let c = term.cell_at(col, row).expect("filled rect cell");
                assert_eq!(c.ch, '#');
                assert_eq!(c.fg, PackedColor::Indexed(2));
                assert_eq!(c.bg, PackedColor::Indexed(4));
                assert!(c.flags.contains(CellFlags::PROTECTED));
            }
        }
        assert_eq!(term.cell_at(1, 1).expect("outside left").ch, ' ');
    }
    let fill_insp = fill_frame.inspect_shapes();
    assert!(!fill_insp.text_mesh_vertices().is_empty());
    assert!(fill_insp.has_rect_or_mesh_color(rgb_to_c32(h.config.theme.scheme.ansi[4])));

    // Overwrite 1-based row 3 (0-based row 2) with unprotected "UNPROT" across cols 1..=6.
    // Then DECSERA (`CSI 2;3;4;6 $ {`) preserves protected '#' on rows 1 & 3, erases PROT in box.
    h.runtime
        .input_mut()
        .push_str("\x1b[3;1HUNPROT\x1b[2;3;4;6${");
    h.run_frame();
    {
        let term = h.runtime.terminal();
        assert_eq!(term.visible_line_text(1).as_deref(), Some("  ####"));
        assert_eq!(term.visible_line_text(2).as_deref(), Some("UN"));
        assert_eq!(term.visible_line_text(3).as_deref(), Some("  ####"));
    }

    // DECCRA copies source box (2..=4, 3..=6) to dst (row=3, col=7) -> 0-based (rows 2..=4, cols 6..=9).
    h.runtime.input_mut().push_str("\x1b[2;3;4;6;1;3;7;1$v");
    h.run_frame();
    {
        let term = h.runtime.terminal();
        for col in 6..=9u16 {
            let c_top = term.cell_at(col, 2).expect("copied top row");
            let c_bot = term.cell_at(col, 4).expect("copied bottom row");
            assert_eq!(c_top.ch, '#');
            assert_eq!(c_top.fg, PackedColor::Indexed(2));
            assert!(c_top.flags.contains(CellFlags::PROTECTED));
            assert_eq!(c_bot.ch, '#');
            assert!(c_bot.flags.contains(CellFlags::PROTECTED));
        }
        assert_eq!(term.cell_at(6, 3).expect("copied middle blank").ch, ' ');
    }

    // DECERA (`CSI 2;3;4;6 $ z`) unconditionally erases source box including protected '#'.
    h.runtime.input_mut().push_str("\x1b[2;3;4;6$z");
    h.run_frame();
    {
        let term = h.runtime.terminal();
        for row in 1..=3u16 {
            for col in 2..=5u16 {
                assert_eq!(term.cell_at(col, row).expect("erased cell").ch, ' ');
            }
        }
        // Copied destination remains intact outside erased source.
        assert_eq!(term.cell_at(6, 2).expect("dst preserved").ch, '#');
    }
}

#[test]
fn t1_item3_04_xtpushsgr_xtpopsgr_and_decrqss_m() {
    let mut h = HeadlessTermHarness::scripted();
    h.focus_terminal();

    // Full XTPUSHSGR (`CSI # {`) / XTPOPSGR (`CSI # }`) with RGB fg, curly underline, and SGR 58 color.
    h.runtime.input_mut().push_str(
        "\x1b[1;3;4:3;38;2;200;10;20;58;2;1;2;3m\x1b[#{\x1b[0;32mG\x1b[#}R\x1bP$qm\x1b\\",
    );
    let frame = h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1bP1$r0;1;3;4:3;38;2;200;10;20;58;2;1;2;3m\x1b\\"
    );
    h.runtime.input_mut().clear_written();

    let term = h.runtime.terminal();
    let g = term.cell_at(0, 0).expect("G cell");
    assert_eq!(g.ch, 'G');
    assert_eq!(g.fg, PackedColor::Indexed(2));
    assert!(!g.flags.contains(CellFlags::BOLD));
    assert_eq!(g.underline_style, UnderlineStyle::None);

    let r = term.cell_at(1, 0).expect("R cell");
    assert_eq!(r.ch, 'R');
    assert_eq!(r.fg, PackedColor::Rgb(200, 10, 20));
    assert_eq!(r.underline_color, Some(PackedColor::Rgb(1, 2, 3)));
    assert_eq!(r.underline_style, UnderlineStyle::Curly);
    assert!(r.flags.contains(CellFlags::BOLD | CellFlags::ITALIC));

    let insp = frame.inspect_shapes();
    assert!(insp.has_text_fg_color(Color32::from_rgb(200, 10, 20)));
    assert!(insp.path_count >= 1);

    // Also verify restored SGR 58 underline color renders into overlay mesh under SGR 4:1.
    h.runtime
        .input_mut()
        .push_str("\x1b[0;4:1;58;2;1;2;3m\x1b[#{\x1b[0m\x1b[#}U");
    let ul_frame = h.run_frame();
    assert!(
        ul_frame
            .inspect_shapes()
            .has_rect_or_mesh_color(Color32::from_rgb(1, 2, 3))
    );

    // Selective push (`CSI 1;11 # p`) saves ONLY BOLD (1) and BG (11), preserving inner ITALIC on pop (`CSI # q`).
    h.runtime
        .input_mut()
        .push_str("\x1b[0;1;42m\x1b[1;11#p\x1b[0;3;44mI\x1b[#qB");
    h.run_frame();
    let b = h.runtime.terminal().cell_at(4, 0).expect("B cell");
    assert_eq!(b.ch, 'B');
    assert!(b.flags.contains(CellFlags::BOLD | CellFlags::ITALIC));
    assert_eq!(b.bg, PackedColor::Indexed(2));
}

#[test]
fn t1_item3_05_xtwinops_11_13_19_20_21() {
    let mut h = HeadlessTermHarness::scripted();
    let warm = h.run_frame();
    let cols = warm.terminal_response.cols;
    let rows = warm.terminal_response.rows;

    h.runtime
        .input_mut()
        .push_str("\x1b]1;MyIcon\x07\x1b]2;MyTitle\x07\x1b[11t\x1b[13t\x1b[19t\x1b[20t\x1b[21t");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        format!("\x1b[1t\x1b[3;0;0t\x1b[9;{rows};{cols}t\x1b]LMyIcon\x1b\\\x1b]lMyTitle\x1b\\")
    );

    // Also verify non-default window iconified, window position, and screen char size in core.
    let mut term = Terminal::new(80, 24, CoreConfig::default());
    term.set_window_iconified(true);
    term.set_window_pos(120, 340);
    term.set_screen_char_size(Some((160, 48)));
    term.advance(b"\x1b]1;IconX\x07\x1b]2;TitleY\x07\x1b[11t\x1b[13t\x1b[19t\x1b[20t\x1b[21t");
    assert_eq!(
        String::from_utf8(term.drain_outgoing()).expect("valid utf8"),
        "\x1b[2t\x1b[3;120;340t\x1b[9;48;160t\x1b]LIconX\x1b\\\x1b]lTitleY\x1b\\"
    );
}

#[test]
fn t1_item3_06_g2_g3_charsets_and_locking_single_shifts() {
    let mut h = HeadlessTermHarness::scripted();
    h.focus_terminal();

    // Designate G0=ASCII, G1=DEC, G2=DEC, G3=UK; save via DECSC (`ESC 7`), test LS2/LS3/SS2/SS3/SO/SI/LS*R.
    h.runtime.input_mut().push_str(concat!(
        "\x1b(B\x1b)0\x1b*0\x1b+A",
        "a",       // G0 ASCII -> 'a'
        "\x1bnq",  // LS2 (GL=G2 DEC) -> '─'
        "\x1bo#",  // LS3 (GL=G3 UK) -> '£'
        "\x1bNl#", // SS2 (single-shift G2 DEC) -> '┌', then back to G3 UK '#' -> '£'
        "\x1bO#",  // SS3 (single-shift G3 UK) -> '£'
        "\x0ex",   // SO (GL=G1 DEC) -> '│'
        "\x0fz",   // SI (GL=G0 ASCII) -> 'z'
    ));
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("a─£┌££│z")
    );

    // LS1R / LS2R / LS3R (`ESC ~`, `ESC }`, `ESC |`) update GR selector without corrupting UTF-8 output.
    h.runtime.input_mut().push_str("\x1b~\x1b}\x1b|\r\n你好🚀");
    h.run_frame();
    assert_eq!(h.runtime.terminal().cursor().charsets.gr, 3);
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("你好🚀")
    );

    // DECSC / DECRC (`ESC 7` / `ESC 8`) preserves G0..G3 and GL/GR slots.
    h.runtime
        .input_mut()
        .push_str("\x1bn\x1b7\x1b*B\x1bo\x1b8q");
    h.run_frame();
    let line2 = h
        .runtime
        .terminal()
        .visible_line_text(1)
        .expect("row 1 exists");
    assert!(line2.ends_with('\u{2500}'));
    assert_eq!(h.runtime.terminal().cursor().charsets.gl, 2);
}

#[test]
fn t1_item3_07_decaln_and_decbi_decfi() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(220.0, 140.0);
    let warm = h.run_frame();
    let cols = warm.terminal_response.cols;
    let rows = warm.terminal_response.rows;
    assert!(cols >= 10 && rows >= 5);

    // Push lines into scrollback, set margins + DECOM, then trigger DECALN (`ESC # 8`).
    let mut seq = String::from("SCROLL_KEEP\r\n");
    for i in 0..rows {
        seq.push_str(&format!("line_{i}\r\n"));
    }
    seq.push_str("\x1b[2;4r\x1b[?69h\x1b[3;6s\x1b[?6h\x1b#8");
    h.runtime.input_mut().push_str(seq);
    h.run_frame();

    let term = h.runtime.terminal();
    assert!(term.scrollback_len() >= 1);
    assert_eq!(term.scrollback_line_text(0).as_deref(), Some("SCROLL_KEEP"));
    assert_eq!(term.cursor_pos(), (0, 0));
    assert!(!term.modes().origin_mode);
    let expected_row = "E".repeat(usize::from(cols));
    for r in 0..rows {
        assert_eq!(
            term.visible_line_text(r).as_deref(),
            Some(expected_row.as_str())
        );
    }

    // DECBI (`ESC 6`) at scroll_left shifts margin box right; DECFI (`ESC 9`) at scroll_right shifts left.
    h.runtime
        .input_mut()
        .push_str("\x1b[2J\x1b[?69h\x1b[2;4r\x1b[3;6s\x1b[2;3HABCD\x1b[2;3H\x1b6");
    h.run_frame();
    assert_eq!(h.runtime.terminal().cursor_pos(), (2, 1));
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("   ABC")
    );

    // Cursor inside margins steps left/right without scrolling; at scroll_right DECFI shifts left.
    h.runtime.input_mut().push_str("\x1b[2;5H\x1b9\x1b9");
    h.run_frame();
    assert_eq!(h.runtime.terminal().cursor_pos(), (5, 1));
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("  ABC")
    );
}

#[test]
fn t1_item3_08_declrmm_nel_bs_decstbm_parity() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(240.0, 140.0);
    h.run_frame();

    // Enable DECLRMM cols 3..=8 (0-based 2..=7), DECSTBM rows 2..=5 (0-based 1..=4), and DECOM (?6h).
    // NEL (`ESC E`) lands at scroll_left (2); BS stops at scroll_left (2).
    h.runtime
        .input_mut()
        .push_str("\x1b[?69h\x1b[3;8s\x1b[2;5r\x1b[?6h\x1b[1;5H\x1bE");
    h.run_frame();
    assert_eq!(h.runtime.terminal().cursor_pos(), (2, 2));

    // BS stops at scroll_left when starting inside margins.
    h.runtime.input_mut().push_str("\x1b[1;3H\x08\x08\x08");
    h.run_frame();
    assert_eq!(h.runtime.terminal().cursor_pos(), (2, 1));

    // DECSTBM under DECOM homes cursor to (scroll_top, scroll_left) so CPR reports (1, 1).
    h.runtime.input_mut().push_str("\x1b[2;4r\x1b[6n");
    h.run_frame();
    assert_eq!(h.runtime.terminal().cursor_pos(), (2, 1));
    assert_eq!(h.runtime.input().written_str(), "\x1b[1;1R");
    h.runtime.input_mut().clear_written();

    // Disabling ?69 makes `CSI s` act as SCOSC again and ignores `CSI Pl;Pr s`.
    h.runtime
        .input_mut()
        .push_str("\x1b[?6l\x1b[?69l\x1b[2;4H\x1b[s\x1b[1;1H\x1b[3;7s\x1b[u");
    h.run_frame();
    assert_eq!(h.runtime.terminal().cursor_pos(), (3, 1));
}

// ===========================================================================
// Item #4 — Modern OSC & Input Protocols: Tier 1 (7 Equivalence-Class Tests)
// ===========================================================================

#[test]
fn t1_item4_01_osc5_osc105_special_colors() {
    let mut h = HeadlessTermHarness::scripted();
    h.run_frame();
    let epoch0 = h.runtime.terminal().palette_epoch();

    // Chained OSC 5 set & ST-terminated query for bold (0) and italic (4), plus defaults (1, 3).
    h.runtime
        .input_mut()
        .push_str("\x1b]5;0;#ff1122;4;rgb:00/aa/bb\x07\x1b]5;0;?;4;?\x1b\\");
    let set_frame = h.run_frame();
    assert!(
        set_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    let epoch1 = h.runtime.terminal().palette_epoch();
    assert!(epoch1 > epoch0);
    assert_eq!(
        h.runtime.terminal().special_colors().bold,
        Some([0xff, 0x11, 0x22])
    );
    assert_eq!(
        h.runtime.terminal().special_colors().italic,
        Some([0x00, 0xaa, 0xbb])
    );
    assert_eq!(
        h.runtime.terminal().active_snapshot().special_colors,
        [
            Some([0xff, 0x11, 0x22]),
            None,
            None,
            None,
            Some([0x00, 0xaa, 0xbb]),
        ]
    );
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b]5;0;rgb:ffff/1111/2222\x1b\\\x1b]5;4;rgb:0000/aaaa/bbbb\x1b\\"
    );
    h.runtime.input_mut().clear_written();

    // Selective reset `OSC 105 ; 0` resets bold while preserving italic.
    h.runtime.input_mut().push_str("\x1b]105;0\x07");
    let sel_reset = h.run_frame();
    assert!(
        sel_reset
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    assert!(h.runtime.terminal().palette_epoch() > epoch1);
    assert_eq!(h.runtime.terminal().special_colors().bold, None);
    assert_eq!(
        h.runtime.terminal().special_colors().italic,
        Some([0x00, 0xaa, 0xbb])
    );

    // Global reset `OSC 105` clears all special colors.
    h.runtime.input_mut().push_str("\x1b]105\x07");
    let all_reset = h.run_frame();
    assert!(
        all_reset
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    assert_eq!(
        h.runtime.terminal().active_snapshot().special_colors,
        [None; 5]
    );
}

#[test]
fn t1_item4_02_osc6_document_uri_and_ris() {
    let mut h = HeadlessTermHarness::scripted();
    h.run_frame();
    assert_eq!(h.runtime.terminal().document_uri(), None);

    // iTerm2 tab color (`OSC 6 ; 1 ; bg ; ...`) and xterm enable (`OSC 6 ; 0 ; 1`) do not touch document URI.
    h.runtime
        .input_mut()
        .push_str("\x1b]6;1;bg;red;brightness;255\x07\x1b]6;0;1\x07");
    let ignored = h.run_frame();
    assert_eq!(h.runtime.terminal().document_uri(), None);
    assert!(
        !ignored
            .terminal_response
            .events
            .iter()
            .any(|ev| matches!(ev, TerminalEvent::DocumentChanged(_)))
    );

    // Set document URI via ST, then clear with empty OSC 6.
    h.runtime
        .input_mut()
        .push_str("\x1b]6;file://localhost/Users/dev/my%20doc.rs\x1b\\");
    let set_doc = h.run_frame();
    assert_eq!(
        h.runtime.terminal().document_uri(),
        Some("file://localhost/Users/dev/my%20doc.rs")
    );
    assert!(
        set_doc
            .terminal_response
            .events
            .contains(&TerminalEvent::DocumentChanged(Some(
                "file://localhost/Users/dev/my%20doc.rs".to_owned()
            )))
    );

    h.runtime.input_mut().push_str("\x1b]6;\x07");
    let cleared = h.run_frame();
    assert_eq!(h.runtime.terminal().document_uri(), None);
    assert!(
        cleared
            .terminal_response
            .events
            .contains(&TerminalEvent::DocumentChanged(None))
    );

    // RIS (`ESC c`) also clears document URI.
    h.runtime
        .input_mut()
        .push_str("\x1b]6;file:///tmp/build.log\x07");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().document_uri(),
        Some("file:///tmp/build.log")
    );
    h.runtime.input_mut().push_str("\x1bc");
    h.run_frame();
    assert_eq!(h.runtime.terminal().document_uri(), None);
}

#[test]
fn t1_item4_03_osc9_4_progress_all_states_and_events() {
    let mut h = HeadlessTermHarness::scripted();
    h.runtime.input_mut().push_str(concat!(
        "\x1b]9;4;1;42\x07",
        "\x1b]9;4;2;90\x07",
        "\x1b]9;4;3\x07",
        "\x1b]9;4;4;60\x07",
        "\x1b]9;4;0\x07",
        "\x1b]9;Build finished\x07",
    ));
    let frame = h.run_frame();
    assert_eq!(
        frame.terminal_response.events,
        vec![
            TerminalEvent::Progress {
                state: 1,
                progress: 42
            },
            TerminalEvent::Progress {
                state: 2,
                progress: 90
            },
            TerminalEvent::Progress {
                state: 3,
                progress: 0
            },
            TerminalEvent::Progress {
                state: 4,
                progress: 60
            },
            TerminalEvent::Progress {
                state: 0,
                progress: 0
            },
            TerminalEvent::Notification {
                title: String::new(),
                body: "Build finished".to_owned()
            },
        ]
    );
}

#[test]
fn t1_item4_04_osc22_stack_query_and_osc99_structured() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(400.0, 240.0);
    h.focus_terminal();
    let hover_pos = h.cell_center(2, 2).expect("hover cell inside grid");

    // OSC 22 push stack, comma-separated fallback, capability query, current query, and pop.
    h.runtime.input_mut().push_str(concat!(
        "\x1b]22;>grab\x07",
        "\x1b]22;>nonexistent_shape,pointer\x07",
        "\x1b]22;?__current__\x07",
        "\x1b]22;?pointer,bogus,text\x07",
    ));
    h.inject_pointer_move(hover_pos);
    let pushed = h.run_frame();
    assert_eq!(pushed.platform_output.cursor_icon, CursorIcon::PointingHand);
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b]22;pointer\x07\x1b]22;1,0,1\x07"
    );
    h.runtime.input_mut().clear_written();

    // Pop restores `grab`.
    h.runtime.input_mut().push_str("\x1b]22;<\x07");
    let popped = h.run_frame();
    assert_eq!(popped.platform_output.cursor_icon, CursorIcon::Grab);
    assert!(
        popped
            .terminal_response
            .events
            .contains(&TerminalEvent::MouseShape("grab".to_owned()))
    );

    // OSC 99 keyed multi-chunk with base64 body, `p=close`, and `p=?` capability query.
    h.runtime.input_mut().push_str(concat!(
        "\x1b]99;i=job1:p=title:u=2:o=always:d=0;Deploy\x1b\\",
        "\x1b]99;i=job2:p=title:d=0;Canceled\x1b\\",
        "\x1b]99;i=job2:p=close;\x1b\\",
        "\x1b]99;i=job1:p=body:e=1:d=1;T0s=\x1b\\",
        "\x1b]99;i=q1:p=?;\x1b\\",
    ));
    let notif = h.run_frame();
    assert!(
        notif
            .terminal_response
            .events
            .contains(&TerminalEvent::NotificationClosed {
                id: "job2".to_owned()
            })
    );
    assert!(
        notif
            .terminal_response
            .events
            .contains(&TerminalEvent::Notification {
                title: "Deploy".to_owned(),
                body: "OK".to_owned()
            })
    );
    assert!(
        !notif.terminal_response.events.iter().any(
            |ev| matches!(ev, TerminalEvent::Notification { title, .. } if title == "Canceled")
        )
    );
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b]99;i=q1;p=title,body,close,?:e=0,1:d=0,1:o=always,unfocused,invisible:u=0,1,2\x1b\\"
    );
}

#[test]
fn t1_item4_05_osc66_text_sizing() {
    let mut h = HeadlessTermHarness::scripted();
    h.run_frame();

    // Auto-width s=2 puts H at (0,0) and 1 at (2,0), placing SIZED_SPACER in 2x2 boxes, then "sub" at (4,0).
    h.runtime
        .input_mut()
        .push_str("\x1b]66;s=2;H1\x07sub\r\n\r\n\x1b]66;s=2:w=3:n=1:d=2:v=2:h=1;A\x07Z");
    let frame = h.run_frame();

    let term = h.runtime.terminal();
    assert_eq!(term.visible_line_text(0).as_deref(), Some("H1sub"));
    assert_eq!(term.render_snapshot_line_text(0).as_deref(), Some("H1sub"));
    assert_eq!(term.visible_line_text(2).as_deref(), Some("AZ"));
    assert_eq!(term.cursor_pos(), (7, 2));

    let snap = term.active_snapshot();
    let row0 = snap.visible_rows.get(0).expect("row 0");
    let h_extra_id = row0.cells()[0].extra_id;
    assert_eq!(
        snap.cell_extra(h_extra_id).and_then(|e| e.text_sizing),
        Some(TextSizing {
            scale: 2,
            width: 0,
            frac_num: 0,
            frac_den: 0,
            v_align: 0,
            h_align: 0,
        })
    );
    assert!(
        term.cell_at(1, 0)
            .expect("sized spacer (1,0)")
            .flags
            .contains(CellFlags::SIZED_SPACER)
    );
    assert!(
        term.cell_at(0, 1)
            .expect("sized spacer (0,1)")
            .flags
            .contains(CellFlags::SIZED_SPACER)
    );

    let row2 = snap.visible_rows.get(2).expect("row 2");
    let a_extra_id = row2.cells()[0].extra_id;
    assert_eq!(
        snap.cell_extra(a_extra_id).and_then(|e| e.text_sizing),
        Some(TextSizing {
            scale: 2,
            width: 3,
            frac_num: 1,
            frac_den: 2,
            v_align: 2,
            h_align: 1,
        })
    );
    let insp66 = frame.inspect_shapes();
    assert!(!insp66.text_mesh_vertices().is_empty());
    let bounds66 = insp66.text_mesh_bounds().expect("OSC 66 mesh bounds");
    assert!(bounds66.height() > frame.terminal_response.cell_size.y * 2.0);
}

#[test]
fn t1_item4_06_modify_other_keys_levels_and_query() {
    let mut h = HeadlessTermHarness::scripted();
    h.runtime
        .input_mut()
        .push_str("\x1b[?4m\x1b[>4;1m\x1b[?4m\x1b[>4;2m\x1b[?4m");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b[>4;0m\x1b[>4;1m\x1b[>4;2m"
    );
    assert_eq!(h.runtime.terminal().modify_other_keys_level(), 2);

    #[cfg(feature = "kitty-keyboard")]
    {
        let shift_enter = KittyKeyEvent {
            key: KittyKey::Enter,
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                shift: true,
                ..Default::default()
            },
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        let ctrl_tab = KittyKeyEvent {
            key: KittyKey::Tab,
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                ctrl: true,
                ..Default::default()
            },
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        let ctrl_a = KittyKeyEvent {
            key: KittyKey::Char('a'),
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                ctrl: true,
                ..Default::default()
            },
            shifted_key: Some('A'),
            base_layout_key: None,
            associated_text: None,
        };
        let shift_space = KittyKeyEvent {
            key: KittyKey::Space,
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                shift: true,
                ..Default::default()
            },
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        let ctrl_shift_a = KittyKeyEvent {
            key: KittyKey::Char('a'),
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                ctrl: true,
                shift: true,
                ..Default::default()
            },
            shifted_key: Some('A'),
            base_layout_key: None,
            associated_text: None,
        };

        // Level 2 (`Always`): encodes Ctrl+a, Shift+Space, Ctrl+Shift+A.
        assert_eq!(
            h.runtime.terminal().modify_other_keys(),
            ModifyOtherKeys::Always
        );
        let mut buf = Vec::new();
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&ctrl_a, &mut buf)
        );
        assert_eq!(buf, b"\x1b[27;5;97~");
        buf.clear();
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&shift_space, &mut buf)
        );
        assert_eq!(buf, b"\x1b[27;2;32~");
        buf.clear();
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&ctrl_shift_a, &mut buf)
        );
        assert_eq!(buf, b"\x1b[27;6;65~");

        // Switch to Level 1 (`Enabled`): Shift+Enter & Ctrl+Tab encoded; Ctrl+a keeps legacy 0x01.
        h.runtime.input_mut().push_str("\x1b[>4;1m");
        h.run_frame();
        assert_eq!(
            h.runtime.terminal().modify_other_keys(),
            ModifyOtherKeys::Enabled
        );
        buf.clear();
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&shift_enter, &mut buf)
        );
        assert_eq!(buf, b"\x1b[27;2;13~");
        buf.clear();
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&ctrl_tab, &mut buf)
        );
        assert_eq!(buf, b"\x1b[27;5;9~");
        buf.clear();
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&ctrl_a, &mut buf)
        );
        assert_eq!(buf, b"\x01");
    }

    // `CSI > 4 m` resets level to 0.
    h.runtime.input_mut().push_str("\x1b[>4m");
    h.run_frame();
    assert_eq!(h.runtime.terminal().modify_other_keys_level(), 0);
}

#[test]
fn t1_item4_07_osc52_policy_sgr_pixels_and_bracketed_paste() {
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

    // Enable Allow read policy + write OSC 52 + read query + host reply.
    h.runtime
        .terminal_mut()
        .set_osc52_read_policy(Osc52ReadPolicy::Allow);
    h.runtime
        .input_mut()
        .push_str("\x1b]52;c;aGVsbG8=\x07\x1b]52;c;?\x07");
    let osc52_frame = h.run_frame();
    assert!(
        osc52_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::ClipboardWrite("hello".to_owned()))
    );
    assert!(osc52_frame.terminal_response.events.contains(
        &TerminalEvent::ClipboardReadRequested {
            selection: ClipboardSelection::Clipboard
        }
    ));
    h.runtime
        .terminal_mut()
        .provide_clipboard_read(ClipboardSelection::Clipboard, "ok");
    assert_eq!(
        h.runtime.terminal_mut().drain_outgoing(),
        b"\x1b]52;c;b2s=\x1b\\"
    );

    // DEC ?1003h + ?1016h SGR-pixels hover, press, release, and wheel.
    h.runtime.input_mut().push_str("\x1b[?1003h\x1b[?1016h");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().modes().mouse_protocol,
        MouseProtocol::AnyEvent
    );
    assert_eq!(
        h.runtime.terminal().modes().mouse_encoding,
        MouseEncoding::SgrPixels
    );
    let direct_px = h.runtime.terminal().encode_mouse_event_pixels(
        MouseInputEvent {
            kind: MouseEventKind::Press,
            button: Some(MouseButton::Left),
            col: 3,
            row: 2,
            modifiers: MouseModifiers {
                shift: false,
                alt: true,
                ctrl: false,
            },
        },
        42,
        84,
    );
    assert_eq!(direct_px.as_deref(), Some(b"\x1b[<8;42;84M".as_slice()));
    let p = h.cell_center(3, 2).expect("cell (3,2)");
    let px = ((p.x - origin.x).max(0.0) * 2.0).round() as u32;
    let py = ((p.y - origin.y).max(0.0) * 2.0).round() as u32;
    h.inject_pointer_move(p);
    h.inject_pointer_button(p, PointerButton::Primary, true, Modifiers::NONE);
    h.inject_pointer_button(p, PointerButton::Primary, false, Modifiers::NONE);
    h.inject_scroll(MouseWheelUnit::Line, Vec2::new(0.0, 1.0), Modifiers::NONE);
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        format!("\x1b[<35;{px};{py}M\x1b[<0;{px};{py}M\x1b[<0;{px};{py}m\x1b[<64;{px};{py}M")
    );
    h.runtime.input_mut().clear_written();

    // DEC ?2004h bracketed paste wraps safe text + \t/\n.
    h.runtime.input_mut().push_str("\x1b[?2004h");
    h.run_frame();
    h.inject_paste("hello\tworld\n");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b[200~hello\tworld\n\x1b[201~"
    );
}

// ===========================================================================
// Item #3 — VT / CSI / ESC Sequences: Tier 2 (7 Boundary / Corner / Error Tests)
// ===========================================================================

#[test]
fn t2_item3_01_sl_sr_zero_omitted_overflow_and_wide_boundary() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(140.0, 100.0);
    let warm = h.run_frame();
    let cols = warm.terminal_response.cols;
    assert!(cols >= 8);

    // CSI 0 SP @ and CSI SP @ both shift left by 1; wide char split at left margin clears orphan spacer.
    h.runtime
        .input_mut()
        .push_str("A中文B\x1b[1;1H\x1b[0 @\x1b[ @");
    h.run_frame();
    let term = h.runtime.terminal();
    // After SL 1 twice (total 2 cols left), 'A' and lead of '中' are shifted out, spacer cleared to ' ',
    // leaving ' ' at col 0, '文' at col 1..=2, 'B' at col 3.
    let c0 = term.cell_at(0, 0).expect("col 0");
    assert_eq!(c0.ch, ' ');
    assert!(!c0.is_wide_spacer);
    assert_eq!(term.cell_at(1, 0).expect("文 lead").ch, '文');
    assert!(term.cell_at(2, 0).expect("文 spacer").is_wide_spacer);
    assert_eq!(term.cell_at(3, 0).expect("B").ch, 'B');

    // SL/SR operate on configured margins even when cursor sits outside margins.
    h.runtime
        .input_mut()
        .push_str("\x1b[2J\x1b[1;1H01234567\x1b[?69h\x1b[3;6s\x1b[1;1H\x1b[2 @");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("0145  67")
    );
    assert_eq!(h.runtime.terminal().cursor_pos(), (0, 0));

    // Overflow `CSI 999 SP A` clears margin box cleanly with BCE bg.
    h.runtime.input_mut().push_str("\x1b[48;5;3m\x1b[999 A");
    h.run_frame();
    for col in 2..=5u16 {
        let cell = h.runtime.terminal().cell_at(col, 0).expect("margin cell");
        assert_eq!(cell.ch, ' ');
        assert_eq!(cell.bg, PackedColor::Indexed(3));
    }
    assert_eq!(
        h.runtime.terminal().cell_at(0, 0).expect("outside left").ch,
        '0'
    );
    assert_eq!(
        h.runtime
            .terminal()
            .cell_at(6, 0)
            .expect("outside right")
            .ch,
        '6'
    );
}

#[test]
fn t2_item3_02_decsca_invalid_param_and_wide_selective_erase() {
    let mut h = HeadlessTermHarness::scripted();
    h.focus_terminal();

    // Enable DECSCA 1, send invalid `CSI 99 " q` (ignored), write protected '中', disable DECSCA, write '文'.
    h.runtime
        .input_mut()
        .push_str("\x1b[1\"q\x1b[99\"q\x1bP$q\"q\x1b\\中\x1b[0\"q文");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1bP1$r1\"q\x1b\\");
    h.runtime.input_mut().clear_written();

    // DECSEL 2 (`CSI ? 2 K`) keeps both lead & spacer of '中' and erases both lead & spacer of '文'.
    h.runtime.input_mut().push_str("\x1b[?2K");
    h.run_frame();
    let term = h.runtime.terminal();
    assert_eq!(term.cell_at(0, 0).expect("中 lead").ch, '中');
    assert!(term.cell_at(0, 0).expect("中 lead").is_wide);
    assert!(term.cell_at(1, 0).expect("中 spacer").is_wide_spacer);
    assert!(
        term.cell_at(1, 0)
            .expect("中 spacer")
            .flags
            .contains(CellFlags::PROTECTED)
    );
    assert_eq!(term.cell_at(2, 0).expect("erased lead").ch, ' ');
    assert!(!term.cell_at(2, 0).expect("erased lead").is_wide);
    assert_eq!(term.cell_at(3, 0).expect("erased spacer").ch, ' ');
    assert!(!term.cell_at(3, 0).expect("erased spacer").is_wide_spacer);

    // DECSTR (`CSI ! p`) resets active DECSCA state to unprotected while preserving screen text.
    h.runtime
        .input_mut()
        .push_str("\x1b[1\"q\x1b[!p\x1bP$q\"q\x1b\\");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1bP1$r0\"q\x1b\\");
    assert_eq!(
        h.runtime.terminal().cell_at(0, 0).expect("中 kept").ch,
        '中'
    );
}

#[test]
fn t2_item3_03_rect_ops_inverted_oob_and_overlap() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(100.0, 100.0);
    let warm = h.run_frame();
    let cols = warm.terminal_response.cols;
    assert!(cols >= 6);

    // Seed "ABCDEF" and verify snapshot semantics on overlapping DECCRA (1..=4 -> col 3 = "ABABCD").
    h.runtime
        .input_mut()
        .push_str("ABCDEF\x1b[1;1;1;4;1;1;3;1$v");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("ABABCD")
    );

    // DECFRA rejects inverted coords, Pch=0, empty Pch slot, and BEL control (must NOT emit Bell!).
    h.runtime.input_mut().push_str(concat!(
        "\x1b[65;5;1;2;4$x", // Pt > Pb
        "\x1b[65;1;8;4;3$x", // Pl > Pr
        "\x1b[0;1;1;1;2$x",  // Pch=0
        "\x1b[;1;1;1;2$x",   // empty Pch slot
        "\x1b[7;1;1;1;2$x",  // BEL control
    ));
    let invalid_fra = h.run_frame();
    assert!(
        !invalid_fra
            .terminal_response
            .events
            .contains(&TerminalEvent::Bell)
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("ABABCD")
    );

    // Pr=999 clamps cleanly to `cols`.
    h.runtime.input_mut().push_str("\x1b[43;2;2;2;999$x");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(1),
        Some(format!(" {}", "+".repeat(usize::from(cols) - 1)))
    );

    // DECCRA ignores non-existent page >= 2 and safely slices wide characters at source boundaries.
    h.runtime
        .input_mut()
        .push_str("\x1b[2J\x1b[1;1H中文\x1b[1;1;1;4;2;2;1;1$v\x1b[1;2;1;3;1;2;1;1$v");
    h.run_frame();
    let term = h.runtime.terminal();
    // Row 1 cols 0..2 got sliced halves of '中' and '文', so both must become plain spaces!
    assert_eq!(term.cell_at(0, 1).expect("sliced right half of 中").ch, ' ');
    assert!(!term.cell_at(0, 1).expect("no orphan spacer").is_wide_spacer);
    assert_eq!(term.cell_at(1, 1).expect("sliced left half of 文").ch, ' ');
    assert!(!term.cell_at(1, 1).expect("no orphan lead").is_wide);
}

#[test]
fn t2_item3_04_xtpushsgr_stack_overflow_and_empty_pop() {
    let mut h = HeadlessTermHarness::scripted();
    h.run_frame();

    // Popping an empty stack is a safe no-op.
    h.runtime.input_mut().push_str("\x1b[1;31m\x1b[#}A");
    h.run_frame();
    let a = h.runtime.terminal().cell_at(0, 0).expect("A");
    assert_eq!(a.fg, PackedColor::Indexed(1));
    assert!(a.flags.contains(CellFlags::BOLD));

    // Push 16 levels (beyond depth 10 cap), then pop 20 times: balanced pops recover cleanly.
    let mut push_pop = String::from("\x1b[0;1m");
    for _ in 0..16 {
        push_pop.push_str("\x1b[#{\x1b[0;3m");
    }
    for _ in 0..20 {
        push_pop.push_str("\x1b[#}");
    }
    push_pop.push('B');
    h.runtime.input_mut().push_str(push_pop);
    h.run_frame();
    let b = h.runtime.terminal().cell_at(1, 0).expect("B");
    assert!(b.flags.contains(CellFlags::BOLD));
    assert!(!b.flags.contains(CellFlags::ITALIC));

    // DECSTR clears SGR stack so subsequent pop does not restore stale attributes.
    h.runtime
        .input_mut()
        .push_str("\x1b[0;34m\x1b[#{\x1b[!p\x1b[32m\x1b[#}C");
    h.run_frame();
    let c = h.runtime.terminal().cell_at(2, 0).expect("C");
    assert_eq!(c.fg, PackedColor::Indexed(2));
}

#[test]
fn t2_item3_05_xtwinops_20_21_empty_utf8_and_control_sanitization() {
    let mut h = HeadlessTermHarness::scripted();
    let warm = h.run_frame();
    let cols = warm.terminal_response.cols;
    let rows = warm.terminal_response.rows;

    // Empty icon & title replies.
    h.runtime.input_mut().push_str("\x1b[20t\x1b[21t");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b]L\x1b\\\x1b]l\x1b\\");
    h.runtime.input_mut().clear_written();

    // Multi-byte UTF-8 CJK + emoji plus embedded control chars stripped from 20t/21t replies.
    h.runtime
        .input_mut()
        .push_str("\x1b]1;ic\x08on\x07\x1b]2;终端\x7f🚀\x1b\\\x1b[20t\x1b[21t");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b]Licon\x1b\\\x1b]l终端🚀\x1b\\"
    );

    // Core `pixel_size == (0, 0)` fallback on `14t` and negative `window_pos` on `13t`.
    let mut term = Terminal::new(cols, rows, CoreConfig::default());
    term.set_pixel_size(0, 0);
    term.set_window_pos(-10, 42);
    term.advance(b"\x1b[13t\x1b[14t\x1b[16t");
    let expected_w = usize::from(cols) * 8;
    let expected_h = usize::from(rows) * 16;
    assert_eq!(
        String::from_utf8(term.drain_outgoing()).expect("valid utf8"),
        format!("\x1b[3;-10;42t\x1b[4;{expected_h};{expected_w}t\x1b[6;16;8t")
    );
}

#[test]
fn t2_item3_06_g2_g3_single_shift_interrupted_by_control() {
    let mut h = HeadlessTermHarness::scripted();
    h.run_frame();

    // Pending SS2/SS3 cancelled by CSI, ESC, and C0 BS, but preserved across NUL and DEL.
    // Unknown charset designator (`ESC * Z`) is ignored without replacing G2=DEC.
    h.runtime.input_mut().push_str(concat!(
        "\x1b(B\x1b*0\x1b*Z\x1b+0",
        "\x1bN\x1b[1Cq",            // CSI cancels SS2 -> " q"
        "\r\n\x1bO\x1b7q",          // ESC 7 cancels SS3 -> "q"
        "\r\nX\x1bN\x08q",          // BS cancels SS2 -> "q" at col 0
        "\r\n\x1bN\x00q\x1bN\x7fq", // NUL and DEL preserve SS2 -> "──"
    ));
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some(" q")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("q")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(2).as_deref(),
        Some("q")
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(3).as_deref(),
        Some("──")
    );

    // DECSTR resets charsets back to ASCII / GL=0 / GR=2.
    h.runtime.input_mut().push_str("\x1bn\x1b[!pq");
    h.run_frame();
    assert_eq!(h.runtime.terminal().cursor().charsets.gl, 0);
    assert_eq!(h.runtime.terminal().cursor().charsets.gr, 2);
}

#[test]
fn t2_item3_07_decbi_decfi_at_grid_edges_and_outside_margins() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(140.0, 100.0);
    let warm = h.run_frame();
    let cols = warm.terminal_response.cols;
    assert!(cols >= 8);

    // When cursor is on row 0 OUTSIDE vertical margins (2..=4), DECBI at col 0 and DECFI at col=cols-1 are no-ops.
    h.runtime
        .input_mut()
        .push_str(format!("ROW0\x1b[2;4r\x1b[1;1H\x1b6\x1b[1;{cols}H\x1b9"));
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("ROW0")
    );
    assert_eq!(h.runtime.terminal().cursor_pos(), (cols - 1, 0));

    // With DECLRMM on (cols 3..=6), DECBI at col 0 (outside scroll_left=2) is a no-op;
    // DECBI at col=scroll_left on row 0 (outside vertical margins 2..=4) moves cursor left to col 1 without scrolling.
    h.runtime
        .input_mut()
        .push_str("\x1b[?69h\x1b[3;6s\x1b[2;4r\x1b[1;1H\x1b6\x1b[1;3H\x1b6");
    h.run_frame();
    assert_eq!(h.runtime.terminal().cursor_pos(), (1, 0));
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("ROW0")
    );

    // DECALN (`ESC # 8`) on minimum 1x1 grid succeeds cleanly without panic.
    let mut tiny = Terminal::new(1, 1, CoreConfig::default());
    tiny.advance(b"\x1b#8");
    assert_eq!(tiny.visible_line_text(0).as_deref(), Some("E"));
    assert_eq!(tiny.cursor_pos(), (0, 0));
}

// ===========================================================================
// Item #4 — Modern OSC & Input Protocols: Tier 2 (7 Boundary / Error Tests)
// ===========================================================================

#[test]
fn t2_item4_01_osc5_osc105_invalid_index_and_bad_color() {
    let mut h = HeadlessTermHarness::scripted();
    h.run_frame();
    let epoch0 = h.runtime.terminal().palette_epoch();

    h.runtime.input_mut().push_str(concat!(
        "\x1b]5;5;#112233\x07",
        "\x1b]5;0;notacolor\x07",
        "\x1b]5;abc;#112233\x07",
        "\x1b]105;9\x07",
    ));
    let frame = h.run_frame();
    assert!(
        !frame
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    assert_eq!(h.runtime.terminal().palette_epoch(), epoch0);
    assert_eq!(
        h.runtime.terminal().active_snapshot().special_colors,
        [None; 5]
    );
}

#[test]
fn t2_item4_02_osc6_invalid_utf8_and_percent_escapes() {
    let mut h = HeadlessTermHarness::scripted();
    h.run_frame();

    // Malformed UTF-8, UTF-8 C1 control chars, relative path without scheme, and duplicate empty reset.
    let mut bad_bytes = Vec::new();
    bad_bytes.extend_from_slice(b"\x1b]6;file:///\xff\xfe\x07");
    bad_bytes.extend_from_slice("\x1b]6;file:///bad\u{0085}path\x07".as_bytes());
    bad_bytes.extend_from_slice(b"\x1b]6;/no/scheme/here\x07");
    bad_bytes.extend_from_slice(b"\x1b]6;\x07");
    h.runtime.input_mut().push_bytes(bad_bytes);
    let bad_frame = h.run_frame();
    assert_eq!(h.runtime.terminal().document_uri(), None);
    assert!(
        !bad_frame
            .terminal_response
            .events
            .iter()
            .any(|ev| matches!(ev, TerminalEvent::DocumentChanged(_)))
    );

    // Oversized URI (> MAX_HYPERLINK_URI_BYTES) is rejected cleanly.
    let huge_uri = format!("\x1b]6;file:///{}\x07", "a".repeat(9000));
    h.runtime.input_mut().push_str(huge_uri);
    let huge_frame = h.run_frame();
    assert_eq!(h.runtime.terminal().document_uri(), None);
    assert!(
        !huge_frame
            .terminal_response
            .events
            .iter()
            .any(|ev| matches!(ev, TerminalEvent::DocumentChanged(_)))
    );
}

#[test]
fn t2_item4_03_osc9_4_clamp_over_100_and_invalid_state() {
    let mut h = HeadlessTermHarness::scripted();
    h.runtime.input_mut().push_str(concat!(
        "\x1b]9;4;1;255\x07",
        "\x1b]9;4;3\x07",
        "\x1b]9;4;5;50\x07",
        "\x1b]9;4;abc;50\x07",
        "\x1b]9;4;2;notnum\x07",
    ));
    let frame = h.run_frame();
    assert_eq!(
        frame.terminal_response.events,
        vec![
            TerminalEvent::Progress {
                state: 1,
                progress: 100
            },
            TerminalEvent::Progress {
                state: 3,
                progress: 0
            },
            TerminalEvent::Progress {
                state: 2,
                progress: 0
            },
        ]
    );
}

#[test]
fn t2_item4_04_osc66_malformed_metadata_and_osc99_bad_b64() {
    let mut h = HeadlessTermHarness::scripted();
    h.run_frame();

    // Malformed OSC 66 sequences ignored cleanly; valid `s=0` clamps to 1 and `n=3:d=2` clears frac.
    h.runtime.input_mut().push_str(concat!(
        "\x1b]66;s=999;BAD\x07",
        "\x1b]66;w=8;BAD\x07",
        "\x1b]66;bad_kv;BAD\x07",
        "\x1b]66;s=1;C\u{0085}D\x07",
        "\x1b]66;s=0:n=3:d=2;Z\x07",
    ));
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("Z")
    );
    let z_extra = h
        .runtime
        .terminal()
        .active_snapshot()
        .visible_rows
        .get(0)
        .expect("row 0")
        .cells()[0]
        .extra_id;
    assert_eq!(
        h.runtime
            .terminal()
            .cell_extra(z_extra)
            .and_then(|e| e.text_sizing),
        Some(TextSizing::default())
    );

    // OSC 99 malformed base64, invalid UTF-8 base64, invalid i=<id>, and unknown p=icon part.
    h.runtime.input_mut().push_str(concat!(
        "\x1b]99;p=title:d=0;KeepMe\x1b\\",
        "\x1b]99;p=body:e=1;not_b64!\x1b\\",
        "\x1b]99;p=body:e=1;/w==\x1b\\",
        "\x1b]99;i=bad/id:p=body:d=1;Hijacked\x1b\\",
        "\x1b]99;p=icon:d=0;ICONDATA\x1b\\",
        "\x1b]99;p=body:d=1;BodyOK\x1b\\",
    ));
    let frame = h.run_frame();
    assert_eq!(
        frame.terminal_response.events,
        vec![TerminalEvent::Notification {
            title: "KeepMe".to_owned(),
            body: "BodyOK".to_owned(),
        }]
    );
}

#[test]
fn t2_item4_05_osc52_read_disabled_and_oversized_or_bad_base64() {
    let mut h = HeadlessTermHarness::scripted();
    assert_eq!(
        h.runtime.terminal().osc52_read_policy(),
        Osc52ReadPolicy::Deny
    );

    // Default Deny drops `OSC 52 ; c ; ?`; bad/invalid-UTF-8 base64 writes emit no ClipboardWrite; empty writes "".
    h.runtime
        .input_mut()
        .push_str("\x1b]52;c;?\x07\x1b]52;c;***\x07\x1b]52;c;/w==\x07\x1b]52;p;\x07");
    let deny_frame = h.run_frame();
    assert_eq!(
        deny_frame.terminal_response.events,
        vec![TerminalEvent::ClipboardWrite(String::new())]
    );
    assert!(h.runtime.input().written_bytes().is_empty());

    // Prompt policy emits ClipboardReadPrompt instead of ClipboardReadRequested.
    h.runtime
        .terminal_mut()
        .set_osc52_read_policy(Osc52ReadPolicy::Prompt);
    h.runtime.input_mut().push_str("\x1b]52;p;?\x07");
    let prompt_frame = h.run_frame();
    assert_eq!(
        prompt_frame.terminal_response.events,
        vec![TerminalEvent::ClipboardReadPrompt {
            selection: ClipboardSelection::Primary
        }]
    );
}

#[test]
fn t2_item4_06_modify_other_keys_unknown_pv_and_unmodified_keys() {
    let mut h = HeadlessTermHarness::scripted();
    h.runtime.input_mut().push_str("\x1b[>4;2m\x1b[>4;99m");
    h.run_frame();
    assert_eq!(h.runtime.terminal().modify_other_keys_level(), 2);

    // CAN (0x18) aborts partial `CSI > 4 ;` without changing modifyOtherKeys level.
    h.runtime.input_mut().push_str("\x1b[>4;\x18");
    h.run_frame();
    assert_eq!(h.runtime.terminal().modify_other_keys_level(), 2);

    #[cfg(feature = "kitty-keyboard")]
    {
        let plain_a = KittyKeyEvent {
            key: KittyKey::Char('a'),
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers::default(),
            shifted_key: Some('A'),
            base_layout_key: None,
            associated_text: None,
        };
        let shift_a = KittyKeyEvent {
            key: KittyKey::Char('a'),
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                shift: true,
                ..Default::default()
            },
            shifted_key: Some('A'),
            base_layout_key: None,
            associated_text: None,
        };
        let mut buf = Vec::new();
        assert!(
            !h.runtime
                .terminal()
                .encode_modify_other_keys_into(&plain_a, &mut buf)
        );
        assert!(
            !h.runtime
                .terminal()
                .encode_modify_other_keys_into(&shift_a, &mut buf)
        );
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&plain_a, &mut buf)
        );
        assert_eq!(buf, b"a");
        buf.clear();
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&shift_a, &mut buf)
        );
        assert_eq!(buf, b"A");
    }

    // DECSTR resets modifyOtherKeys to 0, as does `CSI > 4 n`.
    h.runtime.input_mut().push_str("\x1b[!p\x1b[?4m");
    h.run_frame();
    assert_eq!(h.runtime.terminal().modify_other_keys_level(), 0);
    assert_eq!(h.runtime.input().written_str(), "\x1b[>4;0m");
    h.runtime.input_mut().clear_written();

    h.runtime.input_mut().push_str("\x1b[>4;2m\x1b[>4n");
    h.run_frame();
    assert_eq!(h.runtime.terminal().modify_other_keys_level(), 0);
}

#[test]
fn t2_item4_07_paste_utf8_boundaries_and_mouse_1016_out_of_bounds() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_pixels_per_point(2.0);
    h.focus_terminal();
    let rect = h
        .last_outcome()
        .expect("warm frame")
        .terminal_response
        .response
        .rect;

    // Bracketed paste sanitizes split markers, C1 controls, and DEL across multi-byte UTF-8 scalars.
    h.runtime.input_mut().push_str("\x1b[?2004h");
    h.run_frame();
    h.inject_paste("é\x1b[201~中\x1b[200~\u{0080}\x7f🚀");
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b[200~é中🚀\x1b[201~");
    h.runtime.input_mut().clear_written();

    // SGR-Pixels (?1002h + ?1016h) drag starting inside and extending outside viewport clamps without underflow.
    h.runtime.input_mut().push_str("\x1b[?1002h\x1b[?1016h");
    h.run_frame();
    let inside = h.cell_center(1, 1).expect("cell (1,1)");
    h.inject_pointer_move(inside);
    h.inject_pointer_button(inside, PointerButton::Primary, true, Modifiers::NONE);
    h.run_frame();
    h.inject_pointer_move(Pos2::new(rect.min.x - 100.0, rect.min.y - 100.0));
    h.run_frame();
    h.inject_pointer_button(
        Pos2::new(rect.max.x + 200.0, rect.max.y + 200.0),
        PointerButton::Primary,
        false,
        Modifiers::NONE,
    );
    h.run_frame();

    let max_px = ((rect.width()) * 2.0).round() as u32;
    let max_py = ((rect.height()) * 2.0).round() as u32;
    let mouse_out = h.runtime.input().written_str();
    assert!(mouse_out.contains("\x1b[<32;0;0M"), "got {mouse_out:?}");
    assert!(
        mouse_out.ends_with(&format!("\x1b[<0;{max_px};{max_py}m")),
        "got {mouse_out:?}"
    );
    h.runtime.input_mut().clear_written();

    // Shift+Ctrl+Click bypasses mouse tracking.
    h.inject_click(
        inside,
        PointerButton::Primary,
        Modifiers::SHIFT | Modifiers::CTRL,
    );
    assert!(h.runtime.input().written_bytes().is_empty());
}

// ===========================================================================
// Items #3 & #4 — Tier 3 Pairwise Combinations (3 for #3 + 3 for #4 = 6 Tests)
// ===========================================================================

#[test]
fn t3_vt_01_declrmm_with_decfra_deccra_decsera_sl_sr_decbi_decfi() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(140.0, 140.0);
    let warm = h.run_frame();
    let cols = warm.terminal_response.cols;
    let rows = warm.terminal_response.rows;
    assert!(cols >= 10 && rows >= 7);

    // Seed outer sentinel frame across full grid, then constrain to rows 2..=5, cols 3..=7 under DECOM.
    let mut setup = String::from("\x1b[2J");
    for r in 1..=rows {
        setup.push_str(&format!("\x1b[{r};1H{}", "O".repeat(usize::from(cols))));
    }
    setup.push_str("\x1b[?69h\x1b[2;5r\x1b[3;7s\x1b[?6h");
    h.runtime.input_mut().push_str(setup);
    h.run_frame();

    // DECFRA fills DECOM-relative (1,1..4,5) -> 0-based rows 1..=4, cols 2..=6 with '.'.
    // Mark first two columns inside box protected and fill with '#'.
    h.runtime
        .input_mut()
        .push_str("\x1b[46;1;1;4;5$x\x1b[1\"q\x1b[35;1;1;4;2$x\x1b[0\"q");
    h.run_frame();

    // Shift right (`SR 1`), copy protected cols to DECOM col 4 (`DECCRA`), then selective erase (`DECSERA`).
    h.runtime
        .input_mut()
        .push_str("\x1b[1 A\x1b[1;2;2;3;1;3;4;1$v\x1b[1;1;4;5${");
    h.run_frame();

    // Verify outer border rows and columns remain 100% 'O', and only protected '#' shifted/copied survived.
    let term = h.runtime.terminal();
    let full_o = "O".repeat(usize::from(cols));
    assert_eq!(term.visible_line_text(0).as_deref(), Some(full_o.as_str()));
    assert_eq!(term.visible_line_text(5).as_deref(), Some(full_o.as_str()));
    for row in 1..=4u16 {
        assert_eq!(term.cell_at(0, row).expect("outer left 0").ch, 'O');
        assert_eq!(term.cell_at(1, row).expect("outer left 1").ch, 'O');
        for col in 7..cols {
            assert_eq!(term.cell_at(col, row).expect("outer right").ch, 'O');
        }
        // SR 1 moved protected '#' from cols 2..=3 to cols 3..=4; DECSERA preserved them and erased '.'.
        assert_eq!(term.cell_at(2, row).expect("erased col 2").ch, ' ');
        assert_eq!(term.cell_at(3, row).expect("protected col 3").ch, '#');
        assert_eq!(term.cell_at(4, row).expect("protected col 4").ch, '#');
    }
    // DECCRA copied cols (1+1..1+2) = 0-based (3..=4) rows (1..=2) to DECOM (3,4) = 0-based (row 3, col 5..=6).
    assert_eq!(term.cell_at(5, 3).expect("deccra col 5").ch, '#');
    assert_eq!(term.cell_at(6, 3).expect("deccra col 6").ch, '#');

    // DECBI / DECFI inside DECOM margins shift strictly inside the box without touching outer 'O'.
    h.runtime
        .input_mut()
        .push_str("\x1b[1;1H\x1b6\x1b[1;5H\x1b9");
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some(full_o.as_str())
    );
}

#[test]
fn t3_vt_02_xtpushsgr_alt_screen_and_decrqss_isolation() {
    let mut h = HeadlessTermHarness::scripted();
    h.focus_terminal();

    // Primary screen: set bold + red + DECSCA 1 (+ OSC 8 when enabled), push SGR, enter alt screen (?1049h).
    #[cfg(feature = "hyperlinks")]
    h.runtime
        .input_mut()
        .push_str("\x1b]8;;https://example.org/sgr\x07");
    h.runtime
        .input_mut()
        .push_str("\x1b[1\"q\x1b[1;31m\x1b[#{\x1b[?1049h\x1b[0;3;34m\x1b[#{\x1bP$qm\x1b\\");
    h.run_frame();
    assert!(h.runtime.terminal().is_alt_screen());
    assert_eq!(h.runtime.input().written_str(), "\x1bP1$r0;3;38;5;4m\x1b\\");
    h.runtime.input_mut().clear_written();

    // Mutate alt screen attrs, pop alt-screen push, exit alt screen (`?1049l`), pop primary push.
    h.runtime
        .input_mut()
        .push_str("\x1b[0;4;32m\x1b[#}\x1bP$qm\x1b\\\x1b[?1049l\x1b[#}P\x1bP$qm\x1b\\");
    h.run_frame();
    assert!(!h.runtime.terminal().is_alt_screen());
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1bP1$r0;3;38;5;4m\x1b\\\x1bP1$r0;1;38;5;1m\x1b\\"
    );

    let p = h.runtime.terminal().cell_at(0, 0).expect("P cell");
    assert_eq!(p.ch, 'P');
    assert_eq!(p.fg, PackedColor::Indexed(1));
    assert!(p.flags.contains(CellFlags::BOLD | CellFlags::PROTECTED));
    #[cfg(feature = "hyperlinks")]
    assert_eq!(
        h.runtime
            .terminal()
            .hyperlink_at(0, 0)
            .map(|l| l.uri.as_str()),
        Some("https://example.org/sgr")
    );
}

#[test]
fn t3_vt_03_decaln_interaction_with_margins_selection_and_scrollback() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(200.0, 120.0);
    let warm = h.run_frame();
    let cols = warm.terminal_response.cols;
    let rows = warm.terminal_response.rows;

    // Push 10 lines into scrollback.
    let mut lines = String::new();
    for i in 0..(usize::from(rows) + 10) {
        lines.push_str(&format!("SB_{i:02}\r\n"));
    }
    h.runtime.input_mut().push_str(lines);
    h.run_frame();
    let sb_len_before = h.runtime.terminal().scrollback_len();
    assert!(sb_len_before >= 10);

    // Select scrollback line while scrolled up, enable margins + DECOM, then emit DECALN (`ESC # 8`).
    h.runtime.terminal_mut().scroll_display(5);
    h.runtime
        .terminal_mut()
        .selection_start(0, 0, SelectionKind::Stream);
    h.runtime.terminal_mut().selection_update(0, 4);
    h.runtime.terminal_mut().selection_finish();
    assert!(h.runtime.terminal().has_non_empty_selection());

    h.runtime
        .input_mut()
        .push_str("\x1b[2;4r\x1b[?69h\x1b[3;6s\x1b[?6h\x1b#8");
    h.run_frame();
    h.runtime.terminal_mut().scroll_to_bottom();

    let term = h.runtime.terminal();
    assert_eq!(term.scrollback_len(), sb_len_before);
    assert_eq!(term.scrollback_line_text(0).as_deref(), Some("SB_00"));
    assert!(!term.modes().origin_mode);
    assert_eq!(term.cursor_pos(), (0, 0));
    let expected = "E".repeat(usize::from(cols));
    for r in 0..rows {
        assert_eq!(
            term.visible_line_text(r).as_deref(),
            Some(expected.as_str())
        );
    }
}

#[test]
fn t3_input_01_modify_other_keys_vs_kitty_keyboard_precedence() {
    let mut h = HeadlessTermHarness::scripted();
    h.runtime.input_mut().push_str("\x1b[>4;2m");
    h.run_frame();
    assert_eq!(h.runtime.terminal().modify_other_keys_level(), 2);

    #[cfg(feature = "kitty-keyboard")]
    {
        let ctrl_a = KittyKeyEvent {
            key: KittyKey::Char('a'),
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                ctrl: true,
                ..Default::default()
            },
            shifted_key: Some('A'),
            base_layout_key: None,
            associated_text: None,
        };
        let mut buf = Vec::new();
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&ctrl_a, &mut buf)
        );
        assert_eq!(buf, b"\x1b[27;5;97~");

        // Pushing Kitty keyboard flag 1 on primary takes precedence over modifyOtherKeys=2.
        h.runtime.input_mut().push_str("\x1b[>1u");
        h.run_frame();
        buf.clear();
        assert!(
            !h.runtime
                .terminal()
                .encode_modify_other_keys_into(&ctrl_a, &mut buf)
        );
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&ctrl_a, &mut buf)
        );
        assert_eq!(buf, b"\x1b[97;5u");

        // Entering alt screen uses an independent Kitty stack (empty), so modifyOtherKeys=2 resumes!
        h.runtime.input_mut().push_str("\x1b[?1049h");
        h.run_frame();
        buf.clear();
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&ctrl_a, &mut buf)
        );
        assert_eq!(buf, b"\x1b[27;5;97~");

        // Exiting alt screen restores primary's Kitty stack; popping primary reverts to modifyOtherKeys.
        h.runtime.input_mut().push_str("\x1b[?1049l");
        h.run_frame();
        buf.clear();
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&ctrl_a, &mut buf)
        );
        assert_eq!(buf, b"\x1b[97;5u");

        h.runtime.input_mut().push_str("\x1b[<u");
        h.run_frame();
        buf.clear();
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&ctrl_a, &mut buf)
        );
        assert_eq!(buf, b"\x1b[27;5;97~");
    }
}

#[test]
fn t3_input_02_sgr_pixels_1016_scrolled_viewport_hidpi_and_alt_scroll() {
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

    // Seed primary scrollback, scroll up (`display_offset > 0`), and verify ?1002h + ?1016h HiDPI drag.
    let mut seed = String::new();
    for i in 0..40 {
        seed.push_str(&format!("hist_{i}\r\n"));
    }
    h.runtime.input_mut().push_str(seed);
    h.run_frame();
    h.runtime.terminal_mut().scroll_display(5);
    assert!(h.runtime.terminal().display_offset() > 0);

    h.runtime
        .input_mut()
        .push_str("\x1b[?1049h\x1b[?1002h\x1b[?1016h");
    h.run_frame();

    let p1 = h.cell_center(2, 1).expect("p1");
    let p2 = h.cell_center(4, 2).expect("p2");
    let px1 = ((p1.x - origin.x).max(0.0) * 2.0).round() as u32;
    let py1 = ((p1.y - origin.y).max(0.0) * 2.0).round() as u32;
    let px2 = ((p2.x - origin.x).max(0.0) * 2.0).round() as u32;
    let py2 = ((p2.y - origin.y).max(0.0) * 2.0).round() as u32;

    h.inject_pointer_move(p1);
    h.inject_pointer_button(p1, PointerButton::Primary, true, Modifiers::NONE);
    h.run_frame();
    h.inject_pointer_move(p2);
    h.run_frame();
    h.inject_pointer_button(p2, PointerButton::Primary, false, Modifiers::NONE);
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        format!("\x1b[<0;{px1};{py1}M\x1b[<32;{px2};{py2}M\x1b[<0;{px2};{py2}m")
    );
    h.runtime.input_mut().clear_written();

    // Disabling ?1016 switches back to legacy cell encoding.
    h.runtime.input_mut().push_str("\x1b[?1016l");
    h.run_frame();
    h.inject_pointer_move(p1);
    h.inject_pointer_button(p1, PointerButton::Primary, true, Modifiers::NONE);
    h.inject_pointer_button(p1, PointerButton::Primary, false, Modifiers::NONE);
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_bytes(),
        &[0x1B, b'[', b'M', 32, 35, 34, 0x1B, b'[', b'M', 35, 35, 34]
    );
    h.runtime.input_mut().clear_written();

    // Disable mouse tracking and enable alt-scroll (`?1007h`): wheel sends Up/Down arrows on alt screen.
    h.runtime.input_mut().push_str("\x1b[?1002l\x1b[?1007h");
    h.run_frame();
    h.inject_pointer_move(p1);
    h.inject_scroll(MouseWheelUnit::Line, Vec2::new(0.0, 1.0), Modifiers::NONE);
    h.inject_scroll(MouseWheelUnit::Line, Vec2::new(0.0, -1.0), Modifiers::NONE);
    h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b[A\x1b[B");
}

#[test]
fn t3_osc_01_osc5_osc105_with_dec2026_sync_latch_and_palette_epoch() {
    let cfg = TerminalConfig {
        cursor_blink: false,
        idle_poll_interval_ms: 0,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.focus_terminal();
    let default_bg = rgb_to_c32(h.config.theme.scheme.background);
    let synced_bg = Color32::from_rgb(0x11, 0x22, 0x33);

    let base_frame = h.run_frame();
    let epoch0 = h.runtime.terminal().palette_epoch();
    assert!(
        base_frame
            .inspect_shapes()
            .has_rect_or_mesh_color(default_bg)
    );

    // Enter ?2026h and mutate OSC 5, OSC 4, OSC 11 inside synchronized block.
    h.runtime
        .input_mut()
        .push_str("\x1b[?2026h\x1b]5;0;#ff00ff\x07\x1b]4;1;#00ff00\x07\x1b]11;#112233\x07");
    let latched = h.run_frame();
    assert!(h.runtime.terminal().is_synchronized_output());
    assert!(h.runtime.terminal().palette_epoch() > epoch0);
    // Snapshot freezes pre-sync `palette_epoch`, `special_colors`, and rendered background!
    {
        let snap_latched = h.runtime.terminal().active_snapshot();
        assert_eq!(snap_latched.palette_epoch, epoch0);
        assert_eq!(snap_latched.special_colors[0], None);
    }
    let latched_shapes = latched.inspect_shapes();
    assert!(latched_shapes.has_rect_or_mesh_color(default_bg));
    assert!(!latched_shapes.has_rect_or_mesh_color(synced_bg));

    // End sync (`?2026l`) atomically exposes updated palette_epoch, OSC 5 color, and OSC 11 background.
    h.runtime.input_mut().push_str("\x1b[?2026l");
    let unlatched = h.run_frame();
    assert!(!h.runtime.terminal().is_synchronized_output());
    {
        let snap_after = h.runtime.terminal().active_snapshot();
        assert!(snap_after.palette_epoch > epoch0);
        assert_eq!(snap_after.special_colors[0], Some([0xff, 0x00, 0xff]));
        assert_eq!(snap_after.resolve_indexed_color(1), [0x00, 0xff, 0x00]);
    }
    assert!(unlatched.inspect_shapes().has_rect_or_mesh_color(synced_bg));

    // OSC 105 inside another ?2026h latch freezes reset until watchdog deadline expires (> 250ms).
    let epoch_mid = h.runtime.terminal().palette_epoch();
    h.runtime.input_mut().push_str("\x1b[?2026h\x1b]105\x07");
    h.advance_time(0.05);
    h.run_frame();
    assert_eq!(
        h.runtime.terminal().active_snapshot().special_colors[0],
        Some([0xff, 0x00, 0xff])
    );

    h.advance_time(0.30);
    h.run_frame();
    assert!(!h.runtime.terminal().is_synchronized_output());
    assert!(h.runtime.terminal().palette_epoch() > epoch_mid);
    assert_eq!(
        h.runtime.terminal().active_snapshot().special_colors[0],
        None
    );
}

// ===========================================================================
// Items #3 & #4 — Tier 4 Realistic Application Workloads (2 Workloads)
// ===========================================================================

#[test]
fn t4_workload_01_tui_spreadsheet_editor_declrmm_deccra_decsca() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(640.0, 384.0);
    h.focus_terminal();
    let warm = h.run_frame();
    let cols = warm.terminal_response.cols;
    let rows = warm.terminal_response.rows;
    assert!(cols >= 40 && rows >= 12);

    // 1. Set window title/icon and render protected header + left row-number gutter with DECSCA 1.
    let mut sheet = String::from("\x1b]1;SheetIcon\x07\x1b]2;Ledger.xlsx\x07\x1b[1\"q");
    sheet.push_str("\x1b[1;1H\x1b[1;44;37m  ID  | COL_A   | COL_B   | COL_C   \x1b[0m\r\n");
    sheet.push_str("\x1b*0\x1bn"); // G2 = DEC Special Graphics, LS2 active
    sheet.push_str("qqqqqqnqqqqqqqqqnqqqqqqqqqnqqqqqqqqq");
    sheet.push('\x0f'); // SI back to G0 ASCII
    for r in 3..=(rows - 2) {
        sheet.push_str(&format!("\x1b[{r};1HR{r:02}  \x1bnx\x0f"));
    }
    sheet.push_str("\x1b[0\"q"); // disable DECSCA for mutable data cells

    // 2. Confine data viewport to rows 3..=(rows-2) and cols 7..=(cols-2) with DECSTBM + DECLRMM.
    let bot = rows - 2;
    let right = cols - 2;
    sheet.push_str(&format!("\x1b[3;{bot}r\x1b[?69h\x1b[7;{right}s"));

    // 3. Fill empty data cells with '.' (`DECFRA`), write cell values using XTPUSHSGR/XTPOPSGR.
    sheet.push_str(&format!("\x1b[46;3;7;{bot};{right}$x"));
    sheet.push_str("\x1b[3;8H\x1b[33;1m\x1b[#{\x1b[31mTEMP\x1b[#}100.5\x1b[0m");
    sheet.push_str("\x1b[4;8H200.0");

    // 4. Copy cell block (rows 3..=4, cols 8..=16) to (row=5, col=18) via DECCRA.
    sheet.push_str("\x1b[3;8;4;16;1;5;18;1$v");

    // 5. Scroll data viewport horizontally with SL (`CSI 2 SP @`), SR (`CSI 1 SP A`), DECBI, DECFI.
    sheet.push_str("\x1b[2 @\x1b[1 A\x1b[3;7H\x1b6");
    sheet.push_str(&format!("\x1b[3;{right}H\x1b9"));

    // 6. Query geometry & title/icon via XTWINOPS 11, 13, 18, 19, 20, 21.
    sheet.push_str("\x1b[11t\x1b[13t\x1b[18t\x1b[19t\x1b[20t\x1b[21t");

    h.runtime.input_mut().push_str(sheet);
    let rendered = h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        format!(
            "\x1b[1t\x1b[3;0;0t\x1b[8;{rows};{cols}t\x1b[9;{rows};{cols}t\x1b]LSheetIcon\x1b\\\x1b]lLedger.xlsx\x1b\\"
        )
    );
    h.runtime.input_mut().clear_written();

    let insp = rendered.inspect_shapes();
    assert!(!insp.text_mesh_vertices().is_empty());
    assert!(
        h.runtime
            .terminal()
            .visible_line_text(0)
            .as_deref()
            .is_some_and(|l| l.contains("COL_A"))
    );
    assert!(
        h.runtime
            .terminal()
            .visible_line_text(2)
            .as_deref()
            .is_some_and(|l| l.contains("100.5"))
    );

    // Protected header & gutter stay intact under DECSERA and DECSED 2 across entire viewport.
    h.runtime
        .input_mut()
        .push_str(format!("\x1b[1;1;{rows};{cols}${{"));
    h.run_frame();
    h.runtime.input_mut().push_str("\x1b[?2J");
    h.run_frame();
    let term = h.runtime.terminal();
    let row0 = term.visible_line_text(0).expect("header row");
    assert!(row0.contains("COL_A"));
    assert!(term.visible_line_text(1).expect("border").contains('─'));
    assert!(
        term.visible_line_text(2)
            .expect("gutter row 2")
            .starts_with("R03  │")
    );
    // Unprotected data cell area was selectively cleared.
    assert_eq!(term.cell_at(8, 2).expect("cleared data cell").ch, ' ');
}

#[test]
fn t4_workload_02_modern_ci_build_runner_osc_suite() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(640.0, 360.0);
    h.focus_terminal();
    let hover_pos = h.cell_center(4, 4).expect("cell (4,4)");
    h.runtime
        .terminal_mut()
        .set_osc52_read_policy(Osc52ReadPolicy::Allow);

    // Phase 1: Document URI, title, custom bold special color, OSC 66 header, indeterminate/progress,
    // wait/progress cursor, and ?2004h / modifyOtherKeys=2 setup.
    h.runtime.input_mut().push_str(concat!(
        "\x1b]6;file://localhost/workspace/Cargo.toml\x07",
        "\x1b]2;cargo test --workspace\x07",
        "\x1b]5;0;#ffaa00\x07",
        "\x1b]66;s=2;BUILD\x07\r\n\r\n",
        "\x1b]9;4;3\x07",
        "\x1b]9;4;1;25\x07",
        "\x1b]9;4;1;100\x07",
        "\x1b]22;progress\x07",
        "\x1b[?2004h",
        "\x1b[>4;2m",
    ));
    h.inject_pointer_move(hover_pos);
    let build_frame = h.run_frame();

    assert_eq!(
        h.runtime.terminal().document_uri(),
        Some("file://localhost/workspace/Cargo.toml")
    );
    assert_eq!(h.runtime.terminal().title(), "cargo test --workspace");
    assert_eq!(
        h.runtime.terminal().special_colors().bold,
        Some([0xff, 0xaa, 0x00])
    );
    assert_eq!(
        build_frame.platform_output.cursor_icon,
        CursorIcon::Progress
    );
    assert!(
        build_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::DocumentChanged(Some(
                "file://localhost/workspace/Cargo.toml".to_owned()
            )))
    );
    assert!(
        build_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::TitleChanged(
                "cargo test --workspace".to_owned()
            ))
    );
    assert!(
        build_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    assert!(
        build_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::Progress {
                state: 3,
                progress: 0
            })
    );
    assert!(
        build_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::Progress {
                state: 1,
                progress: 100
            })
    );
    assert!(
        build_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::MouseShape("progress".to_owned()))
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("BUILD")
    );
    let build_insp = build_frame.inspect_shapes();
    assert!(!build_insp.text_mesh_vertices().is_empty());
    assert!(
        build_insp
            .text_mesh_bounds()
            .is_some_and(|b| b.height() > build_frame.terminal_response.cell_size.y)
    );

    // Phase 2: Build completes -> clear progress, restore default cursor, emit chunked base64 OSC 99,
    // and request OSC 52 clipboard read.
    h.runtime.input_mut().push_str(concat!(
        "\x1b]9;4;0\x07",
        "\x1b]22;default\x07",
        "\x1b]99;i=ci1:p=title:u=1:d=0;CI Runner\x1b\\",
        "\x1b]99;i=ci1:p=body:e=1:d=1;QWxsIHBhc3NlZA==\x1b\\",
        "\x1b]52;c;?\x07",
    ));
    let done_frame = h.run_frame();
    assert_eq!(done_frame.platform_output.cursor_icon, CursorIcon::Default);
    assert!(
        done_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::Progress {
                state: 0,
                progress: 0
            })
    );
    assert!(
        done_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::Notification {
                title: "CI Runner".to_owned(),
                body: "All passed".to_owned(),
            })
    );
    assert!(
        done_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::ClipboardReadRequested {
                selection: ClipboardSelection::Clipboard
            })
    );

    // Phase 3: Host fulfills OSC 52 read, user pastes payload with injected `\x1b[201~`,
    // and presses Ctrl+Enter under `modifyOtherKeys=2`.
    h.runtime
        .terminal_mut()
        .provide_clipboard_read(ClipboardSelection::Clipboard, "ci-token");
    assert_eq!(
        h.runtime.terminal_mut().drain_outgoing(),
        b"\x1b]52;c;Y2ktdG9rZW4=\x1b\\"
    );

    h.inject_paste("cargo build\x1b[201~; rm -rf /\n");
    h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b[200~cargo build; rm -rf /\n\x1b[201~"
    );

    #[cfg(feature = "kitty-keyboard")]
    {
        let ctrl_enter = KittyKeyEvent {
            key: KittyKey::Enter,
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                ctrl: true,
                ..Default::default()
            },
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        let mut encoded = Vec::new();
        assert!(
            h.runtime
                .terminal()
                .encode_key_event_into(&ctrl_enter, &mut encoded)
        );
        assert_eq!(encoded, b"\x1b[27;5;13~");
    }
}
