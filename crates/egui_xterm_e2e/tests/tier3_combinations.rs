//! Tier 3 — Cross-Feature Pairwise Combination E2E Tests (`T3-01`..`T3-26`, 26 specs + 4 M1+M2 core companions = 30 tests).
//!
//! Exercises pairwise interactions across core (`egui_xterm_core`), optional features
//! (`kitty-keyboard`, `search`, `shell-integration`, `hyperlinks`, `serde`, `metrics`,
//! `vector-glyphs`), `egui_xterm` headless runtime/rendering, and native PTY integration.

// ===========================================================================
// Ungated M1 + M2 Core Pairwise Tests (6 tests: T3-01..T3-06 core)
// ===========================================================================

mod stage_m2_core {
    use std::cell::RefCell;
    use std::rc::Rc;

    use egui_xterm::TerminalInput;
    use egui_xterm_core::{CoreConfig, PackedColor, SelectionKind, Terminal, UnderlineStyle};
    use egui_xterm_e2e::{LocalRcInput, ScriptedInput};

    fn drain_into_terminal(src: &mut impl TerminalInput, term: &mut Terminal, buf_size: usize) {
        let mut buf = vec![0u8; buf_size];
        loop {
            match src.poll_read(&mut buf) {
                Ok(0) => break,
                Ok(n) => term.advance(&buf[..n]),
                Err(err) if err.is_closed() => break,
                Err(err) => panic!("unexpected poll_read error: {err}"),
            }
        }
    }

    #[test]
    fn t3_01_core_f2_f3_chunked_utf8_and_csi_truecolor() {
        let payload = "你好🚀\x1b[38;2;12;34;56mOK";

        // 1-byte chunks via ScriptedInput
        let mut scripted = ScriptedInput::new();
        scripted.push_chunked(payload.as_bytes(), 1);
        let mut term1 = Terminal::new(40, 5, CoreConfig::default());
        drain_into_terminal(&mut scripted, &mut term1, 1);

        // 3-byte chunks via !Send LocalRcInput
        let shared = Rc::new(RefCell::new(ScriptedInput::new()));
        shared.borrow_mut().push_chunked(payload.as_bytes(), 3);
        let mut local = LocalRcInput::new(Rc::clone(&shared));
        let mut term2 = Terminal::new(40, 5, CoreConfig::default());
        drain_into_terminal(&mut local, &mut term2, 3);

        for term in [&term1, &term2] {
            let line = term.visible_line_text(0).expect("row 0 exists");
            assert_eq!(line, "你好🚀OK");
            assert!(!line.contains('\u{FFFD}'));
            assert_eq!(term.cell_at(0, 0).expect("col 0").ch, '你');
            assert_eq!(term.cell_at(2, 0).expect("col 2").ch, '好');
            assert_eq!(term.cell_at(4, 0).expect("col 4").grapheme, "🚀");

            let o = term.cell_at(6, 0).expect("cell O");
            let k = term.cell_at(7, 0).expect("cell K");
            assert_eq!(o.ch, 'O');
            assert_eq!(k.ch, 'K');
            assert_eq!(o.fg, PackedColor::rgb(12, 34, 56));
            assert_eq!(k.fg, PackedColor::rgb(12, 34, 56));
        }
    }

    #[test]
    fn t3_02_f3_f4_grapheme_underline_and_partial_erasure() {
        let mut term = Terminal::new(40, 6, CoreConfig::default());

        // Row 0: ZWJ emoji + SGR 58 curly underline combining mark + "AB", then CSI K (EL 0) at col 2.
        term.advance("👨‍👩‍👧‍👦\x1b[4:3;58:2::255:0:128me\u{0301}AB\x1b[0m".as_bytes());
        term.advance(b"\x1b[1;3H\x1b[K");
        assert_eq!(term.cell_at(0, 0).expect("emoji").grapheme, "👨‍👩‍👧‍👦");
        for col in 2..5u16 {
            let cleared = term.cell_at(col, 0).expect("cleared col");
            assert_eq!(cleared.ch, ' ');
            assert_eq!(cleared.grapheme, " ");
            assert_eq!(cleared.underline_color, None);
            assert_eq!(cleared.underline_style, UnderlineStyle::None);
        }

        // Row 1: SGR 58 combining + "XY" + SGR 0 "Z", then CSI 1 K (EL 1) at col 1 (clears cols 0..=1).
        term.advance(
            "\x1b[2;1H\x1b[4:1;58:2::10:20:30me\u{0301}X\x1b[4:1;58:2::10:20:30mY\x1b[0mZ"
                .as_bytes(),
        );
        term.advance(b"\x1b[2;2H\x1b[1K");
        for col in 0..=1u16 {
            let erased = term.cell_at(col, 1).expect("EL 1 erased");
            assert_eq!(erased.ch, ' ');
            assert_eq!(erased.grapheme, " ");
            assert_eq!(erased.underline_color, None);
            assert_eq!(erased.underline_style, UnderlineStyle::None);
        }
        let y = term.cell_at(2, 1).expect("surviving Y");
        assert_eq!(y.ch, 'Y');
        assert_eq!(y.underline_color, Some(PackedColor::rgb(10, 20, 30)));
        assert_eq!(y.underline_style, UnderlineStyle::Single);
        let z = term.cell_at(3, 1).expect("surviving Z");
        assert_eq!(z.ch, 'Z');
        assert_eq!(z.underline_color, None);

        // Row 2: wide ZWJ emoji + styled "CD", then DCH 2 at col 0 deletes wide emoji (2 cols).
        term.advance("\x1b[3;1H\x1b[4:2;58:2::255:0:128m👨‍👩‍👧‍👦CD\x1b[0m".as_bytes());
        term.advance(b"\x1b[3;1H\x1b[2P");
        let c = term.cell_at(0, 2).expect("shifted C");
        let d = term.cell_at(1, 2).expect("shifted D");
        assert_eq!(c.ch, 'C');
        assert_eq!(c.underline_color, Some(PackedColor::rgb(255, 0, 128)));
        assert_eq!(c.underline_style, UnderlineStyle::Double);
        assert_eq!(d.ch, 'D');
        assert_eq!(d.underline_color, Some(PackedColor::rgb(255, 0, 128)));
        for col in 2..6u16 {
            let tail = term.cell_at(col, 2).expect("vacated tail");
            assert_eq!(tail.ch, ' ');
            assert_eq!(tail.underline_color, None);
            assert_eq!(tail.underline_style, UnderlineStyle::None);
        }

        // Row 3: DCH 1 (\x1b[1P) targeting wide ZWJ emoji lead/spacer cleans wide cell and preserves SGR 58 neighbors.
        term.advance("\x1b[4;1H\x1b[4:3;58:2::255:0:128m👨‍👩‍👧‍👦CD\x1b[0m".as_bytes());
        term.advance(b"\x1b[4;2H\x1b[1P");
        let cleared_lead = term.cell_at(0, 3).expect("cleared wide lead");
        assert_eq!(cleared_lead.ch, ' ');
        assert!(!cleared_lead.is_wide && !cleared_lead.is_wide_spacer);
        assert_eq!(cleared_lead.underline_color, None);
        assert_eq!(cleared_lead.underline_style, UnderlineStyle::None);
        let shifted_c = term.cell_at(1, 3).expect("shifted C after DCH 1");
        assert_eq!(shifted_c.ch, 'C');
        assert_eq!(
            shifted_c.underline_color,
            Some(PackedColor::rgb(255, 0, 128))
        );
        assert_eq!(shifted_c.underline_style, UnderlineStyle::Curly);
        let shifted_d = term.cell_at(2, 3).expect("shifted D after DCH 1");
        assert_eq!(shifted_d.ch, 'D');
        assert_eq!(
            shifted_d.underline_color,
            Some(PackedColor::rgb(255, 0, 128))
        );
        assert_eq!(shifted_d.underline_style, UnderlineStyle::Curly);

        // Row 4: EL 0 (\x1b[K) landing on the spacer cell of a 2-col wide glyph clears the whole wide glyph and keeps left SGR 58 neighbor.
        term.advance(
            "\x1b[5;1H\x1b[4:1;58:2::10:20:30mA\x1b[4:2;58:2::40:50:60m中文\x1b[4:3;58:2::70:80:90mB\x1b[0m"
                .as_bytes(),
        );
        term.advance(b"\x1b[5;3H\x1b[K");
        let surv_a = term.cell_at(0, 4).expect("surviving A");
        assert_eq!(surv_a.ch, 'A');
        assert_eq!(surv_a.underline_color, Some(PackedColor::rgb(10, 20, 30)));
        assert_eq!(surv_a.underline_style, UnderlineStyle::Single);
        for col in 1..6u16 {
            let cleared = term.cell_at(col, 4).expect("EL 0 across wide boundary");
            assert_eq!(cleared.ch, ' ');
            assert!(!cleared.is_wide && !cleared.is_wide_spacer);
            assert_eq!(cleared.underline_color, None);
            assert_eq!(cleared.underline_style, UnderlineStyle::None);
        }

        // Row 5: EL 1 (\x1b[1K) landing on the lead cell of a 2-col wide ZWJ emoji clears its spacer too and keeps right SGR 58 neighbor.
        term.advance(
            "\x1b[6;1H\x1b[4:1;58:2::10:20:30mA\x1b[4:2;58:2::40:50:60m👨‍👩‍👧‍👦\x1b[4:3;58:2::70:80:90mB\x1b[0m"
                .as_bytes(),
        );
        term.advance(b"\x1b[6;2H\x1b[1K");
        for col in 0..=2u16 {
            let cleared = term.cell_at(col, 5).expect("EL 1 across wide boundary");
            assert_eq!(cleared.ch, ' ');
            assert!(!cleared.is_wide && !cleared.is_wide_spacer);
            assert_eq!(cleared.underline_color, None);
            assert_eq!(cleared.underline_style, UnderlineStyle::None);
        }
        let surv_b = term.cell_at(3, 5).expect("surviving B");
        assert_eq!(surv_b.ch, 'B');
        assert_eq!(surv_b.underline_color, Some(PackedColor::rgb(70, 80, 90)));
        assert_eq!(surv_b.underline_style, UnderlineStyle::Curly);
    }

