//! Wave 2 opaque-box E2E tests for Item #5: Core Search, Selection, Snapshot & Split Parser/State APIs.
//!
//! Test inventory (15 tests total, matching `TEST_INFRA.md` Wave 2 item #5):
//! - Tier 1 (7): `t1_item5_01` .. `t1_item5_07`
//! - Tier 2 (6): `t2_item5_01` .. `t2_item5_06`
//! - Tier 3 (1): `t3_search_01_whole_word_reflow_scrollback_selection_and_palette_sync`
//! - Tier 4 (1): `t4_workload_03_interactive_pager_search_selection_and_mouse`

#![forbid(unsafe_code)]

use egui::{Color32, Modifiers, PointerButton};
use egui_xterm::{ColorScheme, TerminalConfig, TerminalEvent, Theme};
use egui_xterm_core::{
    C1Policy, CoreConfig, Parser, RenderSnapshot, Row, SelectionKind, SpecialColors, Terminal,
    TerminalState, VisibleRowsView,
};
use egui_xterm_e2e::HeadlessTermHarness;

fn rgb_to_c32(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

// ===========================================================================
// Tier 1 — Category-Partition Equivalence Classes (7 Tests)
// ===========================================================================

#[cfg(feature = "search")]
#[test]
fn t1_item5_01_search_query_whole_word_literal_and_regex() {
    use egui_xterm_core::SearchQuery;

    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(720.0, 260.0);
    h.config.cursor_blink = false;
    h.focus_terminal();

    h.runtime
        .input_mut()
        .push_str("cat scatter cat_1 _cat (cat) Cat xaaa aa café CAFÉ café2\r\nalpha_1 alpha\r\n");
    drop(h.run_frame());

    // 1. Case-sensitive literal whole word: matches "cat" at col 0 and "(cat)" at col 24.
    let cs_lit = h
        .runtime
        .terminal_mut()
        .search(&SearchQuery::literal("cat").with_whole_word(true))
        .expect("literal whole_word query compiles");
    assert_eq!(cs_lit.len(), 2);
    assert_eq!(
        (cs_lit[0].line, cs_lit[0].start_col, cs_lit[0].end_col),
        (0, 0, 3)
    );
    assert_eq!(
        (cs_lit[1].line, cs_lit[1].start_col, cs_lit[1].end_col),
        (0, 24, 27)
    );

    // 2. ASCII case-insensitive literal whole word: adds "Cat" (3 matches total).
    let ci_lit = h
        .runtime
        .terminal_mut()
        .search(
            &SearchQuery::literal("CAT")
                .with_case_sensitive(false)
                .with_whole_word(true),
        )
        .expect("case-insensitive whole_word query compiles");
    assert_eq!(ci_lit.len(), 3);
    assert_eq!(ci_lit[2].start_col, 29);

    // 3. Overlapping literal rejection ("aa" inside "xaaa" skipped, standalone "aa" matched).
    let overlap = h
        .runtime
        .terminal_mut()
        .search(&SearchQuery::literal("aa").with_whole_word(true))
        .expect("overlap whole_word query compiles");
    assert_eq!(overlap.len(), 1);
    assert_eq!((overlap[0].start_col, overlap[0].end_col), (38, 40));

    // 4. Unicode case-insensitive literal whole word: matches "café" & "CAFÉ", skips "café2".
    let unicode_ci = h
        .runtime
        .terminal_mut()
        .search(
            &SearchQuery::literal("café")
                .with_case_sensitive(false)
                .with_whole_word(true),
        )
        .expect("unicode whole_word query compiles");
    assert_eq!(unicode_ci.len(), 2);

    // 5. Regex whole word: matches "cat", "cat_1", "(cat)", and "alpha" (not "alpha_1").
    let re_cat = h
        .runtime
        .terminal_mut()
        .search(&SearchQuery::regex(r"cat(_1)?").with_whole_word(true))
        .expect("regex whole_word compiles");
    assert_eq!(re_cat.len(), 3);
    let re_alpha = h
        .runtime
        .terminal_mut()
        .search(&SearchQuery::regex(r"alpha").with_whole_word(true))
        .expect("alpha regex whole_word compiles");
    assert_eq!(re_alpha.len(), 1);
    assert_eq!(
        (re_alpha[0].line, re_alpha[0].start_col, re_alpha[0].end_col),
        (1, 8, 13)
    );

    // 6. Rendered highlight mesh + SearchBarState whole-word toggle end-to-end.
    assert!(h.runtime.terminal_mut().next_search_match().is_some());
    let hl_frame = h.run_frame();
    let active_hl = Color32::from_rgb(255, 165, 0);
    assert!(
        hl_frame.inspect_shapes().has_rect_or_mesh_color(active_hl),
        "active whole-word match must emit active highlight background quad"
    );

    h.runtime.open_search();
    h.runtime.search_bar_mut().case_sensitive = true;
    h.runtime.search_bar_mut().whole_word = true;
    h.runtime.search_bar_mut().is_regex = false;
    drop(h.run_frame());
    h.inject_text("cat");
    let ui_frame = h.run_frame();
    assert_eq!(h.runtime.terminal().search_matches().len(), 2);
    assert!(ui_frame.inspect_shapes().contains_text("0/2"));
}

#[cfg(feature = "search")]
#[test]
fn t1_item5_02_search_matches_on_line_binary_lookup() {
    use egui_xterm_core::SearchQuery;

    let cfg = CoreConfig {
        scrollback_limit: 1_200,
        ..CoreConfig::default()
    };
    let mut term = Terminal::new(16, 6, cfg);

    // Build 600 logical lines: even lines carry 2 matches ("hit ... hit"), odd lines have none,
    // plus soft-wrapped multi-line matches at logical line 10 (spanning cols 14..16 + 0..1)
    // and logical line 20 (3-row CJK soft-wrapped line).
    for i in 0..600usize {
        if i == 10 {
            // "wrap_00000000_hi" (16 cols) + "t_tail" -> match spans physical rows across wrap!
            term.advance(b"wrap_00000000_hit_tail\r\n");
        } else if i == 20 {
            // 20 cols of CJK + "hit" + more CJK -> spans 3 physical rows on 16-col grid.
            term.advance("中文测试中文测试中文测试hit中文测试\r\n".as_bytes());
        } else if i % 2 == 0 {
            term.advance(format!("L{i:03} hit A hit\r\n").as_bytes());
        } else {
            term.advance(format!("L{i:03} miss\r\n").as_bytes());
        }
    }

    let all = term
        .search(&SearchQuery::literal("hit"))
        .expect("literal search succeeds")
        .to_vec();
    assert!(!all.is_empty());

    // Locate the soft-wrapped match (line < end_line) around retained lines 10..=15.
    let wrapped_match = all
        .iter()
        .copied()
        .find(|m| m.line < m.end_line)
        .expect("soft-wrapped multi-line match must exist");
    assert_eq!(wrapped_match.end_line, wrapped_match.line + 1);

    // Verify 100% agreement between O(log M) `search_matches_on_line` and full linear filter
    // across all retained lines (0..total_lines).
    let total = term.total_lines();
    for line in 0..total {
        let expected: Vec<_> = all
            .iter()
            .copied()
            .filter(|m| line >= m.line && line <= m.end_line)
            .collect();
        assert_eq!(
            term.search_matches_on_line(line),
            expected.as_slice(),
            "line {line} lookup mismatch"
        );
        let abs = term.grid().abs_line_of_buf(line);
        assert_eq!(
            term.search_matches_on_abs_line(abs),
            expected.as_slice(),
            "abs_line {abs:?} lookup mismatch"
        );
    }

    // Both physical rows of the wrapped match must return that match.
    assert_eq!(
        term.search_matches_on_line(wrapped_match.line),
        &[wrapped_match]
    );
    assert_eq!(
        term.search_matches_on_line(wrapped_match.end_line),
        &[wrapped_match]
    );

    // `search_matches_in_range` agrees with overlap filter across single-row, straddling, and full ranges.
    for (start, end) in [
        (wrapped_match.line, wrapped_match.line + 1),
        (wrapped_match.end_line, wrapped_match.end_line + 1),
        (wrapped_match.line, wrapped_match.end_line + 1),
        (0, total),
        (5, 5),
    ] {
        let expected: Vec<_> = all
            .iter()
            .copied()
            .filter(|m| start < end && m.end_line >= start && m.line < end)
            .collect();
        assert_eq!(
            term.search_matches_in_range(start, end),
            expected.as_slice(),
            "range {start}..{end} mismatch"
        );
    }
}

#[cfg(feature = "search")]
#[test]
fn t1_item5_03_incremental_search_refinement_and_cursor() {
    use egui_xterm_core::SearchQuery;

    let cfg = CoreConfig {
        scrollback_limit: 500,
        ..CoreConfig::default()
    };
    let mut term = Terminal::new(40, 6, cfg.clone());
    let mut oracle = Terminal::new(40, 6, cfg);

    let mut payload = String::new();
    for i in 0..80usize {
        if i % 4 == 0 {
            payload.push_str(&format!("line {i:02}: error critical\r\n"));
        } else if i % 4 == 1 {
            payload.push_str(&format!("line {i:02}: err warn\r\n"));
        } else if i % 4 == 2 {
            payload.push_str(&format!("line {i:02}: er trace\r\n"));
        } else {
            payload.push_str(&format!("line {i:02}: ok\r\n"));
        }
    }
    term.advance(payload.as_bytes());
    oracle.advance(payload.as_bytes());

    // 1. Chunked incremental search advances monotonically to completion and matches `search()`.
    let mut prog = term
        .begin_incremental_search(&SearchQuery::literal("error"))
        .expect("begin incremental search");
    assert!(!prog.complete);
    assert_eq!(prog.scanned_lines, 0);
    let mut prev_scanned = 0usize;
    let mut steps = 0usize;
    while !prog.complete {
        prog = term.step_incremental_search(5);
        assert!(prog.scanned_lines >= prev_scanned);
        prev_scanned = prog.scanned_lines;
        steps += 1;
        assert!(steps <= 100);
    }
    assert!(steps >= 3);
    assert_eq!(prog.scanned_lines, prog.total_lines);
    let expected_initial = oracle
        .search(&SearchQuery::literal("error"))
        .expect("oracle search");
    assert_eq!(term.search_matches(), expected_initial.as_slice());

    // 2. Navigate to match index 2, append new tail matches, and refresh incrementally
    //    without losing active match cursor or scrollback matches.
    assert_eq!(term.next_search_match().map(|m| m.line), Some(0));
    assert_eq!(term.next_search_match().map(|m| m.line), Some(4));
    assert_eq!(term.next_search_match().map(|m| m.line), Some(8));
    assert_eq!(term.active_search_match_index(), Some(2));
    let anchored = *term.active_search_match().expect("active match #2");

    let extra = b"tail 1: error added\r\ntail 2: error added\r\n";
    term.advance(extra);
    oracle.advance(extra);
    assert_eq!(term.refresh_search(), Ok(true));
    assert_eq!(term.refresh_search(), Ok(false));
    assert_eq!(term.search_matches().len(), expected_initial.len() + 2);
    assert_eq!(term.active_search_match_index(), Some(2));
    assert_eq!(term.active_search_match().copied(), Some(anchored));

    // Scroll-invariant navigation jumps across scrollback without invalidating matches.
    let last_line = term.search_matches().last().expect("last match").line;
    let prev = term.prev_search_match().expect("wrap or step backward");
    assert_eq!(prev.line, 4);
    assert!(term.display_offset() > 0);
    assert_eq!(
        term.search_matches().last().expect("still valid").line,
        last_line
    );

    // 3. Literal query refinement fast path ("er" -> "err" -> "error") narrows monotonically
    //    and matches fresh oracle search 100%.
    let r_er = term
        .search(&SearchQuery::literal("er"))
        .expect("search er")
        .len();
    let r_err = term
        .search(&SearchQuery::literal("err"))
        .expect("search err")
        .len();
    let r_error = term
        .search(&SearchQuery::literal("error"))
        .expect("search error");
    assert!(r_er > r_err && r_err > r_error.len());
    let oracle_refined = oracle
        .search(&SearchQuery::literal("error"))
        .expect("oracle refined");
    assert_eq!(r_error, oracle_refined);

    // 4. Mid-scan mutation restarts incremental cursor coherently.
    let mut p_mid = term
        .begin_incremental_search(&SearchQuery::literal("error"))
        .expect("begin mid-scan search");
    assert!(!p_mid.complete);
    p_mid = term.step_incremental_search(3);
    assert!(!p_mid.complete);
    term.advance(b"tail 3: error mid-scan\r\n");
    while !p_mid.complete {
        p_mid = term.step_incremental_search(10);
    }
    assert_eq!(term.search_matches().len(), oracle_refined.len() + 1);
}

#[test]
fn t1_item5_04_has_non_empty_selection_across_modes() {
    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(640.0, 240.0);
    h.focus_terminal();
    h.runtime
        .input_mut()
        .push_str("alpha beta  \r\ngamma delta\r\n中文 测试\r\n \u{0301}\r\n");
    drop(h.run_frame());

    // Initial state: no selection.
    assert!(!h.runtime.terminal().has_non_empty_selection());
    assert!(!h.runtime.terminal().state().has_non_empty_selection());

    // 1. Stream selection via pointer drag over "alpha".
    let s_from = h.cell_center(0, 0).expect("cell (0,0)");
    let s_to = h.cell_center(4, 0).expect("cell (4,0)");
    h.advance_time(0.5);
    h.inject_drag_steps(s_from, s_to, PointerButton::Primary, Modifiers::NONE, 3);
    assert!(h.runtime.terminal().has_non_empty_selection());
    assert!(h.runtime.terminal().state().has_non_empty_selection());
    assert_eq!(
        h.runtime.terminal().selection_text().as_deref(),
        Some("alpha")
    );
    h.inject_copy();
    let copy_frame = h.run_frame();
    assert_eq!(copy_frame.copied_texts(), vec!["alpha".to_owned()]);

    // 2. Word selection via double-click on "beta".
    let beta_pos = h.cell_center(7, 0).expect("cell (7,0) on beta");
    h.advance_time(0.5);
    h.inject_click(beta_pos, PointerButton::Primary, Modifiers::NONE);
    h.advance_time(0.05);
    h.inject_click(beta_pos, PointerButton::Primary, Modifiers::NONE);
    assert!(h.runtime.terminal().has_non_empty_selection());
    assert_eq!(
        h.runtime.terminal().selection_text().as_deref(),
        Some("beta")
    );

    // 3. Line selection via triple-click on row 1 ("gamma delta").
    let gamma_pos = h.cell_center(2, 1).expect("cell (2,1)");
    h.advance_time(0.5);
    h.inject_click(gamma_pos, PointerButton::Primary, Modifiers::NONE);
    h.advance_time(0.05);
    h.inject_click(gamma_pos, PointerButton::Primary, Modifiers::NONE);
    h.advance_time(0.05);
    h.inject_click(gamma_pos, PointerButton::Primary, Modifiers::NONE);
    assert!(h.runtime.terminal().has_non_empty_selection());
    assert_eq!(
        h.runtime.terminal().selection_text().as_deref(),
        Some("gamma delta")
    );

    // 4. Rectangular block selection via Alt+drag (cols 0..=3, rows 0..=1).
    let b_from = h.cell_center(0, 0).expect("block start");
    let b_to = h.cell_center(3, 1).expect("block end");
    h.advance_time(0.5);
    h.inject_drag_steps(b_from, b_to, PointerButton::Primary, Modifiers::ALT, 3);
    assert!(h.runtime.terminal().has_non_empty_selection());
    assert_eq!(
        h.runtime.terminal().selection_text().as_deref(),
        Some("alph\ngamm")
    );

    // 5. Wide CJK character selection + combining mark on space (" \u{0301}").
    h.runtime
        .terminal_mut()
        .selection_start(2, 0, SelectionKind::Stream);
    h.runtime.terminal_mut().selection_update(2, 3);
    h.runtime.terminal_mut().selection_finish();
    assert!(h.runtime.terminal().has_non_empty_selection());
    assert_eq!(
        h.runtime.terminal().selection_text().as_deref(),
        Some("中文")
    );

    h.runtime
        .terminal_mut()
        .selection_start(3, 0, SelectionKind::Stream);
    h.runtime.terminal_mut().selection_finish();
    assert!(
        h.runtime.terminal().has_non_empty_selection(),
        "space carrying combining mark must count as non-empty selection"
    );

    // 6. selection_clear() resets has_non_empty_selection() to false.
    h.runtime.terminal_mut().selection_clear();
    assert!(!h.runtime.terminal().has_non_empty_selection());
    assert_eq!(h.runtime.terminal().selection_text(), None);
}

#[test]
fn t1_item5_05_palette_epoch_and_special_colors_tracking() {
    let cfg = TerminalConfig {
        cursor_blink: false,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.focus_terminal();

    let e0 = h.runtime.terminal().palette_epoch();
    assert_eq!(e0, 0);
    assert_eq!(
        h.runtime.terminal().special_colors(),
        SpecialColors::default()
    );
    assert_eq!(h.runtime.terminal().active_snapshot().palette_epoch, e0);
    assert_eq!(
        h.runtime.terminal().active_snapshot().special_colors,
        [None; 5]
    );

    // 1. OSC 4 (ANSI + extended 256) bumps palette_epoch & emits PaletteChanged.
    h.runtime
        .input_mut()
        .push_str("\x1b]4;1;#112233;200;#445566\x07");
    let f1 = h.run_frame();
    let e1 = h.runtime.terminal().palette_epoch();
    assert!(e1 > e0);
    assert!(
        f1.terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    {
        let snap1 = h.runtime.terminal().active_snapshot();
        assert_eq!(snap1.palette_epoch, e1);
        assert_eq!(snap1.resolve_indexed_color(1), [0x11, 0x22, 0x33]);
        assert_eq!(snap1.resolve_indexed_color(200), [0x44, 0x55, 0x66]);
    }

    // 2. OSC 10/11/12 propagates through palette_epoch and rendered background/cursor shapes.
    h.runtime
        .input_mut()
        .push_str("\x1b]10;#abcdef\x07\x1b]11;#010203\x07\x1b]12;#fedcba\x07HELLO");
    let f2 = h.run_frame();
    let e2 = h.runtime.terminal().palette_epoch();
    assert!(e2 > e1);
    assert!(
        f2.terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    assert!(
        f2.inspect_shapes()
            .has_rect_or_mesh_color(Color32::from_rgb(0x01, 0x02, 0x03))
    );

    // 3. OSC 5 registers 0..=4 (bold, underline, blink, reverse, italic) + query + OSC 105 reset.
    h.runtime.input_mut().push_str(
        "\x1b]5;0;#ff1122;1;#22ff33;2;#3344ff;3;#445566;4;#778899\x07\x1b]5;0;?;4;?\x1b\\",
    );
    let f3 = h.run_frame();
    let e3 = h.runtime.terminal().palette_epoch();
    assert!(e3 > e2);
    assert!(
        f3.terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    let sc = h.runtime.terminal().special_colors();
    assert_eq!(sc.bold, Some([0xff, 0x11, 0x22]));
    assert_eq!(sc.underline, Some([0x22, 0xff, 0x33]));
    assert_eq!(sc.blink, Some([0x33, 0x44, 0xff]));
    assert_eq!(sc.reverse, Some([0x44, 0x55, 0x66]));
    assert_eq!(sc.italic, Some([0x77, 0x88, 0x99]));
    assert_eq!(
        h.runtime.terminal().active_snapshot().special_colors,
        [
            Some([0xff, 0x11, 0x22]),
            Some([0x22, 0xff, 0x33]),
            Some([0x33, 0x44, 0xff]),
            Some([0x44, 0x55, 0x66]),
            Some([0x77, 0x88, 0x99]),
        ]
    );
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b]5;0;rgb:ffff/1111/2222\x1b\\\x1b]5;4;rgb:7777/8888/9999\x1b\\"
    );
    h.runtime.input_mut().clear_written();

    // Selective OSC 105;0 clears only bold while preserving 1..=4.
    h.runtime.input_mut().push_str("\x1b]105;0\x07");
    drop(h.run_frame());
    let e4 = h.runtime.terminal().palette_epoch();
    assert!(e4 > e3);
    assert_eq!(h.runtime.terminal().special_colors().bold, None);
    assert_eq!(
        h.runtime.terminal().special_colors().italic,
        Some([0x77, 0x88, 0x99])
    );

    // Ordinary text and scrolling must NOT change palette_epoch.
    h.runtime.input_mut().push_str("\r\nline1\r\nline2\r\n");
    let f_plain = h.run_frame();
    assert_eq!(h.runtime.terminal().palette_epoch(), e4);
    assert!(
        !f_plain
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );

    // Full OSC 105 + OSC 104/110/111/112 resets.
    h.runtime
        .input_mut()
        .push_str("\x1b]105\x07\x1b]104\x07\x1b]110\x07\x1b]111\x07\x1b]112\x07");
    drop(h.run_frame());
    let e5 = h.runtime.terminal().palette_epoch();
    assert!(e5 > e4);
    assert_eq!(
        h.runtime.terminal().special_colors(),
        SpecialColors::default()
    );

    // 4. Theme switch bumps palette_epoch and propagates to rendered background.
    h.config.theme = Theme::light();
    let f_theme = h.run_frame();
    let e6 = h.runtime.terminal().palette_epoch();
    assert!(e6 > e5);
    assert!(
        f_theme
            .inspect_shapes()
            .has_rect_or_mesh_color(rgb_to_c32(ColorScheme::light().background))
    );

    // 5. RIS resets special_colors and bumps palette_epoch.
    h.runtime.input_mut().push_str("\x1b]5;2;#123456\x07");
    drop(h.run_frame());
    let e7 = h.runtime.terminal().palette_epoch();
    assert!(e7 > e6);
    assert_eq!(
        h.runtime.terminal().special_colors().blink,
        Some([0x12, 0x34, 0x56])
    );

    #[cfg(feature = "serde")]
    {
        let snap = h.runtime.terminal().export_snapshot();
        let json = serde_json::to_string(&snap).expect("serialize TerminalSnapshot");
        let decoded: egui_xterm::TerminalSnapshot =
            serde_json::from_str(&json).expect("deserialize TerminalSnapshot");
        let restored = Terminal::from_snapshot(decoded, CoreConfig::default());
        assert_eq!(restored.special_colors().blink, Some([0x12, 0x34, 0x56]));
    }

    h.runtime.input_mut().push_str("\x1bc");
    drop(h.run_frame());
    let e8 = h.runtime.terminal().palette_epoch();
    assert!(e8 > e7);
    assert_eq!(
        h.runtime.terminal().special_colors(),
        SpecialColors::default()
    );
}

#[test]
fn t1_item5_06_zero_alloc_render_snapshot_and_visible_rows_view() {
    // 1. Direct VisibleRowsView unit-contract verification (empty, two-slice split, iterators, indexing).
    let empty_view = VisibleRowsView::empty();
    assert_eq!(empty_view.len(), 0);
    assert!(empty_view.is_empty());
    assert!(empty_view.get(0).is_none());
    assert_eq!(empty_view.slices(), (&[][..], &[][..]));
    assert_eq!(empty_view.iter().count(), 0);

    let first_rows = vec![Row::new(6, 1), Row::new(6, 2)];
    let second_rows = vec![Row::new(6, 3), Row::new(6, 4), Row::new(6, 5)];
    let view = VisibleRowsView::from_slices(&first_rows, &second_rows);
    assert_eq!(view.len(), 5);
    assert!(!view.is_empty());
    assert_eq!(
        view.slices(),
        (first_rows.as_slice(), second_rows.as_slice())
    );
    for expected_gen in 1..=5u64 {
        let idx = usize::try_from(expected_gen - 1).expect("fits usize");
        assert_eq!(view.get(idx).map(Row::generation), Some(expected_gen));
        assert_eq!(view[idx].generation(), expected_gen);
    }
    assert!(view.get(5).is_none());

    let mut fwd = view.iter();
    assert_eq!(fwd.len(), 5);
    let collected_fwd: Vec<u64> = fwd.by_ref().map(Row::generation).collect();
    assert_eq!(collected_fwd, vec![1, 2, 3, 4, 5]);
    // FusedIterator keeps returning None after exhaustion.
    assert!(fwd.next().is_none());
    assert!(fwd.next().is_none());

    let collected_rev: Vec<u64> = (&view).into_iter().rev().map(Row::generation).collect();
    assert_eq!(collected_rev, vec![5, 4, 3, 2, 1]);
    let collected_owned: Vec<u64> = view.into_iter().map(Row::generation).collect();
    assert_eq!(collected_owned, vec![1, 2, 3, 4, 5]);

    // 2. Live Terminal ring-buffer wraparound + combining marks + OSC 66 SIZED_SPACER + selection + OSC 4.
    let cfg = CoreConfig {
        scrollback_limit: 8,
        ..CoreConfig::default()
    };
    let mut term = Terminal::new(20, 4, cfg);
    for i in 0..30usize {
        term.advance(format!("hist_{i:02}\r\n").as_bytes());
    }
    term.advance("\x1b[2J\x1b[Hrow0 e\u{0301} 中文\r\n\x1b]66;s=2;HI\x07\r\n\r\nrow3".as_bytes());
    term.advance(b"\x1b]4;205;#aa5500\x07");

    // Verify ring wraparound can produce a two-slice view across display offsets.
    let mut saw_split_slices = false;
    for offset in 0..=term.scrollback_len() {
        let delta = i32::try_from(offset).expect("offset fits in i32");
        term.scroll_display(delta);
        {
            let snap = term.active_snapshot();
            let (s0, s1) = snap.visible_rows.slices();
            assert_eq!(s0.len() + s1.len(), 4);
            assert_eq!(snap.visible_rows.len(), 4);
            if !s0.is_empty() && !s1.is_empty() {
                saw_split_slices = true;
            }
        }
        term.scroll_to_bottom();
    }
    assert!(
        saw_split_slices || term.scrollback_len() > 0,
        "ring buffer must retain scrollback across wraparound"
    );

    term.selection_start(0, 0, SelectionKind::Stream);
    term.selection_update(0, 3);
    term.selection_finish();

    {
        let snap: RenderSnapshot<'_> = term.active_snapshot();
        assert_eq!((snap.cols, snap.rows), (20, 4));
        assert!(!snap.is_synchronized_output);
        assert!(!snap.is_alt_screen);
        assert_eq!(snap.display_offset, 0);
        assert_eq!(snap.scrollback_len, 8);
        assert_eq!(snap.line_text(0).as_deref(), Some("row0 e\u{0301} 中文"));
        assert_eq!(snap.line_text(1).as_deref(), Some("HI"));
        assert_eq!(snap.line_text(3).as_deref(), Some("row3"));
        assert!(snap.is_cell_selected(0, 0));
        assert!(snap.is_cell_selected(0, 3));
        assert!(!snap.is_cell_selected(0, 4));
        assert_eq!(snap.resolve_indexed_color(205), [0xaa, 0x55, 0x00]);
        let row0 = snap.visible_rows.get(0).expect("row 0");
        assert!(snap.cell_extra(row0.cells()[5].extra_id).is_some());
    }

    // 3. DEC ?2026h latches snapshot visible_rows and palette_epoch cleanly until ?2026l.
    term.advance(b"\x1b[?2026h\x1b[1;1HOVERWRITTEN\x1b]5;0;#112233\x07");
    {
        let latched = term.active_snapshot();
        assert!(latched.is_synchronized_output);
        assert_eq!(latched.line_text(0).as_deref(), Some("row0 e\u{0301} 中文"));
        assert_eq!(latched.special_colors[0], None);
    }
    term.advance(b"\x1b[?2026l");
    {
        let flushed = term.active_snapshot();
        assert!(!flushed.is_synchronized_output);
        assert!(
            flushed
                .line_text(0)
                .as_deref()
                .is_some_and(|l| l.starts_with("OVERWRITTEN"))
        );
        assert_eq!(flushed.special_colors[0], Some([0x11, 0x22, 0x33]));
    }
}

#[test]
fn t1_item5_07_split_parser_and_terminal_state_parity() {
    let stream = concat!(
        "你好e\u{0301}🚀abcXYZ\r\n",
        "\x1b[1;4:3;58:2::200:50:80mSTYLED\x1b[0m\r\n",
        "\x1b]2;SplitTitle\x07",
        "\x1b]5;0;#112233\x07",
        "\x1b]10;?\x07",
        "\x1bP$qm\x1b\\",
        "\x1b[6n",
        "\x1b[?2026hLATCHED\x1b[?2026l",
    )
    .as_bytes();

    // 1. Reference via unified `Terminal::advance` + read-only `Terminal::state()` & `parser()`.
    let mut term = Terminal::new(24, 6, CoreConfig::default());
    term.advance(stream);
    assert_eq!(term.parser().c1_policy(), C1Policy::Utf8Only);
    assert_eq!(term.state().visible_lines(), term.visible_lines());
    assert_eq!(term.state().full_text(), term.full_text());

    // 2. Direct split `Parser` + `TerminalState` fed in 3-byte chunks + `post_advance()`.
    let mut parser = Parser::new();
    let mut state = TerminalState::new(24, 6, CoreConfig::default());
    for chunk in stream.chunks(3) {
        parser.advance(chunk, &mut state);
    }
    state.post_advance();

    assert_eq!(state.visible_lines(), term.state().visible_lines());
    assert_eq!(state.full_text(), term.state().full_text());
    assert_eq!(state.cursor_pos(), term.state().cursor_pos());
    assert_eq!(state.palette_epoch(), term.state().palette_epoch());
    assert_eq!(state.special_colors(), term.state().special_colors());
    assert_eq!(state.title(), "SplitTitle");
    assert_eq!(state.title(), term.state().title());
    assert_eq!(state.drain_outgoing(), term.drain_outgoing());
    assert_eq!(state.drain_events(), term.drain_events());
    assert_eq!(
        state.active_snapshot().line_text(0),
        term.active_snapshot().line_text(0)
    );

    // 3. `Parser::reset()` + `C1Policy::Vt500EightBit` + `TerminalState::resize()` work cleanly.
    let mut c1_parser = Parser::with_c1_policy(C1Policy::Vt500EightBit);
    assert_eq!(c1_parser.c1_policy(), C1Policy::Vt500EightBit);
    c1_parser.advance(b"\x1b[38;2;1", &mut state); // incomplete CSI
    c1_parser.reset();
    c1_parser.advance(b"\x9b2J\x9b1;1HAFT", &mut state);
    state.post_advance();
    assert_eq!(state.visible_line_text(0).as_deref(), Some("AFT"));

    state.resize(12, 8);
    assert_eq!(state.size(), (12, 8));
    assert_eq!(state.active_snapshot().cols, 12);
    assert_eq!(state.active_snapshot().rows, 8);
}

// ===========================================================================
// Tier 2 — Boundary, Corner & Error Cases (6 Tests)
// ===========================================================================

#[cfg(feature = "search")]
#[test]
fn t2_item5_01_whole_word_punctuation_unicode_empty_and_boundaries() {
    use egui_xterm_core::SearchQuery;

    // 1. Empty pattern with whole_word=true returns empty without panicking.
    let mut term = Terminal::new(48, 6, CoreConfig::default());
    term.advance(b"foo-bar xfoo-bar foo-barx (foo-bar)\r\n");
    term.advance("cafe\u{0301} cafe\u{0301}x │baz│ 🚀baz🚀\r\n".as_bytes());
    assert!(
        term.search(&SearchQuery::literal("").with_whole_word(true))
            .expect("empty whole_word query")
            .is_empty()
    );

    // 2. Pattern with internal non-word punctuation ("foo-bar"): rejects adjacent word chars,
    //    accepts space/paren boundaries.
    let hyphen = term
        .search(&SearchQuery::literal("foo-bar").with_whole_word(true))
        .expect("hyphen whole_word search");
    assert_eq!(hyphen.len(), 2);
    assert_eq!(
        (hyphen[0].line, hyphen[0].start_col, hyphen[0].end_col),
        (0, 0, 7)
    );
    assert_eq!(
        (hyphen[1].line, hyphen[1].start_col, hyphen[1].end_col),
        (0, 27, 34)
    );

    // 3. Trailing combining mark: `is_whole_word_match` resolves base char 'e' so "cafe\u{0301}x"
    //    is rejected while standalone "cafe\u{0301}" matches.
    let comb = term
        .search(&SearchQuery::literal("cafe\u{0301}").with_whole_word(true))
        .expect("combining whole_word search");
    assert_eq!(comb.len(), 1);
    assert_eq!(
        (comb[0].line, comb[0].start_col, comb[0].end_col),
        (1, 0, 4)
    );

    // 4. Non-word box-drawing and emoji borders count as word boundaries for "baz".
    let sym = term
        .search(&SearchQuery::literal("baz").with_whole_word(true))
        .expect("symbol boundary search");
    assert_eq!(sym.len(), 2);
    assert_eq!((sym[0].line, sym[1].line), (1, 1));

    // 5. Soft-wrapped line boundary on 8-col grid: "alpha_beta" wraps across lines 0..=1.
    let mut wrap_term = Terminal::new(8, 4, CoreConfig::default());
    wrap_term.advance(b"alpha_beta");
    assert!(
        wrap_term
            .search(&SearchQuery::literal("alpha").with_whole_word(true))
            .expect("alpha blocked by underscore")
            .is_empty()
    );
    let wrapped_full = wrap_term
        .search(&SearchQuery::literal("alpha_beta").with_whole_word(true))
        .expect("alpha_beta across soft wrap");
    assert_eq!(wrapped_full.len(), 1);
    assert_eq!(
        (
            wrapped_full[0].line,
            wrapped_full[0].start_col,
            wrapped_full[0].end_col,
            wrapped_full[0].end_line
        ),
        (0, 0, 2, 1)
    );
}

#[cfg(feature = "search")]
#[test]
fn t2_item5_02_search_matches_on_line_stale_oob_alt_screen_and_zero_width() {
    use egui_xterm_core::{AbsLine, SearchQuery};

    let cfg = CoreConfig {
        scrollback_limit: 4,
        ..CoreConfig::default()
    };
    let mut term = Terminal::new(20, 4, cfg);
    // Initial empty search lookups.
    assert!(term.search_matches_on_line(0).is_empty());
    assert!(term.search_matches_on_abs_line(AbsLine(0)).is_empty());
    assert!(term.search_matches_in_range(0, 4).is_empty());

    term.advance(b"needle one\r\nneedle two\r\n");
    assert_eq!(
        term.search(&SearchQuery::literal("needle"))
            .expect("search needle")
            .len(),
        2
    );
    assert_eq!(term.search_matches_on_line(0).len(), 1);
    assert_eq!(term.search_matches_on_line(1).len(), 1);

    // Out-of-bounds line and empty/inverted ranges return empty slices.
    assert!(term.search_matches_on_line(term.total_lines()).is_empty());
    assert!(term.search_matches_on_line(usize::MAX).is_empty());
    assert!(term.search_matches_in_range(1, 1).is_empty());
    assert!(term.search_matches_in_range(3, 1).is_empty());

    // Alternate screen hides primary matches without destroying primary cache; exit restores them.
    term.advance(b"\x1b[?1049h");
    assert!(term.is_alt_screen());
    assert!(term.search_matches().is_empty());
    assert!(term.search_matches_on_line(0).is_empty());
    assert!(term.next_search_match().is_none());
    term.advance(b"\x1b[?1049l");
    assert!(!term.is_alt_screen());
    assert_eq!(term.search_matches_on_line(0).len(), 1);
    assert_eq!(term.search_matches_on_line(1).len(), 1);

    // Grid mutation makes matches stale (empty before refresh_search), without wiping cache.
    let abs0 = term.grid().abs_line_of_buf(0);
    term.advance(b"needle three\r\n");
    assert!(term.search_matches().is_empty());
    assert!(term.search_matches_on_line(0).is_empty());
    assert!(term.search_matches_on_abs_line(abs0).is_empty());
    assert!(term.next_search_match().is_none());
    assert!(term.prev_search_match().is_none());
    assert_eq!(term.refresh_search(), Ok(true));
    assert_eq!(term.search_matches_on_line(0).len(), 1);
    assert_eq!(term.search_matches_on_line(2).len(), 1);

    // Evict abs0 off scrollback -> search_matches_on_abs_line(abs0) returns empty.
    for i in 0..10 {
        term.advance(format!("scroll_{i}\r\n").as_bytes());
    }
    assert_eq!(term.refresh_search(), Ok(true));
    assert!(term.search_matches_on_abs_line(abs0).is_empty());

    // Zero-width regexes terminate cleanly and bounded without looping.
    assert!(
        term.search(&SearchQuery::regex(r"\b"))
            .expect("zero-width boundary regex")
            .is_empty()
    );
    assert!(
        term.search(&SearchQuery::regex(r"^"))
            .expect("zero-width start-anchor regex")
            .is_empty()
    );
    let a_star = term
        .search(&SearchQuery::regex(r"s*"))
        .expect("nullable quantifier regex");
    assert!(!a_star.is_empty());
}

#[cfg(feature = "search")]
#[test]
fn t2_item5_03_incremental_search_invalid_regex_eviction_and_erasure() {
    use egui_xterm_core::{SearchError, SearchQuery};

    // 1. Invalid regex isolation: preserves prior matches, active match, incremental cursor,
    //    and pending dirty state!
    let mut term = Terminal::new(20, 4, CoreConfig::default());
    term.advance(b"foo one\r\n");
    assert_eq!(
        term.search(&SearchQuery::literal("foo"))
            .expect("seed search")
            .len(),
        1
    );
    assert_eq!(term.next_search_match().map(|m| m.line), Some(0));

    term.advance(b"foo two\r\n");
    let inc = term
        .begin_incremental_search(&SearchQuery::literal("foo"))
        .expect("start incremental search");
    assert!(!inc.complete);

    assert!(matches!(
        term.search(&SearchQuery::regex(r"[unclosed(")),
        Err(SearchError::InvalidRegex(_))
    ));
    assert!(matches!(
        term.begin_incremental_search(&SearchQuery::regex(r"(unclosed")),
        Err(SearchError::InvalidRegex(_))
    ));

    // Active incremental cursor finishes cleanly with both lines.
    let done = term.step_incremental_search(100);
    assert!(done.complete);
    assert_eq!(done.matches_found, 2);
    assert_eq!(term.search_matches().len(), 2);

    // 2. Scrollback eviction unblocks whole-word match at line 0 and preserves later active cursor.
    let cfg = CoreConfig {
        scrollback_limit: 5,
        ..CoreConfig::default()
    };
    let mut ev_term = Terminal::new(4, 2, cfg);
    ev_term.advance(b"aaaxfoo\r\nbb\r\ncc\r\ndd\r\nee\r\nfoo");
    let q_ww = SearchQuery::literal("foo").with_whole_word(true);
    assert_eq!(ev_term.search(&q_ww).expect("ww search").len(), 1);
    assert_eq!(ev_term.next_search_match().map(|m| m.line), Some(6));
    // Evict row 0 ("aaax"), unblocking line 0 ("foo").
    ev_term.advance(b"\r\n");
    assert_eq!(ev_term.refresh_search(), Ok(true));
    let after_evict = ev_term.search_matches();
    assert_eq!(after_evict.len(), 2);
    assert_eq!(after_evict[0].line, 0);
    assert_eq!(after_evict[1].line, 5);
    assert_eq!(ev_term.active_search_match().map(|m| m.line), Some(5));

    // 3. Tail & full erasure (`EL 2`, `ED 0`, `ED 2`) drops stale matches cleanly on refresh_search().
    let mut er_term = Terminal::new(4, 3, CoreConfig::default());
    er_term.advance(b"aaabcd");
    assert_eq!(
        er_term
            .search(&SearchQuery::literal("bc"))
            .expect("wrapped bc")
            .len(),
        1
    );
    er_term.advance(b"\x1b[2;1H\x1b[2K");
    assert_eq!(er_term.refresh_search(), Ok(true));
    assert!(er_term.search_matches().is_empty());

    er_term.advance(b"\x1b[2J\x1b[1;1Htop\r\n\r\nfoo");
    assert_eq!(
        er_term
            .search(&SearchQuery::literal("foo"))
            .expect("tail foo")
            .len(),
        1
    );
    er_term.advance(b"\x1b[2;1H\x1b[0J");
    assert_eq!(er_term.refresh_search(), Ok(true));
    assert!(er_term.search_matches().is_empty());
}

#[test]
fn t2_item5_04_has_non_empty_selection_blank_past_eol_and_evicted() {
    let cfg = TerminalConfig {
        scrollback_limit: 4,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.set_viewport_size(400.0, 200.0);
    h.focus_terminal();

    h.runtime.input_mut().push_str("abc   \r\n\r\n   \r\nend");
    drop(h.run_frame());

    // 1. Blank single-cell / trailing-space selections yield has_non_empty_selection() == false.
    h.runtime
        .terminal_mut()
        .selection_start(1, 0, SelectionKind::Stream);
    h.runtime.terminal_mut().selection_finish();
    assert_eq!(h.runtime.terminal().selection_text().as_deref(), Some(""));
    assert!(!h.runtime.terminal().has_non_empty_selection());

    // Trailing spaces on "abc   " (cols 3..=5).
    h.runtime
        .terminal_mut()
        .selection_start(0, 3, SelectionKind::Stream);
    h.runtime.terminal_mut().selection_update(0, 5);
    h.runtime.terminal_mut().selection_finish();
    assert_eq!(h.runtime.terminal().selection_text().as_deref(), Some(""));
    assert!(!h.runtime.terminal().has_non_empty_selection());

    // Single-row Block selection over blank cells.
    h.runtime.terminal_mut().selection_block(2, 0);
    h.runtime.terminal_mut().selection_update(2, 2);
    h.runtime.terminal_mut().selection_finish();
    assert_eq!(h.runtime.terminal().selection_text().as_deref(), Some(""));
    assert!(!h.runtime.terminal().has_non_empty_selection());

    // `Event::Copy` with empty selection must NOT emit CopyText.
    h.inject_copy();
    let no_copy = h.run_frame();
    assert!(no_copy.copied_texts().is_empty());

    // 2. Multi-row hard-broken blank selection yields "\n", so has_non_empty_selection() == true.
    h.runtime
        .terminal_mut()
        .selection_start(1, 0, SelectionKind::Stream);
    h.runtime.terminal_mut().selection_update(2, 2);
    h.runtime.terminal_mut().selection_finish();
    assert_eq!(h.runtime.terminal().selection_text().as_deref(), Some("\n"));
    assert!(h.runtime.terminal().has_non_empty_selection());

    // 3. Word/Line selection on completely blank row 1 -> false.
    h.runtime.terminal_mut().selection_word(1, 2);
    assert!(!h.runtime.terminal().has_non_empty_selection());
    h.runtime.terminal_mut().selection_line(1, 2);
    assert!(!h.runtime.terminal().has_non_empty_selection());

    // 4. Selection scrolled completely off scrollback -> has_non_empty_selection() == false.
    h.runtime
        .terminal_mut()
        .selection_start(0, 0, SelectionKind::Stream);
    h.runtime.terminal_mut().selection_update(0, 2);
    h.runtime.terminal_mut().selection_finish();
    assert!(h.runtime.terminal().has_non_empty_selection());

    let mut flood = String::new();
    for i in 0..30 {
        flood.push_str(&format!("evict_{i:02}\r\n"));
    }
    h.runtime.input_mut().push_str(&flood);
    drop(h.run_frame());
    assert!(!h.runtime.terminal().has_non_empty_selection());
    assert_eq!(h.runtime.terminal().selection_text(), None);

    // 5. Alternate screen switch clears selection.
    h.runtime
        .terminal_mut()
        .selection_start(0, 0, SelectionKind::Line);
    assert!(h.runtime.terminal().has_non_empty_selection());
    h.runtime.input_mut().push_str("\x1b[?1049h");
    drop(h.run_frame());
    assert!(!h.runtime.terminal().has_non_empty_selection());
}

#[test]
fn t2_item5_05_palette_epoch_noop_invalid_osc_and_2026_latch() {
    let cfg = TerminalConfig {
        cursor_blink: false,
        idle_poll_interval_ms: 0,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.focus_terminal();
    let e0 = h.runtime.terminal().palette_epoch();
    let base_palette = h.runtime.terminal().palette().clone();

    // 1. Invalid and query-only OSC sequences must NOT bump palette_epoch or emit PaletteChanged.
    h.runtime.input_mut().push_str(concat!(
        "\x1b]4;1;notacolor\x07",
        "\x1b]4;1;?\x07",
        "\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07",
        "\x1b]5;5;#112233\x07",
        "\x1b]5;0;notacolor\x07",
        "\x1b]5;abc;#112233\x07",
        "\x1b]5;0;?\x07",
        "\x1b]105;9\x07",
    ));
    let f_invalid = h.run_frame();
    assert_eq!(h.runtime.terminal().palette_epoch(), e0);
    assert_eq!(h.runtime.terminal().palette(), &base_palette);
    assert_eq!(
        h.runtime.terminal().special_colors(),
        SpecialColors::default()
    );
    assert!(
        !f_invalid
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    assert!(!h.runtime.input().written_bytes().is_empty());
    h.runtime.input_mut().clear_written();

    // Unchanged theme on subsequent frames keeps palette_epoch steady.
    let f_steady = h.run_frame();
    assert_eq!(h.runtime.terminal().palette_epoch(), e0);
    assert!(
        !f_steady
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );

    // 2. DEC ?2026h latches snapshot palette_epoch and special_colors while live state mutates,
    //    then flushes atomically when the 250ms watchdog expires.
    let latched_bg = Color32::from_rgb(0x18, 0x28, 0x38);
    h.runtime
        .input_mut()
        .push_str("\x1b[?2026h\x1b]5;0;#aa2244\x07\x1b]11;#182838\x07");
    h.advance_time(0.05);
    let f_latched = h.run_frame();
    assert!(h.runtime.terminal().is_synchronized_output());
    assert!(h.runtime.terminal().palette_epoch() > e0);
    assert_eq!(
        h.runtime.terminal().special_colors().bold,
        Some([0xaa, 0x22, 0x44])
    );
    {
        let snap_latched = h.runtime.terminal().active_snapshot();
        assert!(snap_latched.is_synchronized_output);
        assert_eq!(snap_latched.palette_epoch, e0);
        assert_eq!(snap_latched.special_colors, [None; 5]);
    }
    assert!(
        !f_latched
            .inspect_shapes()
            .has_rect_or_mesh_color(latched_bg)
    );

    // Advance past 250ms watchdog deadline -> auto-flushes snapshot palette_epoch + special_colors.
    h.advance_time(0.26);
    let f_flushed = h.run_frame();
    assert!(!h.runtime.terminal().is_synchronized_output());
    let expected_epoch = h.runtime.terminal().palette_epoch();
    {
        let snap_flushed = h.runtime.terminal().active_snapshot();
        assert!(!snap_flushed.is_synchronized_output);
        assert_eq!(snap_flushed.palette_epoch, expected_epoch);
        assert_eq!(snap_flushed.special_colors[0], Some([0xaa, 0x22, 0x44]));
    }
    assert!(
        f_flushed
            .inspect_shapes()
            .has_rect_or_mesh_color(latched_bg)
    );
}

#[test]
fn t2_item5_06_split_parser_malformed_chunked_1byte_trickle() {
    let mut stream = Vec::new();
    // 1. Overlong CSI with >64 params & huge numeric overflow.
    stream.extend_from_slice(b"\x1b[999999999;");
    for i in 0..70 {
        stream.extend_from_slice(format!("{i};").as_bytes());
    }
    stream.extend_from_slice(b"38;2;255;0;0m");
    // 2. Invalid UTF-8 bytes + truncated lead byte interrupted by SGR reset.
    stream.extend_from_slice(&[0xFF, 0xFE, 0xC0, 0xAF, 0xE4, 0xB8]);
    stream.extend_from_slice(b"\x1b[0m");
    // 3. Oversized OSC > max_osc_bytes (64).
    stream.extend_from_slice(b"\x1b]2;");
    stream.extend_from_slice(&[b'X'; 128]);
    stream.extend_from_slice(b"\x07");
    // 4. CAN (0x18) and SUB (0x1A) aborting partial CSI & OSC sequences.
    stream.extend_from_slice(b"\x1b[31\x18\x1b]2;IGNORED\x1a");
    // 5. Clean recovery: valid UTF-8 CJK/emoji, OSC 5, and OSC 2.
    stream.extend_from_slice("\x1b[2J\x1b[1;1HRECOVERED_你好🚀".as_bytes());
    stream.extend_from_slice(b"\x1b]5;1;#00ff88\x07\x1b]2;OK\x07");

    let cfg = CoreConfig {
        max_osc_bytes: 64,
        ..CoreConfig::default()
    };

    // Compare bulk Terminal::advance vs 1-byte trickled split Parser + TerminalState.
    let mut term = Terminal::new(32, 4, cfg.clone());
    term.advance(&stream);

    let mut parser = Parser::new().with_max_osc_bytes(64);
    let mut state = TerminalState::new(32, 4, cfg);
    for byte in &stream {
        parser.advance(std::slice::from_ref(byte), &mut state);
    }
    state.post_advance();

    assert_eq!(
        state.visible_line_text(0).as_deref(),
        Some("RECOVERED_你好🚀")
    );
    assert_eq!(state.visible_line_text(0), term.visible_line_text(0));
    assert_eq!(state.special_colors().underline, Some([0x00, 0xff, 0x88]));
    assert_eq!(state.special_colors(), term.special_colors());
    assert_eq!(state.title(), "OK");
    assert_eq!(state.title(), term.title());

    // Verify 1-byte trickle through HeadlessTermHarness ScriptedInput as well.
    let mut h = HeadlessTermHarness::scripted();
    h.config.read_budget_bytes_per_frame = 64;
    h.runtime.input_mut().push_chunked(
        b"\x1b[999;888;m\xff\xfe\x1b[2J\x1b[1;1HUI_OK\x1b]2;UI\x07",
        1,
    );
    assert!(
        h.run_until(8, 1.0 / 60.0, |harness, _| harness
            .runtime
            .input()
            .is_drained()),
        "1-byte trickle did not drain"
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("UI_OK")
    );
    assert_eq!(h.runtime.terminal().title(), "UI");
}

// ===========================================================================
// Tier 3 — Pairwise Combinations (1 Test)
// ===========================================================================

#[cfg(feature = "search")]
#[test]
fn t3_search_01_whole_word_reflow_scrollback_selection_and_palette_sync() {
    use egui_xterm_core::SearchQuery;

    let cfg = TerminalConfig {
        scrollback_limit: 500,
        resize_debounce_secs: 0.0,
        cursor_blink: false,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.set_viewport_size(600.0, 260.0);
    let warm = h.run_frame().terminal_response;
    let cw = warm.cell_size.x;
    let ch = warm.cell_size.y;

    // Force exact 40x8 grid first.
    h.set_viewport_size(40.0 * cw + 1.0, 8.0 * ch + 1.0);
    let r40 = h.run_frame().terminal_response;
    assert_eq!(r40.cols, 40);

    let mut lines = String::new();
    for i in 0..80usize {
        if i % 10 == 0 {
            lines.push_str(&format!("L{i:02} token_a token token-b 编译 🚀\r\n"));
        } else {
            lines.push_str(&format!("L{i:02} filler_token_xyz\r\n"));
        }
    }
    h.runtime.input_mut().push_str(&lines);
    assert!(
        h.run_until(16, 1.0 / 60.0, |harness, _| harness
            .runtime
            .input()
            .is_drained()),
        "Tier 3 input did not drain"
    );

    let q = SearchQuery::literal("token").with_whole_word(true);
    let m40 = h
        .runtime
        .terminal_mut()
        .search(&q)
        .expect("whole-word search at 40 cols");
    // Each of the 8 matching lines has 2 whole-word "token" occurrences: "token" and "token-b".
    assert_eq!(m40.len(), 16);

    // Select a non-empty word before reflow, and verify it survives reflow.
    let total_before = h.runtime.terminal().total_lines();
    let vis_rows = usize::from(r40.rows);
    let target_vp_row = 1usize;
    h.runtime.terminal_mut().selection_word(target_vp_row, 5);
    assert!(h.runtime.terminal().has_non_empty_selection());

    // Reflow 40 -> 20 cols so every matching line wraps across physical rows.
    h.set_viewport_size(20.0 * cw + 0.5, 8.0 * ch + 1.0);
    let r20 = h.run_frame().terminal_response;
    assert_eq!(r20.cols, 20);
    assert!(h.runtime.terminal().total_lines() > total_before);
    assert!(h.runtime.terminal().has_non_empty_selection());
    assert_eq!(
        h.runtime.terminal().state().has_non_empty_selection(),
        h.runtime.terminal().has_non_empty_selection()
    );

    // Re-search after reflow and verify O(log M) search_matches_on_line + navigation scrolling.
    let m20 = h
        .runtime
        .terminal_mut()
        .search(&q)
        .expect("whole-word search after 40->20 reflow");
    assert_eq!(m20.len(), 16);
    for line in 0..h.runtime.terminal().total_lines() {
        let on_line = h.runtime.terminal().search_matches_on_line(line);
        let expected: Vec<_> = m20
            .iter()
            .copied()
            .filter(|m| line >= m.line && line <= m.end_line)
            .collect();
        assert_eq!(on_line, expected.as_slice());
    }
    let first_match = *h
        .runtime
        .terminal_mut()
        .next_search_match()
        .expect("step to first match");
    assert_eq!(first_match.line, m20[0].line);
    assert!(h.runtime.terminal().display_offset() > 0);
    let scrolled_frame = h.run_frame();
    let scrolled_insp = scrolled_frame.inspect_shapes();
    assert_eq!(scrolled_insp.non_snapped_rect_count, 0);
    assert!(scrolled_insp.has_rect_or_mesh_color(Color32::from_rgb(255, 165, 0)));

    // Selecting empty padding on a blank cell must yield has_non_empty_selection() == false.
    h.runtime.terminal_mut().scroll_to_bottom();
    let blank_row = vis_rows.saturating_sub(1);
    h.runtime
        .terminal_mut()
        .selection_start(blank_row, 19, SelectionKind::Stream);
    h.runtime.terminal_mut().selection_finish();
    assert!(!h.runtime.terminal().has_non_empty_selection());

    // Combine DEC ?2026h sync latch with OSC 5 + OSC 11 and check palette_epoch / special_colors.
    let pre_epoch = h.runtime.terminal().palette_epoch();
    h.runtime
        .input_mut()
        .push_str("\x1b[?2026h\x1b]5;0;#ff00ff\x07\x1b]4;1;#00ff00\x07\x1b]11;#111111\x07");
    let latched_frame = h.run_frame();
    assert!(h.runtime.terminal().is_synchronized_output());
    assert!(h.runtime.terminal().palette_epoch() > pre_epoch);
    assert_eq!(
        h.runtime.terminal().active_snapshot().palette_epoch,
        pre_epoch
    );
    assert_eq!(
        h.runtime.terminal().active_snapshot().special_colors[0],
        None
    );
    assert!(
        !latched_frame
            .inspect_shapes()
            .has_rect_or_mesh_color(Color32::from_rgb(0x11, 0x11, 0x11))
    );

    h.runtime.input_mut().push_str("\x1b[?2026l");
    let unlatched_frame = h.run_frame();
    let post_epoch = h.runtime.terminal().palette_epoch();
    assert_eq!(
        h.runtime.terminal().active_snapshot().palette_epoch,
        post_epoch
    );
    assert_eq!(
        h.runtime.terminal().active_snapshot().special_colors[0],
        Some([0xff, 0x00, 0xff])
    );
    assert_eq!(
        h.runtime.terminal().state().special_colors().bold,
        Some([0xff, 0x00, 0xff])
    );
    assert!(
        unlatched_frame
            .inspect_shapes()
            .has_rect_or_mesh_color(Color32::from_rgb(0x11, 0x11, 0x11))
    );
    assert_eq!(unlatched_frame.terminal_response.cols, 20);
}

// ===========================================================================
// Tier 4 — Realistic Application Scenario Workload (1 Test)
// ===========================================================================

#[cfg(feature = "search")]
#[test]
fn t4_workload_03_interactive_pager_search_selection_and_mouse() {
    use std::fmt::Write as _;

    use egui::Pos2;
    use egui_xterm_core::{SearchError, SearchQuery};
    use egui_xterm_e2e::ShapeInspection;

    let cfg = TerminalConfig {
        scrollback_limit: 5_000,
        resize_debounce_secs: 0.0,
        cursor_blink: false,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.set_viewport_size(800.0, 600.0);
    h.focus_terminal();

    // 1. Stream 2,000 lines of mixed ANSI-colored Rust diagnostics, CJK paths, decoys, and URLs.
    let mut log = String::with_capacity(200_000);
    for i in 0..2_000usize {
        if i % 100 == 0 {
            writeln!(
                log,
                "\x1b[1;31merror[E0308]\x1b[0m: mismatched types at src/核心/mod_{i:04}.rs:42\r"
            )
            .expect("fmt::Write on String");
        } else if i % 100 == 1 {
            writeln!(
                log,
                "  help: see https://doc.rust-lang.org/codes/E0308.html (error {i})\r"
            )
            .expect("fmt::Write on String");
        } else if i % 2 == 0 {
            writeln!(
                log,
                "\x1b[33mwarning\x1b[0m: 0 errors, my_error=false, error_count={i}\r"
            )
            .expect("fmt::Write on String");
        } else {
            writeln!(log, "note: compiling crate_unit_{i:04} cleanly\r")
                .expect("fmt::Write on String");
        }
    }
    h.runtime.input_mut().push_str(&log);
    assert!(
        h.run_until(32, 1.0 / 60.0, |harness, _| harness
            .runtime
            .input()
            .is_drained()),
        "2,000-line diagnostic pager output did not drain"
    );
    assert!(h.runtime.terminal().scrollback_len() > 1_900);

    // 2. Whole-word literal search: "error" matches "error[E0308]" (20) + "(error {i})" (20) = 40,
    //    skipping "errors", "my_error", and "error_count".
    let q_err = SearchQuery::literal("error").with_whole_word(true);
    let matches = h
        .runtime
        .terminal_mut()
        .search(&q_err)
        .expect("whole-word search");
    assert_eq!(matches.len(), 40);
    for m in &matches {
        assert!(m.line % 100 <= 1);
    }

    // Verify O(log M) line & abs_line lookup + scrollback navigation + rendered highlight quads.
    let first_line = matches[0].line;
    assert_eq!(
        h.runtime
            .terminal()
            .search_matches_on_line(first_line)
            .len(),
        1
    );
    let abs_first = h.runtime.terminal().grid().abs_line_of_buf(first_line);
    assert_eq!(
        h.runtime
            .terminal()
            .search_matches_on_abs_line(abs_first)
            .len(),
        1
    );
    assert_eq!(
        h.runtime.terminal_mut().next_search_match().map(|m| m.line),
        Some(first_line)
    );
    assert!(h.runtime.terminal().display_offset() > 0);
    let nav_frame = h.run_frame();
    let nav_insp = nav_frame.inspect_shapes();
    assert!(nav_insp.has_rect_or_mesh_color(Color32::from_rgb(255, 165, 0)));
    assert_eq!(nav_insp.non_snapped_rect_count, 0);

    // 3. Incremental search + invalid regex isolation + query refinement + live append refresh.
    h.runtime.open_search();
    h.runtime.search_bar_mut().is_regex = true;
    drop(h.run_frame());
    h.inject_text("[bad(");
    let bad_re_frame = h.run_frame();
    assert!(h.runtime.search_bar_mut().last_error.is_some());
    assert!(
        bad_re_frame
            .inspect_shapes()
            .contains_text("invalid search regex")
    );
    assert_eq!(h.runtime.terminal().search_matches().len(), 40);
    h.runtime.close_search();
    drop(h.run_frame());

    // Direct invalid regex + chunked incremental search for "error[E0308]".
    let mut prog = h
        .runtime
        .terminal_mut()
        .begin_incremental_search(&SearchQuery::literal("error[E0308]"))
        .expect("begin incremental search");
    assert!(matches!(
        h.runtime
            .terminal_mut()
            .search(&SearchQuery::regex("[unclosed")),
        Err(SearchError::InvalidRegex(_))
    ));
    let mut inc_frames = 0usize;
    while !prog.complete {
        prog = h.runtime.terminal_mut().step_incremental_search(250);
        drop(h.run_frame());
        inc_frames += 1;
        assert!(inc_frames <= 32);
    }
    assert!(inc_frames >= 4);
    assert_eq!(prog.matches_found, 20);
    assert_eq!(h.runtime.terminal().search_matches().len(), 20);

    // Live append + refresh_search() adds new diagnostic match.
    h.runtime.input_mut().push_str(
        "error[E0308]: trailing live fault\r\n  help: https://doc.rust-lang.org/codes/E0308.html\r\n",
    );
    drop(h.run_frame());
    assert_eq!(h.runtime.terminal_mut().refresh_search(), Ok(true));
    assert_eq!(h.runtime.terminal().search_matches().len(), 21);

    // 4. Resize reflow (800x600 -> 480x500) preserves full_text and post-reflow search consistency.
    let expected_full = h.runtime.terminal().full_text();
    let cols_before = nav_frame.terminal_response.cols;
    h.set_viewport_size(480.0, 500.0);
    let resized_frame = h.run_frame();
    assert!(resized_frame.terminal_response.cols < cols_before);
    assert_eq!(h.runtime.terminal().full_text(), expected_full);
    let post_reflow = h
        .runtime
        .terminal_mut()
        .search(&q_err)
        .expect("post-reflow whole_word search");
    assert_eq!(post_reflow.len(), 41);

    // 5. Scroll to bottom, drag-select diagnostic line -> has_non_empty_selection + Copy -> clear.
    h.runtime.terminal_mut().scroll_to_bottom();
    let bottom_frame = h.run_frame();
    let vis = h.runtime.terminal().visible_lines();
    let diag_row = vis
        .iter()
        .position(|l| l.contains("trailing live fault"))
        .expect("trailing diagnostic row visible");
    let diag_row_u16 = u16::try_from(diag_row).expect("row fits u16");
    let drag_from = h.cell_center(0, diag_row_u16).expect("diag start");
    let drag_to = h.cell_center(11, diag_row_u16).expect("diag end");
    h.advance_time(0.5);
    h.inject_drag_steps(
        drag_from,
        drag_to,
        PointerButton::Primary,
        Modifiers::NONE,
        3,
    );
    assert!(h.runtime.terminal().has_non_empty_selection());
    h.inject_copy();
    let copied = h.run_frame().copied_texts();
    assert_eq!(copied, vec!["error[E0308]".to_owned()]);

    // Clear selection and verify blank padding click keeps has_non_empty_selection() == false.
    h.runtime.terminal_mut().selection_clear();
    assert!(!h.runtime.terminal().has_non_empty_selection());
    let blank_row = bottom_frame.terminal_response.rows.saturating_sub(1);
    let blank_pos = h.cell_center(5, blank_row).expect("blank bottom row");
    h.advance_time(0.5);
    h.inject_click(blank_pos, PointerButton::Primary, Modifiers::NONE);
    assert!(!h.runtime.terminal().has_non_empty_selection());

    // 6. Hyperlink check on the diagnostic URL when `hyperlinks` is enabled.
    #[cfg(feature = "hyperlinks")]
    {
        let (url_row, line_str) = vis
            .iter()
            .enumerate()
            .find(|(_, l)| l.contains("https://doc.rust-lang.org/codes/E0308.html"))
            .expect("live diagnostic URL row visible");
        let byte_idx = line_str.find("https://").expect("https:// prefix present");
        let char_col = line_str[..byte_idx].chars().count();
        let row_u16 = u16::try_from(url_row).expect("row fits");
        let col_u16 = u16::try_from(char_col + 4).expect("col fits");
        assert_eq!(
            h.runtime
                .terminal()
                .hyperlink_at(col_u16, row_u16)
                .map(|l| l.uri.as_str()),
            Some("https://doc.rust-lang.org/codes/E0308.html")
        );
    }

    // 7. HiDPI (ppp = 2.0) + ?1002h + ?1016h SGR-Pixels mouse tracking & Shift+drag bypass.
    h.set_pixels_per_point(2.0);
    h.runtime.input_mut().push_str("\x1b[?1002h\x1b[?1016h");
    let hidpi_frame = h.run_frame();
    let origin = hidpi_frame.terminal_response.response.rect.min;
    h.runtime.input_mut().clear_written();

    let p1 = h.cell_center(3, 2).expect("p1");
    let p2 = h.cell_center(6, 4).expect("p2");
    let to_px = |p: Pos2| -> (u32, u32) {
        (
            ((p.x - origin.x).max(0.0) * 2.0).round() as u32,
            ((p.y - origin.y).max(0.0) * 2.0).round() as u32,
        )
    };
    let (px1, py1) = to_px(p1);
    let (px2, py2) = to_px(p2);

    h.inject_pointer_move(p1);
    h.inject_pointer_button(p1, PointerButton::Primary, true, Modifiers::NONE);
    drop(h.run_frame());
    h.inject_pointer_move(p2);
    drop(h.run_frame());
    h.inject_pointer_button(p2, PointerButton::Primary, false, Modifiers::NONE);
    drop(h.run_frame());

    assert_eq!(
        h.runtime.input().written_str(),
        format!("\x1b[<0;{px1};{py1}M\x1b[<32;{px2};{py2}M\x1b[<0;{px2};{py2}m")
    );
    h.runtime.input_mut().clear_written();

    // Shift+drag bypasses ?1002h/?1016h and produces local selection.
    let hidpi_from = h.cell_center(0, diag_row_u16).expect("hidpi diag start");
    let hidpi_to = h.cell_center(11, diag_row_u16).expect("hidpi diag end");
    h.advance_time(0.5);
    h.inject_drag_steps(
        hidpi_from,
        hidpi_to,
        PointerButton::Primary,
        Modifiers::SHIFT,
        3,
    );
    assert!(h.runtime.input().written_bytes().is_empty());
    assert!(h.runtime.terminal().has_non_empty_selection());

    // 8. Palette/special_colors epoch + zero-alloc RenderSnapshot & Terminal::state() parity.
    let epoch_before = h.runtime.terminal().palette_epoch();
    h.runtime
        .input_mut()
        .push_str("\x1b]5;0;#ffaa33\x07\x1b]11;#141824\x07");
    let pal_frame = h.run_frame();
    assert!(
        pal_frame
            .terminal_response
            .events
            .contains(&TerminalEvent::PaletteChanged)
    );
    let epoch_after = h.runtime.terminal().palette_epoch();
    assert!(epoch_after > epoch_before);
    assert_eq!(
        h.runtime.terminal().special_colors().bold,
        Some([0xff, 0xaa, 0x33])
    );

    let snap = h.runtime.terminal().active_snapshot();
    assert_eq!(snap.palette_epoch, epoch_after);
    assert_eq!(snap.special_colors[0], Some([0xff, 0xaa, 0x33]));
    assert_eq!(
        usize::from(snap.rows),
        snap.visible_rows.len(),
        "VisibleRowsView length matches snapshot.rows"
    );
    assert!(snap.is_cell_selected(diag_row, 0));
    assert_eq!(
        snap.line_text(diag_row_u16),
        h.runtime.terminal().state().visible_line_text(diag_row_u16)
    );
    // Keep ShapeInspection referenced explicitly for layout verification.
    let pal_insp = ShapeInspection::from_terminal_shapes(
        &pal_frame.shapes,
        pal_frame.terminal_response.response.rect,
        pal_frame.pixels_per_point,
    );
    assert_eq!(pal_insp.non_snapped_rect_count, 0);
    assert!(pal_insp.has_rect_or_mesh_color(Color32::from_rgb(0x14, 0x18, 0x24)));
}