    #[test]
    fn t3_03_core_f4_f6_wide_cjk_emoji_sgr58_reflow() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance("0123456789\x1b[4:3;58:2::255:0:128m中文测试\x1b[0m👨‍👩‍👧‍👦e\u{0301}".as_bytes());
        let expected_full = term.full_text();

        // Narrow to 11 cols: col 10 has 1 remaining column, so '中' must wrap atomically to row 1.
        term.resize(11, 24);
        let pad = term.cell_at(10, 0).expect("row 0 col 10");
        assert_eq!(pad.ch, ' ');
        assert!(!pad.is_wide);
        assert!(!pad.is_wide_spacer);

        let zhong = term.cell_at(0, 1).expect("row 1 col 0");
        assert_eq!(zhong.ch, '中');
        assert!(zhong.is_wide);
        assert_eq!(zhong.underline_style, UnderlineStyle::Curly);
        assert_eq!(zhong.underline_color, Some(PackedColor::rgb(255, 0, 128)));
        assert!(term.cell_at(1, 1).expect("spacer").is_wide_spacer);
        assert_eq!(term.full_text(), expected_full);

        // Widen back to 80 cols: all graphemes and CellExtra underline attributes remain intact.
        term.resize(80, 24);
        assert_eq!(term.full_text(), expected_full);
        assert_eq!(term.cell_at(10, 0).expect("restored 中").ch, '中');
        assert_eq!(
            term.cell_at(10, 0).expect("restored 中").underline_color,
            Some(PackedColor::rgb(255, 0, 128))
        );
        assert_eq!(term.cell_at(18, 0).expect("restored emoji").grapheme, "👨‍👩‍👧‍👦");
        assert_eq!(
            term.cell_at(20, 0).expect("restored combining").grapheme,
            "e\u{0301}"
        );
    }

    #[test]
    fn t3_04_f5_f6_scrollback_cap_and_anchor_across_reflow() {
        let cfg = CoreConfig {
            scrollback_limit: 40,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(80, 10, cfg);
        let filler = "x".repeat(65);
        for i in 0..100 {
            if i > 0 {
                term.advance(b"\r\n");
            }
            term.advance(format!("L{i:03}_{filler}").as_bytes());
        }
        assert_eq!(term.scrollback_len(), 40);

        term.scroll_display(15);
        assert_eq!(term.display_offset(), 15);
        assert!(
            term.visible_line_text(0)
                .as_deref()
                .is_some_and(|l| l.starts_with("L075_")),
            "expected L075_ at top after scroll_display(15), got {:?}",
            term.visible_line_text(0)
        );

        term.resize(35, 10);
        assert_eq!(term.size(), (35, 10));
        assert_eq!(term.scrollback_len(), 40);
        assert_eq!(term.display_offset(), 40);
        assert!(
            term.visible_line_text(0)
                .as_deref()
                .is_some_and(|l| l.starts_with("L075_")),
            "expected L075_ anchored at top after resize(35, 10), got {:?}",
            term.visible_line_text(0)
        );
        let narrowed = term.visible_lines();
        assert_eq!(narrowed.len(), 10);
        assert!(narrowed.iter().any(|l| !l.is_empty()));

        term.resize(80, 10);
        assert_eq!(term.size(), (80, 10));
        assert_eq!(term.scrollback_len(), 15);
        assert_eq!(term.display_offset(), 15);
        assert!(
            term.visible_line_text(0)
                .as_deref()
                .is_some_and(|l| l.starts_with("L075_")),
            "expected L075_ anchored at top after resize(80, 10), got {:?}",
            term.visible_line_text(0)
        );

        term.scroll_to_bottom();
        assert_eq!(term.display_offset(), 0);
        let bottom = term.visible_lines();
        assert_eq!(
            bottom.last().map(String::as_str),
            Some(format!("L099_{filler}").as_str())
        );
    }

    #[test]
    fn t3_05_core_f6_f10_selection_tracks_reflow() {
        let mut term = Terminal::new(80, 10, CoreConfig::default());
        let p1 = format!("PARA1_{}", "a".repeat(76));
        let p2 = format!("PARA2_HEAD_{}_TAIL", "b".repeat(72));
        let p3 = "PARA3_END";
        term.advance(format!("{p1}\r\n{p2}\r\n{p3}").as_bytes());

        // Paragraph 2 starts on physical row 2 ("PARA2_HEAD_bbbb...") and wraps onto row 3 ("...bbb_TAIL").
        // Select from col 6 ('H' of HEAD) on row 2 through 'L' of _TAIL on row 3.
        let row3_text = term.visible_line_text(3).expect("wrapped row 3");
        let end_col = row3_text.len() - 1;
        term.selection_start(2, 6, SelectionKind::Stream);
        term.selection_update(3, end_col);
        term.selection_finish();

        let expected = format!("HEAD_{}_TAIL", "b".repeat(72));
        assert_eq!(term.selection_text().as_deref(), Some(expected.as_str()));

        term.resize(45, 10);
        assert_eq!(term.selection_text().as_deref(), Some(expected.as_str()));

        term.resize(80, 10);
        assert_eq!(term.selection_text().as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn t3_06_core_f7_f9_alt_screen_decstbm_and_sync_output() {
        let mut term = Terminal::new(40, 8, CoreConfig::default());
        term.advance_with_time(b"MAIN", 0.5);

        term.advance_with_time(b"\x1b[?1049hPRE_SYNC", 0.6);
        assert!(term.is_alt_screen());
        assert_eq!(
            term.render_snapshot_line_text(0),
            Some("PRE_SYNC".to_owned())
        );

        term.advance_with_time(
            b"\x1b[?2026h\x1b[2;5r\x1b[2;1HR2\r\nR3\r\nR4\r\nR5\r\nR6",
            1.0,
        );
        assert!(term.is_synchronized_output());
        assert_eq!(
            term.render_snapshot_line_text(0),
            Some("PRE_SYNC".to_owned())
        );

        term.advance_with_time(b"\x1b[?2026l", 1.05);
        assert!(!term.is_synchronized_output());
        assert_eq!(
            term.render_snapshot_line_text(0).as_deref(),
            Some("PRE_SYNC")
        );
        assert_eq!(term.render_snapshot_line_text(1), Some("R3".to_owned()));
        assert_eq!(term.render_snapshot_line_text(4), Some("R6".to_owned()));
        assert_eq!(term.render_snapshot_line_text(5).as_deref(), Some(""));
        assert_eq!(term.scrollback_len(), 0);

        term.advance_with_time(b"\x1b[?1049l", 1.1);
        assert!(!term.is_alt_screen());
        assert_eq!(term.visible_line_text(0), Some("MAIN".to_owned()));
    }
}

// ===========================================================================
// M3 Core Feature Pairwise Tests (`#[cfg(feature = "...")]`)
// ===========================================================================

#[cfg(feature = "kitty-keyboard")]
#[test]
fn t3_07_f7_f14_alt_screen_kitty_keyboard_stacks() {
    use egui_xterm_core::{CoreConfig, KittyKeyboardFlags, Terminal};

    let mut term = Terminal::new(80, 24, CoreConfig::default());
    term.advance(b"\x1b[>1u\x1b[?1049h\x1b[>15u\x1b[?u\x1b[?1049l\x1b[?u");

    assert_eq!(term.drain_outgoing(), b"\x1b[?15u\x1b[?1u");
    assert_eq!(
        term.kitty_keyboard_flags(),
        KittyKeyboardFlags::from_bits_retain(1)
    );
}

#[cfg(feature = "search")]
#[test]
fn t3_11_f11_f5_search_scrollback_eviction_and_jump() {
    use egui_xterm_core::{CoreConfig, SearchQuery, Terminal};

    let cfg = CoreConfig {
        scrollback_limit: 64,
        ..CoreConfig::default()
    };
    let mut term = Terminal::new(40, 6, cfg);
    for i in 0..120 {
        let line = if i == 5 {
            format!("ROW_{i:03} NEEDLE_OLD\r\n")
        } else if i == 65 {
            format!("ROW_{i:03} NEEDLE_ALPHA\r\n")
        } else {
            format!("ROW_{i:03}\r\n")
        };
        term.advance(line.as_bytes());
    }
    assert!(term.scrollback_len() <= 64);

    let old_matches = term
        .search(&SearchQuery::literal("NEEDLE_OLD"))
        .expect("search old");
    assert!(old_matches.is_empty(), "evicted NEEDLE_OLD must not match");

    let alpha_matches = term
        .search(&SearchQuery::literal("NEEDLE_ALPHA"))
        .expect("search alpha");
    assert_eq!(alpha_matches.len(), 1);
    let jumped = term.next_search_match().expect("jump to NEEDLE_ALPHA");
    assert_eq!(jumped.start_col, 8);
    assert!(term.display_offset() > 0);
    assert!(
        term.visible_lines()
            .iter()
            .any(|l| l.contains("NEEDLE_ALPHA"))
    );
}

#[cfg(feature = "shell-integration")]
mod stage_m3_shell {
    use egui_xterm_core::{CoreConfig, SelectionKind, Terminal};

    #[test]
    fn t3_14_f13_f5_osc133_eviction_and_jump_to_prompt() {
        let cfg = CoreConfig {
            scrollback_limit: 50,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(40, 4, cfg);
        for i in 0..80 {
            term.advance(
                format!(
                    "\x1b]133;A\x07P{i:02}\x1b]133;B\x07cmd\r\n\x1b]133;C\x07out{i:02}\r\n\x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }

        assert!(term.scrollback_len() <= 50);
        let zones = term.shell_zones();
        assert!(!zones.is_empty());
        assert!(zones.len() < 240, "evicted OSC 133 zones must be pruned");

        assert!(term.jump_to_prompt(-1));
        assert!(term.display_offset() > 0);
        assert!(term.display_offset() <= term.scrollback_len());
        assert!(
            !term.visible_lines().iter().any(|l| l.contains("P00")),
            "evicted prompt P00 must never reappear"
        );
    }

    #[test]
    fn t3_15_f13_f10_command_output_extraction_vs_selection() {
        let mut term = Terminal::new(80, 10, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cat out\r\n\x1b]133;C\x07line1\r\nline2\r\n\x1b]133;D;0\x07",
        );

        let extracted = term.command_output_at(1).expect("command output exists");
        assert_eq!(extracted, "line1\nline2");
        assert!(!extracted.contains("cat out"));

        // Place drag-selection endpoints directly on non-space characters ('l' of line1 .. '2' of line2).
        term.selection_start(1, 0, SelectionKind::Stream);
        term.selection_update(2, 4);
        term.selection_finish();
        assert_eq!(term.selection_text().as_deref(), Some(extracted.as_str()));
    }
}

// ===========================================================================
// M4 Headless Runtime & Rendering Pairwise Tests
// ===========================================================================

mod stage_m4_core {
    use std::cell::RefCell;
    use std::rc::Rc;

    use egui::{Color32, Key, Modifiers, PointerButton};
    use egui_xterm::{ScaleMode, TerminalConfig};
    use egui_xterm_core::PackedColor;
    use egui_xterm_e2e::{
        HeadlessTermHarness, LocalRcInput, MultiPaneHarness, ScriptedInput, ShapeInspection,
    };

    #[test]
    fn t3_01_runtime_f2_f3_chunked_utf8_and_csi_truecolor() {
        let payload = "你好🚀\x1b[38;2;12;34;56mOK";

        let mut h1 = HeadlessTermHarness::scripted();
        for chunk in payload.as_bytes().chunks(1) {
            h1.runtime.input_mut().push_bytes(chunk);
            let _ = h1.run_frame();
        }

        let shared = Rc::new(RefCell::new(ScriptedInput::new()));
        let mut h2 = HeadlessTermHarness::new(LocalRcInput::new(Rc::clone(&shared)));
        for chunk in payload.as_bytes().chunks(3) {
            shared.borrow_mut().push_bytes(chunk);
            let _ = h2.run_frame();
        }

        for term in [h1.runtime.terminal(), h2.runtime.terminal()] {
            let line = term.visible_line_text(0).expect("row 0");
            assert_eq!(line, "你好🚀OK");
            assert!(!line.contains('\u{FFFD}'));
            let o = term.cell_at(6, 0).expect("cell O");
            assert_eq!(o.fg, PackedColor::rgb(12, 34, 56));
        }

        let out1 = h1.run_frame();
        let insp1 = out1.inspect_shapes();
        assert!(insp1.contains_text("你好🚀OK"));
        assert!(insp1.has_text_fg_color(Color32::from_rgb(12, 34, 56)));
    }

    #[test]
    fn t3_03_runtime_f4_f6_wide_cjk_emoji_sgr58_reflow() {
        let cfg = TerminalConfig {
            scale_mode: ScaleMode::ReflowGrid,
            resize_debounce_secs: 0.0,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        let warm = h.run_frame().terminal_response;
        let cw = warm.cell_size.x;
        let ch = warm.cell_size.y;

        h.set_viewport_size(80.0 * cw + 1.0, 24.0 * ch + 1.0);
        let r80 = h.run_frame().terminal_response;
        assert_eq!(r80.cols, 80);

        h.runtime
            .input_mut()
            .push_str("0123456789\x1b[4:3;58:2::255:0:128m中文测试\x1b[0m👨‍👩‍👧‍👦e\u{0301}");
        let _ = h.run_frame();
        let expected_full = h.runtime.terminal().full_text();

        h.set_viewport_size(11.0 * cw + 0.5, 24.0 * ch + 1.0);
        let r11 = h.run_frame().terminal_response;
        assert_eq!(r11.cols, 11);
        let pad = h.runtime.terminal().cell_at(10, 0).expect("row 0 col 10");
        assert_eq!(pad.ch, ' ');
        assert!(!pad.is_wide);
        assert_eq!(
            h.runtime.terminal().cell_at(0, 1).expect("row 1 col 0").ch,
            '中'
        );
        assert_eq!(
            h.runtime
                .terminal()
                .cell_at(0, 1)
                .expect("row 1 col 0")
                .underline_color,
            Some(PackedColor::rgb(255, 0, 128))
        );
        assert_eq!(h.runtime.terminal().full_text(), expected_full);

        h.set_viewport_size(80.0 * cw + 1.0, 24.0 * ch + 1.0);
        let out_back = h.run_frame();
        assert_eq!(out_back.terminal_response.cols, 80);
        assert_eq!(h.runtime.terminal().full_text(), expected_full);
        assert!(out_back.inspect_shapes().contains_text("中文测试"));
    }

    #[test]
    fn t3_05_runtime_f6_f10_drag_select_reflow_and_copy() {
        let cfg = TerminalConfig {
            scale_mode: ScaleMode::ReflowGrid,
            resize_debounce_secs: 0.0,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        let warm = h.run_frame().terminal_response;
        let cw = warm.cell_size.x;
        let ch = warm.cell_size.y;

        h.set_viewport_size(80.0 * cw + 1.0, 10.0 * ch + 1.0);
        let r80 = h.run_frame().terminal_response;
        assert_eq!(r80.cols, 80);

        let p1 = format!("PARA1_{}", "a".repeat(76));
        let p2 = format!("PARA2_HEAD_{}_TAIL", "b".repeat(72));
        h.runtime
            .input_mut()
            .push_str(format!("{p1}\r\n{p2}\r\nPARA3"));
        h.focus_terminal();

        // Paragraph 2 starts at row 2 col 0 and wraps onto row 3.
        let row3_len = h
            .runtime
            .terminal()
            .visible_line_text(3)
            .expect("row 3")
            .len();
        let end_col = u16::try_from(row3_len - 1).expect("col fits in u16");
        let from = h.cell_center(6, 2).expect("start cell (6, 2)");
        let to = h.cell_center(end_col, 3).expect("end cell");
        h.inject_drag_steps(from, to, PointerButton::Primary, Modifiers::NONE, 4);

        let expected = format!("HEAD_{}_TAIL", "b".repeat(72));
        assert_eq!(
            h.runtime.terminal().selection_text().as_deref(),
            Some(expected.as_str())
        );

        h.set_viewport_size(45.0 * cw + 0.5, 10.0 * ch + 1.0);
        let r45 = h.run_frame().terminal_response;
        assert_eq!(r45.cols, 45);
        assert_eq!(
            h.runtime.terminal().selection_text().as_deref(),
            Some(expected.as_str())
        );

        h.set_viewport_size(80.0 * cw + 1.0, 10.0 * ch + 1.0);
        let r80_back = h.run_frame().terminal_response;
        assert_eq!(r80_back.cols, 80);
        assert_eq!(
            h.runtime.terminal().selection_text().as_deref(),
            Some(expected.as_str())
        );

        h.inject_copy();
        let copy_out = h.run_frame();
        assert_eq!(copy_out.copied_texts(), vec![expected]);
    }

    #[test]
    fn t3_06_runtime_f7_f9_alt_screen_decstbm_and_sync_output() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("MAIN");
        let _ = h.run_frame();

        h.runtime.input_mut().push_str("\x1b[?1049hPRE_SYNC");
        let pre = h.run_frame().inspect_shapes();
        assert!(pre.contains_text("PRE_SYNC"));

        h.runtime.input_mut().push_str(
            "\x1b[?2026h\x1b[2;5r\x1b[HSTATUS_PARTIAL\x1b[2;1HL2\r\nL3\r\nL4\r\nL5\r\nL6",
        );
        for _ in 0..2 {
            h.advance_time(0.02);
            let mid = h.run_frame().inspect_shapes();
            assert!(mid.contains_text("PRE_SYNC"));
            assert!(!mid.contains_text("STATUS_PARTIAL"));
            assert!(!mid.contains_text("L6"));
        }

        h.runtime.input_mut().push_str("\x1b[?2026l");
        let committed = h.run_frame().inspect_shapes();
        assert!(committed.contains_text("STATUS_PARTIAL"));
        assert!(committed.contains_text("L3"));
        assert!(committed.contains_text("L6"));
        assert!(!committed.contains_text("L2"));

        h.runtime.input_mut().push_str("\x1b[?1049l");
        let restored = h.run_frame().inspect_shapes();
        assert!(!h.runtime.terminal().is_alt_screen());
        assert!(restored.contains_text("MAIN"));
        assert_eq!(
            h.runtime.terminal().visible_line_text(0),
            Some("MAIN".to_owned())
        );
    }

    #[test]
    fn t3_08_f8_f17_truecolor_curly_underline_inverse_osc11_render() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str(
            "\x1b]11;#112233\x07\x1b[38:2::10:200:50;48:2::30:10:40mFG_BG\x1b[4:3;58:2::255:200:0mCURLY\x1b[0;7mINV\x1b[0m",
        );
        let out = h.run_frame();
        let insp = out.inspect_shapes();
        let stats = h.render_to_mesh_stats();

        assert!(insp.contains_text("FG_BG"));
        assert!(insp.contains_text("CURLY"));
        assert!(insp.contains_text("INV"));
        assert!(insp.has_rect_or_mesh_color(Color32::from_rgb(0x11, 0x22, 0x33)));
        assert!(insp.has_rect_or_mesh_color(Color32::from_rgb(30, 10, 40)));
        assert!(insp.has_text_fg_color(Color32::from_rgb(10, 200, 50)));
        assert!(
            insp.path_count > 0 || insp.line_count > 0,
            "expected curly underline shapes"
        );
        assert!(stats.primitive_count > 0 && stats.total_vertices > 0);
    }

    #[test]
    fn t3_09_f9_f16_sync_output_watchdog_and_resize_exit() {
        let cfg = TerminalConfig {
            resize_debounce_secs: 0.0,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);

        // Part A: 250ms DEC 2026 watchdog auto-flushes frozen snapshot when 2026 l is absent.
        h.runtime.input_mut().push_str("FRAME_OLD");
        let _ = h.run_frame();
        h.runtime
            .input_mut()
            .push_str("\x1b[?2026h\x1b[2J\x1b[HFRAME_NEW");
        let latched = h.run_frame().inspect_shapes();
        assert!(h.runtime.terminal().is_synchronized_output());
        assert!(latched.contains_text("FRAME_OLD") && !latched.contains_text("FRAME_NEW"));

        h.advance_time(0.10);
        let mid = h.run_frame().inspect_shapes();
        assert!(h.runtime.terminal().is_synchronized_output());
        assert!(mid.contains_text("FRAME_OLD") && !mid.contains_text("FRAME_NEW"));

        h.advance_time(0.20);
        let flushed = h.run_frame().inspect_shapes();
        assert!(!h.runtime.terminal().is_synchronized_output());
        assert!(flushed.contains_text("FRAME_NEW"));

        // Part B: resize() mid-sync cleanly exits Mode 2026 (term/mod.rs:409-411) and renders reflowed content immediately.
        h.runtime.input_mut().push_str("\x1b[2J\x1b[HBEFORE_RESIZE");
        let _ = h.run_frame();
        h.runtime
            .input_mut()
            .push_str("\x1b[?2026h\x1b[2J\x1b[HREFLOWED_MID_SYNC");
        let before_resize = h.run_frame().inspect_shapes();
        assert!(h.runtime.terminal().is_synchronized_output());
        assert!(before_resize.contains_text("BEFORE_RESIZE"));
        assert!(!before_resize.contains_text("REFLOWED_MID_SYNC"));

        h.set_viewport_size(320.0, 240.0);
        let after_resize = h.run_frame();
        assert!(after_resize.terminal_response.resized_this_frame);
        assert!(!h.runtime.terminal().is_synchronized_output());
        assert!(
            after_resize
                .inspect_shapes()
                .contains_text("REFLOWED_MID_SYNC")
        );
    }

    #[test]
    fn t3_10_f10_f18_mouse_tracking_shift_bypass_and_ctrl_c() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime
            .input_mut()
            .push_str("SELECT_ME\r\n\x1b[?1000;1006h");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        // (a) Drag without Shift -> SGR mouse reporting, no local selection.
        let from = h.cell_center(0, 0).expect("cell (0, 0)");
        let to = h.cell_center(5, 0).expect("cell (5, 0)");
        h.inject_drag_steps(from, to, PointerButton::Primary, Modifiers::NONE, 2);
        let mouse_bytes = h.runtime.input_mut().take_written();
        let mouse_str = String::from_utf8_lossy(&mouse_bytes);
        assert!(mouse_str.contains("\x1b[<0;1;1M"), "{mouse_str:?}");
        assert!(mouse_str.contains("\x1b[<0;6;1m"), "{mouse_str:?}");
        assert_eq!(h.runtime.terminal().selection_text(), None);

        // (b) Drag WITH Shift -> bypasses mouse reporting, selects "SELECT_ME", emits settled + CopyText.
        let end_non_space = h.cell_center(8, 0).expect("cell (8, 0)");
        h.inject_drag_steps(
            from,
            end_non_space,
            PointerButton::Primary,
            Modifiers::SHIFT,
            2,
        );
        assert!(h.runtime.input().written_bytes().is_empty());
        assert_eq!(
            h.runtime.terminal().selection_text().as_deref(),
            Some("SELECT_ME")
        );
        assert!(
            h.last_outcome()
                .expect("drag end")
                .settled_selections()
                .iter()
                .any(|s| s == "SELECT_ME")
        );

        h.inject_copy();
        let copy_out = h.run_frame();
        assert_eq!(copy_out.copied_texts(), vec!["SELECT_ME".to_owned()]);

        // (c) Clear selection and press Ctrl+C -> emits \x03.
        h.runtime.terminal_mut().selection_clear();
        h.runtime.input_mut().clear_written();
        h.tap_key(Key::C, Modifiers::CTRL);
        let _ = h.run_frame();
        assert_eq!(h.runtime.input().written_bytes(), b"\x03");
    }

    #[test]
    fn t3_19_f17_f18_ime_preedit_cursor_rect_and_commit() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("prompt> ");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_ime_preedit("nihao", Some(0..5));
        let pre = h.run_frame();
        let term_rect = pre.terminal_response.response.rect;
        let ime = pre
            .platform_output
            .ime
            .expect("PlatformOutput::ime must be Some during preedit");
        assert!(
            term_rect.expand(1.0).contains_rect(ime.cursor_rect),
            "ime.cursor_rect {:?} outside term_rect {:?}",
            ime.cursor_rect,
            term_rect
        );
        assert!(
            term_rect.expand(1.0).contains_rect(ime.rect),
            "ime.rect {:?} outside term_rect {:?}",
            ime.rect,
            term_rect
        );
        assert!(pre.inspect_shapes().contains_text("nihao"));
        assert!(h.runtime.input().written_bytes().is_empty());

        h.inject_ime_commit("你好");
        let committed = h.run_frame();
        assert_eq!(h.runtime.input().written_bytes(), "你好".as_bytes());
        assert!(!committed.inspect_shapes().contains_text("nihao"));
    }

    #[test]
    fn t3_20_f18_f19_stream_512kib_with_interleaved_input_and_paste() {
        let cfg = TerminalConfig {
            read_budget_bytes_per_frame: 64 * 1024,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        let line = "LOG_LINE_0123456789abcdef\r\n";
        let mut payload = Vec::with_capacity(512 * 1024 + line.len());
        while payload.len() < 512 * 1024 {
            payload.extend_from_slice(line.as_bytes());
        }
        h.runtime.input_mut().push_chunked(&payload, 16 * 1024);

        let mut frames = 0usize;
        while !h.runtime.input().is_drained() && frames < 32 {
            match frames {
                0 => h.inject_text("cmd0"),
                1 => h.inject_paste("paste1"),
                2 => h.tap_key(Key::Enter, Modifiers::NONE),
                _ => {}
            }
            let _ = h.run_frame();
            let stats = h.render_to_mesh_stats();
            assert!(
                stats.total_vertices < 100_000,
                "frame {frames} vertex count exploded: {}",
                stats.total_vertices
            );
            frames += 1;
        }

        assert!(h.runtime.input().is_drained());
        assert!(frames >= 8, "drained too fast without budget pacing");
        assert_eq!(h.runtime.input().written_bytes(), b"cmd0paste1\r");
    }

    #[test]
    fn t3_23_f21_f16_multipane_mixed_scale_modes() {
        let mut panes = MultiPaneHarness::new(
            ScriptedInput::from_bytes(format!("LEFT_{}", "x".repeat(70)).as_bytes()),
            ScriptedInput::from_bytes(b"RIGHT_FIXED"),
        );
        panes.left_config.scale_mode = ScaleMode::ReflowGrid;
        panes.left_config.resize_debounce_secs = 0.0;
        panes.right_config.scale_mode = ScaleMode::AutoScaleToFit { cols: 80, rows: 24 };
        panes.right_config.resize_debounce_secs = 0.0;

        panes.set_viewport_size(1200.0, 700.0);
        let (ra1, rb1, _) = panes.run_split_frame();
        let expected_left_full = panes.left.terminal().full_text();
        assert_eq!((rb1.cols, rb1.rows), (80, 24));

        panes.set_viewport_size(800.0, 500.0);
        let (ra2, rb2, _) = panes.run_split_frame();
        assert!(ra2.cols < ra1.cols);
        assert_eq!(panes.left.terminal().full_text(), expected_left_full);
        assert_eq!((rb2.cols, rb2.rows), (80, 24));
        assert!(rb2.effective_font_size < rb1.effective_font_size);

        // Sanity check ShapeInspection helper import is actively exercised
        let (_, _, meta) = panes.run_split_frame();
        let insp: ShapeInspection = meta.inspect_shapes();
        assert!(insp.contains_text("RIGHT_FIXED"));
    }

    #[test]
    fn t3_24_f21_f18_multipane_focus_paste_and_background_output() {
        let mut panes = MultiPaneHarness::new(ScriptedInput::new(), ScriptedInput::new());
        panes.set_viewport_size(800.0, 600.0);
        let _ = panes.run_split_frame();

        // Click left pane (100, 100) and type "cmdA".
        panes.queue_event(egui::Event::PointerMoved(egui::pos2(100.0, 100.0)));
        panes.queue_event(egui::Event::PointerButton {
            pos: egui::pos2(100.0, 100.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        panes.queue_event(egui::Event::PointerButton {
            pos: egui::pos2(100.0, 100.0),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        let _ = panes.run_split_frame();
        panes.queue_event(egui::Event::Text("cmdA".to_owned()));
        let _ = panes.run_split_frame();

        // Click right pane (600, 100).
        panes.queue_event(egui::Event::PointerMoved(egui::pos2(600.0, 100.0)));
        panes.queue_event(egui::Event::PointerButton {
            pos: egui::pos2(600.0, 100.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        panes.queue_event(egui::Event::PointerButton {
            pos: egui::pos2(600.0, 100.0),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        let _ = panes.run_split_frame();

        // Paste "cmdB" into focused right pane while pushing background output to left pane.
        panes.left.input_mut().push_str("BG_LEFT");
        panes.queue_event(egui::Event::Paste("cmdB".to_owned()));
        let (_, _, meta) = panes.run_split_frame();

        assert_eq!(panes.left.input().written_bytes(), b"cmdA");
        assert_eq!(panes.right.input().written_bytes(), b"cmdB");
        assert_eq!(
            panes.left.terminal().visible_line_text(0).as_deref(),
            Some("BG_LEFT")
        );
        assert!(meta.inspect_shapes().contains_text("BG_LEFT"));
    }
}

#[cfg(feature = "search")]
#[test]
fn t3_12_f11_f6_search_across_soft_wrap_after_resize() {
    use egui_xterm::{ScaleMode, TerminalConfig};
    use egui_xterm_core::SearchQuery;
    use egui_xterm_e2e::HeadlessTermHarness;

    let cfg = TerminalConfig {
        scale_mode: ScaleMode::ReflowGrid,
        resize_debounce_secs: 0.0,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    let warm = h.run_frame().terminal_response;
    let cw = warm.cell_size.x;
    let ch = warm.cell_size.y;

    h.set_viewport_size(80.0 * cw + 1.0, 24.0 * ch + 1.0);
    let r80 = h.run_frame().terminal_response;
    assert_eq!(r80.cols, 80);

    h.runtime
        .input_mut()
        .push_str("long_prefix_TOKEN_MATCH_suffix");
    let _ = h.run_frame();

    h.set_viewport_size(20.0 * cw + 0.5, 24.0 * ch + 1.0);
    let base_out = h.run_frame();
    assert_eq!(base_out.terminal_response.cols, 20);
    let base_insp = base_out.inspect_shapes();

    let matches = h
        .runtime
        .terminal_mut()
        .search(&SearchQuery::literal("TOKEN_MATCH"))
        .expect("search across wrap");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].line, 0);
    assert_eq!(matches[0].start_col, 12);
    assert_eq!(matches[0].end_line, 1);
    assert_eq!(matches[0].end_col, 3);

    let hl_out = h.run_frame();
    let term_rect = hl_out.terminal_response.response.rect;
    let top2_min_y = term_rect.min.y - 1.0;
    let top2_max_y = term_rect.min.y + 2.0 * ch + 1.0;
    let hl_insp = hl_out.inspect_shapes();

    let base_top2_rects: Vec<_> = base_insp
        .rect_fills
        .iter()
        .filter(|(r, _)| r.min.y >= top2_min_y && r.max.y <= top2_max_y)
        .collect();
    let hl_top2_rects: Vec<_> = hl_insp
        .rect_fills
        .iter()
        .filter(|(r, _)| r.min.y >= top2_min_y && r.max.y <= top2_max_y)
        .collect();
    let base_top2_mesh_verts = base_insp
        .mesh_vertex_positions()
        .iter()
        .filter(|p| p.y >= top2_min_y && p.y <= top2_max_y)
        .count();
    let hl_top2_mesh_verts = hl_insp
        .mesh_vertex_positions()
        .iter()
        .filter(|p| p.y >= top2_min_y && p.y <= top2_max_y)
        .count();

    assert!(
        hl_top2_rects != base_top2_rects || hl_top2_mesh_verts > base_top2_mesh_verts,
        "expected search highlight fills/mesh inside top 2 rows y=[{top2_min_y}, {top2_max_y}]"
    );
}

#[cfg(feature = "hyperlinks")]
#[test]
fn t3_13_f12_f6_hyperlinks_across_soft_wrap_on_resize() {
    use egui::{Modifiers, PointerButton};
    use egui_xterm::{ScaleMode, TerminalConfig, TerminalEvent};
    use egui_xterm_e2e::HeadlessTermHarness;

    let cfg = TerminalConfig {
        scale_mode: ScaleMode::ReflowGrid,
        resize_debounce_secs: 0.0,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    let warm = h.run_frame().terminal_response;
    let cw = warm.cell_size.x;
    let ch = warm.cell_size.y;

    h.set_viewport_size(80.0 * cw + 1.0, 24.0 * ch + 1.0);
    let r80 = h.run_frame().terminal_response;
    assert_eq!(r80.cols, 80);

    // 22-char label wraps across rows 0..1 at 18 cols; implicit URL wraps across rows 1..2.
    h.runtime.input_mut().push_str(
        "\x1b]8;;https://example.org/docs/path\x07ClickHereLongLinkLabel\x1b]8;;\x07 and https://rust-lang.org/learn",
    );
    let _ = h.run_frame();

    h.set_viewport_size(18.0 * cw + 0.5, 24.0 * ch + 1.0);
    let r18 = h.run_frame().terminal_response;
    assert_eq!(r18.cols, 18);

    // Verify explicit OSC 8 closed cleanly before " and " and wrapped onto row 1.
    assert_eq!(
        h.runtime
            .terminal()
            .hyperlink_at(1, 1)
            .map(|l| l.uri.as_str()),
        Some("https://example.org/docs/path")
    );
    assert!(h.runtime.terminal().hyperlink_at(5, 1).is_none());

    let explicit_wrapped = h.cell_center(1, 1).expect("wrapped OSC 8 cell (1, 1)");
    h.inject_click(explicit_wrapped, PointerButton::Primary, Modifiers::NONE);
    let out1 = h.last_outcome().expect("click 1");
    assert!(
        out1.opened_urls().iter().any(|u| u == "https://example.org/docs/path")
            || out1.terminal_response.events.iter().any(|e| {
                matches!(e, TerminalEvent::HyperlinkActivated { uri, .. } if uri == "https://example.org/docs/path")
            }),
        "expected wrapped OSC 8 link activation"
    );

    // Locate implicit URL on its wrapped second physical row (row 2).
    assert_eq!(
        h.runtime
            .terminal()
            .hyperlink_at(2, 2)
            .map(|l| l.uri.as_str()),
        Some("https://rust-lang.org/learn")
    );
    let implicit_wrapped = h
        .cell_center(2, 2)
        .expect("wrapped implicit URL cell (2, 2)");
    h.inject_click(implicit_wrapped, PointerButton::Primary, Modifiers::NONE);
    let out2 = h.last_outcome().expect("click 2");
    assert!(
        out2.opened_urls().iter().any(|u| u == "https://rust-lang.org/learn")
            || out2.terminal_response.events.iter().any(|e| {
                matches!(e, TerminalEvent::HyperlinkActivated { uri, .. } if uri == "https://rust-lang.org/learn")
            }),
        "expected wrapped implicit URL activation"
    );
}

#[cfg(feature = "hyperlinks")]
#[test]
fn t3_25_f12_f17_hyperlink_hover_underline_and_click() {
    use egui::{CursorIcon, Modifiers, PointerButton};
    use egui_xterm::TerminalEvent;
    use egui_xterm_e2e::HeadlessTermHarness;

    let mut h = HeadlessTermHarness::scripted();
    h.runtime
        .input_mut()
        .push_str("\x1b]8;;https://egui.rs\x07egui\x1b]8;;\x07");
    let base = h.run_frame().inspect_shapes();

    let target = h.cell_center(1, 0).expect("cell (1, 0)");
    h.inject_pointer_move(target);
    let hover = h.run_frame();
    assert_eq!(hover.platform_output.cursor_icon, CursorIcon::PointingHand);
    let hover_insp = hover.inspect_shapes();
    assert!(
        hover_insp.line_count > base.line_count
            || hover_insp.rect_count > base.rect_count
            || hover_insp.path_count > base.path_count
            || hover_insp.mesh_vertices_total > base.mesh_vertices_total,
        "expected hover underline shape for hyperlink"
    );

    h.inject_click(target, PointerButton::Primary, Modifiers::NONE);
    let clicked = h.last_outcome().expect("click outcome");
    assert!(
        clicked.opened_urls().iter().any(|u| u == "https://egui.rs"),
        "expected OpenUrl for https://egui.rs"
    );
    assert!(clicked.terminal_response.events.iter().any(|ev| {
        matches!(
            ev,
            TerminalEvent::HyperlinkActivated { uri, .. } if uri == "https://egui.rs"
        )
    }));
}

#[cfg(feature = "kitty-keyboard")]
mod stage_m4_kitty {
    use egui::{Key, Modifiers};
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn t3_16_f14_f18_kitty_keyboard_focus_lock_and_csi_u() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[>7u");

        let mut term_rect = egui::Rect::NOTHING;
        let _ = h.run_frame_in_ui(|ui, runtime, config| {
            let _ = ui.button("before");
            let resp = runtime.show_with_config(ui, config);
            term_rect = resp.response.rect;
            let _ = ui.button("after");
            resp
        });
        let center = term_rect.center();
        h.inject_pointer_move(center);
        h.inject_pointer_button(center, egui::PointerButton::Primary, true, Modifiers::NONE);
        h.inject_pointer_button(center, egui::PointerButton::Primary, false, Modifiers::NONE);
        let focused = h.run_frame_in_ui(|ui, runtime, config| {
            let _ = ui.button("before");
            let resp = runtime.show_with_config(ui, config);
            let _ = ui.button("after");
            resp
        });
        assert!(focused.terminal_response.response.has_focus());

        // Settle 1 frame after focus gain so egui's Memory::set_focus_lock_filter (which requires
        // had_focus_last_frame(id) == true) is armed before Tab/Escape/ArrowUp arrive.
        let settled = h.run_frame_in_ui(|ui, runtime, config| {
            let _ = ui.button("before");
            let resp = runtime.show_with_config(ui, config);
            let _ = ui.button("after");
            resp
        });
        assert!(settled.terminal_response.response.has_focus());
        h.runtime.input_mut().clear_written();

        h.tap_key(Key::Tab, Modifiers::NONE);
        h.tap_key(Key::Escape, Modifiers::NONE);
        h.tap_key(Key::ArrowUp, Modifiers::NONE);
        h.tap_key(Key::K, Modifiers::CTRL | Modifiers::SHIFT);

        let after = h.run_frame_in_ui(|ui, runtime, config| {
            let _ = ui.button("before");
            let resp = runtime.show_with_config(ui, config);
            let _ = ui.button("after");
            resp
        });
        assert!(after.terminal_response.response.has_focus());
        let written = h.runtime.input().written_str();
        assert!(
            written.contains("\x1b[27"),
            "expected Escape CSI u: {written:?}"
        );
        assert!(
            written.contains('\t') || written.contains("\x1b[9u") || written.contains("\x1b[9;"),
            "expected Tab (\\t or CSI 9 u): {written:?}"
        );
        assert!(
            written
                .split('\x1b')
                .any(|seq| seq.starts_with("[A") || (seq.starts_with("[1;") && seq.ends_with('A'))),
            "expected ArrowUp sequence: {written:?}"
        );
        assert!(
            written.contains("\x1b[107;") || written.contains("\x1b[107:"),
            "expected Ctrl+Shift+K CSI u (107): {written:?}"
        );
        assert!(
            written.contains('u'),
            "expected Kitty CSI u terminator: {written:?}"
        );
    }

    #[test]
    fn t3_26_f10_f14_bracketed_paste_with_kitty_keyboard_active() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[?2004h\x1b[>1u");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_paste("echo hi\x1b[201~bad\x1b[20\x1b[201~1~");
        let _ = h.run_frame();
        let written = h.runtime.input().written_bytes();
        assert!(written.starts_with(b"\x1b[200~"), "{written:?}");
        assert!(written.ends_with(b"\x1b[201~"), "{written:?}");
        let inner = &written[b"\x1b[200~".len()..written.len() - b"\x1b[201~".len()];
        let inner_str = String::from_utf8_lossy(inner);
        assert!(
            inner_str.contains("echo hi") && inner_str.contains("bad"),
            "missing expected paste payload: {inner_str:?}"
        );
        assert!(
            !inner_str.contains("\x1b[201~") && !inner_str.contains("\x1b[200~"),
            "inner bracketed paste marker leaked: {inner_str:?}"
        );
        assert!(
            !inner_str.contains("\x1b["),
            "unexpected raw CSI or Kitty sequence in paste: {inner_str:?}"
        );
    }
}

#[cfg(all(feature = "serde", feature = "metrics"))]
#[test]
fn t3_17_f15_f16_serde_config_roundtrip_and_metrics_auto_scale() {
    use egui_xterm::{ScaleMode, TerminalConfig};
    use egui_xterm_core::Theme;
    use egui_xterm_e2e::HeadlessTermHarness;

    let cfg = TerminalConfig {
        scale_mode: ScaleMode::AutoScaleToFit { cols: 80, rows: 24 },
        theme: Theme::light(),
        zoom_factor: 1.2,
        ..TerminalConfig::default()
    };
    let json = serde_json::to_string(&cfg).expect("serialize TerminalConfig");
    let decoded: TerminalConfig = serde_json::from_str(&json).expect("deserialize TerminalConfig");

    let mut h = HeadlessTermHarness::scripted_with_config(decoded);
    h.runtime.input_mut().push_str("METRICS_OK\r\n");

    h.set_viewport_size(1280.0, 720.0);
    let r1 = h.run_frame().terminal_response;
    assert_eq!((r1.cols, r1.rows), (80, 24));

    h.set_viewport_size(640.0, 360.0);
    let r2 = h.run_frame().terminal_response;
    assert_eq!((r2.cols, r2.rows), (80, 24));
    assert!(r2.effective_font_size < r1.effective_font_size * 0.7);

    h.set_viewport_size(960.0, 540.0);
    let r3 = h.run_frame().terminal_response;
    assert_eq!((r3.cols, r3.rows), (80, 24));

    let metrics = h.runtime.terminal().metrics();
    assert!(metrics.bytes_processed >= 12);
    assert_eq!(metrics.frames_rendered, 3);
    assert!(metrics.dirty_rows_rendered > 0);
}

#[cfg(feature = "vector-glyphs")]
#[test]
fn t3_18_f16_f17_dpi_pixel_snapped_vector_glyphs() {
    use egui_xterm::ScaleMode;
    use egui_xterm_e2e::{HeadlessTermHarness, ShapeInspection};

    let mut h = HeadlessTermHarness::scripted();
    h.runtime.input_mut().push_str("┌──┐\r\n│OK│\r\n└──┘");

    for mode in [
        ScaleMode::ReflowGrid,
        ScaleMode::AutoScaleToFit { cols: 40, rows: 12 },
    ] {
        h.config.scale_mode = mode;
        for ppp in [1.0_f32, 2.0_f32, 1.25_f32] {
            h.set_pixels_per_point(ppp);
            let out = h.run_frame();
            let term_rect = out.terminal_response.response.rect;
            let insp = ShapeInspection::from_terminal_shapes(&out.shapes, term_rect, ppp);
            assert_eq!(
                insp.non_snapped_rect_count, 0,
                "unsnapped terminal rects at ppp={ppp} mode={mode:?}"
            );
            assert!(
                insp.text_entries
                    .iter()
                    .all(|e| (e.pixels_per_point - ppp).abs() < 1e-6),
                "stale galley DPI at ppp={ppp}"
            );
            assert!(
                insp.line_count + insp.path_count + insp.mesh_shape_count + insp.rect_fills.len()
                    > 1,
                "expected vector-glyph geometry at ppp={ppp}"
            );
        }
    }
}

// ===========================================================================
// M5 Native PTY Pairwise Tests (`#[cfg(all(not(target_arch = "wasm32"), feature = "pty", ...))]`)
// ===========================================================================

#[cfg(all(
    not(target_arch = "wasm32"),
    feature = "pty",
    feature = "shell-integration"
))]
#[test]
fn t3_21_f20_f13_pty_osc133_shell_integration() {
    use std::time::Duration;

    use egui_xterm::{TerminalEvent, TerminalSize};
    use egui_xterm_e2e::PtyTestSession;
    use egui_xterm_pty::PtyConfig;

    #[cfg(unix)]
    let (shell, args) = (
        "sh".to_owned(),
        vec![
            "-c".to_owned(),
            r#"printf "\033]133;A\007\$ \033]133;B\007echo\n\033]133;C\007hello_pty\n\033]133;D;0\007""#
                .to_owned(),
        ],
    );
    #[cfg(windows)]
    let (shell, args) = (
        "powershell.exe".to_owned(),
        vec![
            "-NoLogo".to_owned(),
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "$e=[char]27; $b=[char]7; Write-Host ($e+']133;A'+$b+'PS>'+$e+']133;B'+$b+'cmd'+$e+']133;C'+$b+'hello_pty'+$e+']133;D;0'+$b)".to_owned(),
        ],
    );

    let mut sess = PtyTestSession::spawn(PtyConfig {
        shell: Some(shell),
        args,
        cwd: None,
        env: Vec::new(),
        initial_size: TerminalSize::new(80, 24),
    })
    .expect("spawn OSC 133 PTY");

    let ok = sess
        .run_until_contains("hello_pty", Duration::from_secs(8))
        .expect("poll PTY");
    assert!(ok, "PTY did not emit hello_pty");

    #[cfg(unix)]
    {
        let command_finished = |frame: &egui_xterm_e2e::FrameOutcome| {
            frame
                .terminal_response
                .events
                .contains(&TerminalEvent::CommandFinished { exit_code: Some(0) })
        };
        // The command can finish in the same frame that first displays its output.
        let finished_zero = sess.harness.last_outcome().is_some_and(command_finished)
            || (0..30).any(|_| {
                std::thread::sleep(Duration::from_millis(10));
                sess.harness.advance_time(1.0 / 60.0);
                command_finished(&sess.harness.run_frame())
            });
        assert!(
            !sess.harness.runtime.terminal().shell_zones().is_empty(),
            "expected OSC 133 shell_zones on Unix PTY"
        );
        assert!(
            finished_zero,
            "expected CommandFinished(Some(0)) on Unix PTY"
        );
    }

    #[cfg(windows)]
    {
        // ConPTY may strip or rewrite unknown OSC 133 payloads; assert zero raw leakage,
        // and if ConPTY passes OSC 133 through, verify zone integrity.
        let lines = sess.harness.runtime.terminal().visible_lines();
        assert!(
            lines
                .iter()
                .all(|l| !l.contains("133;A") && !l.contains("133;D")),
            "raw OSC 133 payload leaked to screen: {lines:?}"
        );
        if !sess.harness.runtime.terminal().shell_zones().is_empty() {
            assert!(
                sess.harness
                    .last_outcome()
                    .is_some_and(|o| o.terminal_response.events.iter().all(|e| {
                        !matches!(e, TerminalEvent::CommandFinished { exit_code: Some(code) } if *code != 0)
                    }))
            );
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "pty"))]
#[test]
fn t3_22_f20_f16_pty_debounced_resize_no_storm() {
    use egui_xterm::{TerminalConfig, TerminalSize};
    use egui_xterm_e2e::{CountingResizeInput, HeadlessTermHarness};
    use egui_xterm_pty::{PtyBackend, PtyConfig};

    #[cfg(windows)]
    let (shell, args) = (
        "cmd".to_owned(),
        vec!["/C".to_owned(), "ping -n 5 127.0.0.1 > nul".to_owned()],
    );
    #[cfg(not(windows))]
    let (shell, args) = ("sh".to_owned(), vec!["-c".to_owned(), "sleep 5".to_owned()]);

    let backend = PtyBackend::spawn(PtyConfig {
        shell: Some(shell),
        args,
        cwd: None,
        env: Vec::new(),
        initial_size: TerminalSize::new(80, 24),
    })
    .expect("spawn PTY for debounced resize");

    let cfg = TerminalConfig {
        resize_debounce_secs: 0.15,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::with_config(CountingResizeInput::new(backend), cfg);
    h.set_viewport_size(800.0, 600.0);
    let _ = h.run_frame();
    h.runtime.input_mut().clear_resizes();

    let widths = [740.0_f32, 680.0, 620.0, 560.0, 510.0, 480.0];
    let mut last_resp = None;
    for (idx, w) in widths.into_iter().enumerate() {
        h.advance_time(1.0 / 60.0);
        h.set_viewport_size(w, 600.0 - (idx as f32) * 30.0);
        last_resp = Some(h.run_frame().terminal_response);
        assert!(
            h.runtime.input().recorded_resizes().is_empty(),
            "intermediate resize storm at step {idx}"
        );
    }
    let target = last_resp.expect("final rapid frame");

    h.advance_time(0.20);
    let _ = h.run_frame();
    let resizes = h.runtime.input().recorded_resizes();
    assert_eq!(resizes.len(), 1);
    assert_eq!(
        (resizes[0].cols, resizes[0].rows),
        (target.cols, target.rows)
    );
}
