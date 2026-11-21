//! Tier 4 — Real-World Application Scenario E2E Tests (`W1`..`W10`, 10 workloads).
//!
//! Each workload exercises multiple interacting features across realistic multi-frame
//! user sessions, feature-gated to match the active workspace feature selection.

#[cfg(feature = "kitty-keyboard")]
#[test]
fn w1_fullscreen_modal_tui_editor() {
    use egui::{Color32, Key, Modifiers, PointerButton};
    use egui_xterm::CursorShape;
    use egui_xterm_e2e::HeadlessTermHarness;

    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(800.0, 600.0);
    h.runtime.input_mut().push_str("user@host:~/proj$ ");
    let primary_verts = h.run_frame().inspect_shapes().text_mesh_vertices().to_vec();
    assert!(!primary_verts.is_empty());

    // Enter alt screen, enable button-event + SGR mouse, bracketed paste, Kitty flags 3, bar cursor.
    h.runtime
        .input_mut()
        .push_str("\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[>3u\x1b[5 q");
    let _ = h.run_frame();
    assert!(h.runtime.terminal().is_alt_screen());
    assert_eq!(h.runtime.terminal().cursor_shape(), CursorShape::Bar);

    // Partial synchronized frame must stay latched until ?2026l.
    h.runtime.input_mut().push_str(
        "\x1b[?2026h\x1b[2J\x1b[H\x1b[38;2;86;156;214mfn \x1b[38;2;220;220;170mmain\x1b[0m() {\r\n    println!(\"hi\");",
    );
    let mid_sync = h.run_frame().inspect_shapes();
    assert!(h.runtime.terminal().is_synchronized_output());
    assert!(mid_sync.text_mesh_vertices().is_empty());

    h.runtime.input_mut().push_str("\r\n}\x1b[?2026l");
    let committed = h.run_frame().inspect_shapes();
    assert!(!h.runtime.terminal().is_synchronized_output());
    assert!(
        h.runtime
            .terminal()
            .visible_line_text(0)
            .as_deref()
            .is_some_and(|l| l.contains("main()"))
    );
    assert!(!committed.text_mesh_vertices().is_empty());
    assert!(committed.has_text_fg_color(Color32::from_rgb(86, 156, 214)));

    h.focus_terminal();
    // Settle 1 frame after focus gain so egui's Memory::set_focus_lock_filter
    // (which requires had_focus_last_frame == true) is armed before Esc/Tab/Arrows.
    let _ = h.run_frame();
    h.runtime.input_mut().clear_written();

    h.tap_key(Key::Escape, Modifiers::NONE);
    h.tap_key(Key::ArrowDown, Modifiers::NONE);
    h.tap_key(Key::Tab, Modifiers::NONE);
    h.tap_key(Key::S, Modifiers::CTRL);
    h.inject_paste("lineA\nlineB");
    let click_pos = h.cell_center(4, 1).expect("cell (4, 1)");
    h.inject_click(click_pos, PointerButton::Primary, Modifiers::NONE);
    let after_input = h.run_frame();

    assert!(after_input.terminal_response.response.has_focus());
    let written = h.runtime.input().written_str();
    assert!(
        written.contains("\x1b[27"),
        "missing Kitty Esc: {written:?}"
    );
    assert!(written.contains('u'), "missing Kitty CSI u: {written:?}");
    assert!(
        written.contains("\x1b[200~lineA\nlineB\x1b[201~"),
        "missing bracketed paste: {written:?}"
    );
    assert!(
        written.contains("\x1b[<0;5;2M") && written.contains("\x1b[<0;5;2m"),
        "missing SGR mouse click: {written:?}"
    );

    // Exit alt screen -> primary shell prompt restored cleanly.
    h.runtime.input_mut().push_str("\x1b[?1049l");
    let restored = h.run_frame();
    assert!(!h.runtime.terminal().is_alt_screen());
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("user@host:~/proj$")
    );
    assert_eq!(
        restored.inspect_shapes().text_mesh_vertices(),
        primary_verts.as_slice()
    );
}

#[cfg(feature = "vector-glyphs")]
#[test]
fn w2_60fps_system_monitor() {
    use std::fmt::Write as _;

    use egui::{Color32, Modifiers, MouseWheelUnit, PointerButton, Vec2};
    use egui_xterm_e2e::{HeadlessTermHarness, ShapeInspection};

    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(1024.0, 640.0);
    h.runtime.input_mut().push_str("\x1b[?1000;1006h");
    h.focus_terminal();
    h.runtime.input_mut().clear_written();

    // Verify mid-sync hold on frame 0 before running 60 consecutive updates.
    h.runtime
        .input_mut()
        .push_str("\x1b[?2026h\x1b[H┌──CPU_00──┐\r\n│▁▃▅▇█ ⣿│");
    let latched = h.run_frame().inspect_shapes();
    assert!(latched.text_mesh_vertices().is_empty());
    h.runtime
        .input_mut()
        .push_str("\r\n└──────────┘\x1b[?2026l");
    let committed = h.run_frame().inspect_shapes();
    assert!(
        h.runtime
            .terminal()
            .visible_line_text(0)
            .as_deref()
            .is_some_and(|l| l.contains("CPU_00"))
    );
    assert!(!committed.text_mesh_vertices().is_empty());

    for frame in 0..60usize {
        h.advance_time(1.0 / 60.0);
        let pct = (frame * 7) % 100;
        let mut payload = String::with_capacity(512);
        write!(
            payload,
            "\x1b[?2026h\x1b[H┌──────System Monitor F{frame:02}──────┐\r\n\
             │CPU \x1b[48:2::30:140:70m▁▃▅▇█\x1b[0m {pct:02}% ⣿⣿⣿⣿│\r\n\
             │RAM \x1b[48:2::40:90:180m▁▃▅▇ \x1b[0m {}% ⡇⡇⡇⡇│\r\n\
             └───────────────────────────────┘\x1b[?2026l",
            (pct + 15) % 100
        )
        .expect("String fmt::Write is infallible");
        h.runtime.input_mut().push_str(&payload);

        if frame == 30 {
            let row_pos = h.cell_center(10, 5).expect("cell (10, 5)");
            h.inject_pointer_move(row_pos);
            h.inject_pointer_button(row_pos, PointerButton::Primary, true, Modifiers::NONE);
            h.inject_pointer_button(row_pos, PointerButton::Primary, false, Modifiers::NONE);
            h.inject_scroll(MouseWheelUnit::Line, Vec2::new(0.0, 1.0), Modifiers::NONE);
        }

        let out = h.run_frame();
        assert!(!h.runtime.terminal().is_synchronized_output());
        let insp = ShapeInspection::from_terminal_shapes(
            &out.shapes,
            out.terminal_response.response.rect,
            out.pixels_per_point,
        );
        assert!(
            h.runtime
                .terminal()
                .visible_line_text(0)
                .as_deref()
                .is_some_and(|l| l.contains(&format!("F{frame:02}")))
                && !insp.text_mesh_vertices().is_empty(),
            "missing rendered frame label F{frame:02}"
        );
        assert!(
            insp.has_rect_or_mesh_color(Color32::from_rgb(30, 140, 70)),
            "missing CPU bar TrueColor fill at frame {frame}"
        );
        assert!(
            insp.has_rect_or_mesh_color(Color32::from_rgb(40, 90, 180)),
            "missing RAM bar TrueColor fill at frame {frame}"
        );
        assert_eq!(
            insp.non_snapped_rect_count, 0,
            "unsnapped rect at monitor frame {frame}"
        );
        let stats = h.render_to_mesh_stats();
        assert!(
            stats.total_vertices < 150_000,
            "frame {frame} mesh vertex count exceeded O(grid): {}",
            stats.total_vertices
        );
    }

    let written = h.runtime.input().written_str();
    assert!(
        written.contains("\x1b[<0;11;6M") && written.contains("\x1b[<0;11;6m"),
        "missing SGR click in monitor: {written:?}"
    );
    assert!(
        written.contains("\x1b[<64;11;6M") || written.contains("\x1b[<65;11;6M"),
        "missing SGR wheel in monitor: {written:?}"
    );
}

#[cfg(all(
    feature = "search",
    feature = "hyperlinks",
    feature = "shell-integration"
))]
#[test]
fn w3_interactive_developer_shell() {
    use std::fmt::Write as _;

    use egui::{Modifiers, PointerButton};
    use egui_xterm::{TerminalConfig, TerminalEvent};
    use egui_xterm_core::SearchQuery;
    use egui_xterm_e2e::HeadlessTermHarness;

    let cfg = TerminalConfig {
        scrollback_limit: 500,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.set_viewport_size(800.0, 600.0);

    let mut transcript =
        String::from("\x1b]133;A\x07$ \x1b]133;B\x07cargo test -p ok\r\n\x1b]133;C\x07");
    for i in 0..60 {
        writeln!(transcript, "\x1b[32mok\x1b[0m case_{i:02}\r")
            .expect("String fmt::Write is infallible");
    }
    transcript.push_str("\x1b]133;D;0\x07");
    transcript.push_str(
        "\x1b]133;A\x07$ \x1b]133;B\x07cargo test -p fail\r\n\x1b]133;C\x07\
         test result: \x1b[31mFAILED\x1b[0m. 1 failed\r\n\
         \x1b]8;;file:///src/lib.rs:42\x07src/lib.rs:42\x1b]8;;\x07\r\n\
         See https://docs.rs/egui\r\n\
         \x1b]133;D;101\x07",
    );
    h.runtime.input_mut().push_str(&transcript);

    let mut saw_exit_101 = false;
    for _ in 0..8 {
        let out = h.run_frame();
        if out
            .terminal_response
            .events
            .contains(&TerminalEvent::CommandFinished {
                exit_code: Some(101),
            })
        {
            saw_exit_101 = true;
        }
        if h.runtime.input().is_drained() && saw_exit_101 {
            break;
        }
    }
    assert!(h.runtime.input().is_drained());
    assert!(saw_exit_101, "missing CommandFinished(Some(101))");
    assert!(!h.runtime.terminal().shell_zones().is_empty());

    // Scroll up into history, search "FAILED", and jump between prompts.
    h.runtime.terminal_mut().scroll_display(10);
    let matches = h
        .runtime
        .terminal_mut()
        .search(&SearchQuery::literal("FAILED"))
        .expect("search FAILED");
    assert_eq!(matches.len(), 1);
    assert!(h.runtime.terminal_mut().jump_to_prompt(-1));

    // Return to bottom, drag-select error block with non-space endpoints, and copy.
    h.runtime.terminal_mut().scroll_to_bottom();
    h.focus_terminal();
    let visible = h.runtime.terminal().visible_lines();
    let failed_row = visible
        .iter()
        .position(|l| l.contains("test result: FAILED"))
        .expect("FAILED row visible");
    let link_row = visible
        .iter()
        .position(|l| l.contains("src/lib.rs:42"))
        .expect("OSC 8 row visible");
    let docs_row = visible
        .iter()
        .position(|l| l.contains("https://docs.rs/egui"))
        .expect("implicit URL row visible");

    let failed_row_u16 = u16::try_from(failed_row).expect("row fits u16");
    let from = h
        .cell_center(0, failed_row_u16)
        .expect("start of FAILED row");
    let to = h
        .cell_center(18, failed_row_u16)
        .expect("end of 'test result: FAILED'");
    h.inject_drag_steps(from, to, PointerButton::Primary, Modifiers::NONE, 4);
    h.inject_copy();
    let copy_out = h.run_frame();
    assert_eq!(
        copy_out.copied_texts(),
        vec!["test result: FAILED".to_owned()]
    );

    // Click OSC 8 link and verify implicit URL detection.
    let link_row_u16 = u16::try_from(link_row).expect("row fits u16");
    let link_cell = h.cell_center(2, link_row_u16).expect("OSC 8 link cell");
    h.inject_click(link_cell, PointerButton::Primary, Modifiers::NONE);
    let click_out = h.last_outcome().expect("link click outcome");
    assert!(
        click_out
            .opened_urls()
            .iter()
            .any(|u| u == "file:///src/lib.rs:42")
    );
    assert!(click_out.terminal_response.events.iter().any(|ev| {
        matches!(
            ev,
            TerminalEvent::HyperlinkActivated { uri, .. } if uri == "file:///src/lib.rs:42"
        )
    }));

    let docs_row_u16 = u16::try_from(docs_row).expect("row fits u16");
    assert_eq!(
        h.runtime
            .terminal()
            .hyperlink_at(8, docs_row_u16)
            .map(|l| l.uri.as_str()),
        Some("https://docs.rs/egui")
    );
}

#[test]
fn w4_multipane_split_ide_workspace_resize_and_dpi() {
    use egui::{Event, Key, Modifiers, PointerButton, pos2};
    use egui_xterm::ScaleMode;
    use egui_xterm_e2e::{MultiPaneHarness, ScriptedInput};

    let left_text = "LEFT_BUILD_OUTPUT_LONG_LINE_0123456789ABCDEF";
    let mut panes = MultiPaneHarness::new(
        ScriptedInput::from_bytes(left_text.as_bytes()),
        ScriptedInput::from_bytes(b"MCU> "),
    );
    panes.left_config.scale_mode = ScaleMode::ReflowGrid;
    panes.left_config.resize_debounce_secs = 0.0;
    panes.right_config.scale_mode = ScaleMode::AutoScaleToFit { cols: 80, rows: 24 };
    panes.right_config.resize_debounce_secs = 0.0;

    panes.set_viewport_size(1280.0, 720.0);
    panes.set_pixels_per_point(1.0);
    let (l1, r1, m1) = panes.run_split_frame();
    let expected_left_full = panes.left.terminal().full_text();
    assert_eq!((r1.cols, r1.rows), (80, 24));
    assert!((m1.pixels_per_point - 1.0).abs() < 1e-6);
    assert!(!m1.inspect_shapes().text_mesh_vertices().is_empty());
    assert!(
        m1.inspect_shapes()
            .text_entries
            .iter()
            .all(|e| (e.pixels_per_point - 1.0).abs() < 1e-6)
    );

    panes.set_viewport_size(760.0, 480.0);
    panes.set_pixels_per_point(2.0);
    let (l2, r2, m2) = panes.run_split_frame();
    assert!(l2.cols < l1.cols);
    assert_eq!(panes.left.terminal().full_text(), expected_left_full);
    assert_eq!((r2.cols, r2.rows), (80, 24));
    assert!(r2.effective_font_size < r1.effective_font_size);
    assert!((m2.pixels_per_point - 2.0).abs() < 1e-6);
    assert!(!m2.inspect_shapes().text_mesh_vertices().is_empty());
    assert!(
        m2.inspect_shapes()
            .text_entries
            .iter()
            .all(|e| (e.pixels_per_point - 2.0).abs() < 1e-6)
    );

    panes.set_viewport_size(1400.0, 800.0);
    panes.set_pixels_per_point(1.25);
    let (l3, r3, m3) = panes.run_split_frame();
    assert!(l3.cols > l2.cols);
    assert_eq!(panes.left.terminal().full_text(), expected_left_full);
    assert_eq!((r3.cols, r3.rows), (80, 24));
    assert!(r3.effective_font_size > r2.effective_font_size);
    assert!((m3.pixels_per_point - 1.25).abs() < 1e-6);
    assert!(!m3.inspect_shapes().text_mesh_vertices().is_empty());
    assert!(
        m3.inspect_shapes()
            .text_entries
            .iter()
            .all(|e| (e.pixels_per_point - 1.25).abs() < 1e-6)
    );

    // Click left pane (200, 200) -> type "cargo check" + Key::Enter
    panes.queue_event(Event::PointerMoved(pos2(200.0, 200.0)));
    panes.queue_event(Event::PointerButton {
        pos: pos2(200.0, 200.0),
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    panes.queue_event(Event::PointerButton {
        pos: pos2(200.0, 200.0),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    let _ = panes.run_split_frame();
    panes.queue_event(Event::Text("cargo check".to_owned()));
    panes.queue_event(Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    });
    panes.queue_event(Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: Modifiers::NONE,
    });
    let _ = panes.run_split_frame();

    // Click right pane (1000, 200) -> type "reset" + Key::Enter
    panes.queue_event(Event::PointerMoved(pos2(1000.0, 200.0)));
    panes.queue_event(Event::PointerButton {
        pos: pos2(1000.0, 200.0),
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    panes.queue_event(Event::PointerButton {
        pos: pos2(1000.0, 200.0),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    let _ = panes.run_split_frame();
    panes.queue_event(Event::Text("reset".to_owned()));
    panes.queue_event(Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    });
    panes.queue_event(Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: Modifiers::NONE,
    });
    let _ = panes.run_split_frame();

    assert_eq!(panes.left.input().written_str(), "cargo check\r");
    assert_eq!(panes.right.input().written_str(), "reset\r");
}

#[test]
fn w5_cjk_emoji_mixed_width_reflow_and_drag_select_copy() {
    use egui::{Modifiers, PointerButton};
    use egui_xterm::{ScaleMode, TerminalConfig};
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

    h.set_viewport_size(100.0 * cw + 1.0, 24.0 * ch + 1.0);
    let r100 = h.run_frame().terminal_response;
    assert_eq!(r100.cols, 100);

    let mut logs = String::new();
    for i in 0..200usize {
        if i > 0 {
            logs.push_str("\r\n");
        }
        logs.push_str(&format!(
            "[INFO] 编译完成 🎉 café 👨‍👩‍👧‍👦 \x1b[58:2::0:200:255;4:2mwarn\x1b[0m #{i:03}"
        ));
    }
    h.runtime.input_mut().push_str(&logs);
    assert!(
        h.run_until(32, 1.0 / 60.0, |harness, _| harness
            .runtime
            .input()
            .is_drained()),
        "200 mixed-width lines did not drain"
    );
    let expected_full = h.runtime.terminal().full_text();

    for target_cols in [29u16, 64, 100] {
        h.set_viewport_size(f32::from(target_cols) * cw + 0.5, 24.0 * ch + 1.0);
        let resp = h.run_frame().terminal_response;
        assert_eq!(resp.cols, target_cols);
        assert_eq!(h.runtime.terminal().full_text(), expected_full);

        // Verify wide-cell + wide-spacer invariant across all visible rows.
        for row in 0..resp.rows {
            let mut col = 0u16;
            while col < resp.cols {
                let cell = h
                    .runtime
                    .terminal()
                    .cell_at(col, row)
                    .expect("visible cell");
                if cell.is_wide {
                    let spacer = h
                        .runtime
                        .terminal()
                        .cell_at(col + 1, row)
                        .expect("wide glyph must not split across row boundary");
                    assert!(spacer.is_wide_spacer);
                    col += 2;
                } else {
                    assert!(!cell.is_wide_spacer, "orphaned spacer at ({col}, {row})");
                    col += 1;
                }
            }
        }
    }

    // Clear and write a controlled wrapped CJK + ZWJ emoji line at 19 cols for drag-select + copy.
    // "PREFIX_" (7) + "编译完成" (8) + "_" (1) + "👨‍👩‍👧‍👦" (2) + "_" (1) = 19 cols on row 0; "SUFFIX" on row 1.
    h.set_viewport_size(19.0 * cw + 0.5, 24.0 * ch + 1.0);
    let r19 = h.run_frame().terminal_response;
    assert_eq!(r19.cols, 19);

    h.runtime
        .input_mut()
        .push_str("\x1b[2J\x1b[HPREFIX_编译完成_👨‍👩‍👧‍👦_SUFFIX");
    h.focus_terminal();
    let row0_text = h
        .runtime
        .terminal()
        .visible_line_text(0)
        .expect("wrapped row 0");
    assert!(
        row0_text.starts_with("PREFIX_编译完成_"),
        "row0={row0_text:?}"
    );
    let row1_text = h
        .runtime
        .terminal()
        .visible_line_text(1)
        .expect("wrapped row 1");
    assert!(row1_text.contains("SUFFIX"), "row1={row1_text:?}");

    assert_eq!(
        h.runtime.terminal().cell_at(5, 1).map(|c| c.ch),
        Some('X'),
        "expected 'X' of SUFFIX at (5, 1)"
    );
    // Col 7 on row 0 is '编' (after "PREFIX_"), col 5 on row 1 is 'X' (end of "SUFFIX").
    let from = h.cell_center(7, 0).expect("cell (7, 0)");
    let to = h.cell_center(5, 1).expect("cell (5, 1)");
    h.inject_drag_steps(from, to, PointerButton::Primary, Modifiers::NONE, 4);
    assert_eq!(
        h.runtime.terminal().selection_text().as_deref(),
        Some("编译完成_👨‍👩‍👧‍👦_SUFFIX")
    );
    assert!(
        h.last_outcome()
            .expect("drag settled")
            .settled_selections()
            .iter()
            .any(|s| s == "编译完成_👨‍👩‍👧‍👦_SUFFIX")
    );

    h.inject_copy();
    let copy_out = h.run_frame();
    assert_eq!(
        copy_out.copied_texts(),
        vec!["编译完成_👨‍👩‍👧‍👦_SUFFIX".to_owned()]
    );
}

#[test]
fn w6_ime_cjk_composition_session_repl() {
    use egui_xterm_e2e::HeadlessTermHarness;

    let mut h = HeadlessTermHarness::scripted();
    h.runtime.input_mut().push_str("> ");
    h.focus_terminal();
    h.runtime.input_mut().clear_written();
    assert_eq!(h.runtime.terminal().cursor_pos(), (2, 0));

    for (preedit, range) in [
        ("z", Some(0..1)),
        ("zh", Some(0..2)),
        ("zhong", Some(0..5)),
        ("中文", Some(0..2)),
    ] {
        h.inject_ime_preedit(preedit, range);
        let out = h.run_frame();
        let term_rect = out.terminal_response.response.rect;
        let cell_w = out.terminal_response.cell_size.x;
        let ime: egui::output::IMEOutput = out
            .platform_output
            .ime
            .expect("IMEOutput active during preedit");

        assert!(term_rect.expand(1.0).contains_rect(ime.rect));
        assert!(term_rect.expand(1.0).contains_rect(ime.cursor_rect));
        assert!(
            ime.cursor_rect.min.x >= term_rect.min.x + 1.5 * cell_w,
            "IME cursor_rect {:?} did not track after prompt '> '",
            ime.cursor_rect
        );
        assert!(out.inspect_shapes().contains_text(preedit));
        assert!(h.runtime.input().written_bytes().is_empty());
    }

    h.inject_ime_commit("中文输入");
    let committed = h.run_frame();
    assert_eq!(h.runtime.input().written_bytes(), "中文输入".as_bytes());
    assert!(!committed.inspect_shapes().contains_text("中文"));

    h.runtime.input_mut().push_str("中文输入");
    let echoed = h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0),
        Some("> 中文输入".to_owned())
    );
    assert!(
        echoed.inspect_shapes().text_mesh_vertices().len()
            > committed.inspect_shapes().text_mesh_vertices().len()
    );
}

#[test]
fn w7_burst_5mib_vt_log_flood_under_budget() {
    use egui_xterm::TerminalConfig;
    use egui_xterm_e2e::HeadlessTermHarness;

    let budget = 128 * 1024;
    let cfg = TerminalConfig {
        read_budget_bytes_per_frame: budget,
        scrollback_limit: 500,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);

    let line = b"\x1b[32m[LOG] \x1b[0mline_0123456789abcdef_0123456789abcdef\r\n";
    let target_bytes = 5 * 1024 * 1024;
    let mut payload = Vec::with_capacity(target_bytes + line.len());
    while payload.len() < target_bytes {
        payload.extend_from_slice(line);
    }
    let total_len = payload.len();
    h.runtime.input_mut().push_chunked(&payload, 64 * 1024);

    let mut frames = 0usize;
    while !h.runtime.input().is_drained() && frames < 80 {
        let _ = h.run_frame();
        assert!(h.runtime.terminal().scrollback_len() <= 500);
        let stats = h.render_to_mesh_stats();
        assert!(
            stats.total_vertices < 150_000,
            "frame {frames} vertices exceeded bound: {}",
            stats.total_vertices
        );
        frames += 1;
    }

    assert!(h.runtime.input().is_drained(), "5 MiB flood did not drain");
    assert!(
        frames >= 35,
        "expected incremental budget drain (~40 frames), got {frames}"
    );
    assert_eq!(h.runtime.terminal().scrollback_len(), 500);

    #[cfg(feature = "metrics")]
    {
        let metrics = h.runtime.terminal().metrics();
        assert!(
            metrics.bytes_processed >= total_len as u64,
            "bytes_processed={} < total_len={total_len}",
            metrics.bytes_processed
        );
        assert_eq!(metrics.frames_rendered, frames as u64);
        assert!(metrics.dirty_rows_rendered > 0);
    }
    let _ = total_len;
}

#[cfg(all(not(target_arch = "wasm32"), feature = "pty"))]
#[test]
fn w8_native_desktop_pty_interactive_shell_and_resize() {
    use std::time::Duration;

    use egui_xterm::TerminalInput;
    use egui_xterm_e2e::PtyTestSession;

    let mut sess = PtyTestSession::spawn_interactive_shell(80, 24).expect("spawn shell");
    sess.harness.set_viewport_size(800.0, 600.0);
    let _ = sess.harness.run_frame();

    sess.send_line("echo SHELL_READY").expect("send ready");
    assert!(
        sess.run_until_contains("SHELL_READY", Duration::from_secs(8))
            .expect("poll ready"),
        "shell did not print SHELL_READY"
    );

    // Settle initial 800x600 debounce (0.25s).
    sess.harness.advance_time(0.25);
    let init_resp = sess.harness.run_frame().terminal_response;

    #[cfg(windows)]
    {
        let expected_init =
            (u32::from(init_resp.cols) * 1000 + u32::from(init_resp.rows)).to_string();
        sess.send_line(
            "powershell -NoLogo -NoProfile -Command [Console]::WindowWidth*1000+[Console]::WindowHeight",
        )
        .expect("send initial Windows size probe");
        assert!(
            sess.run_until_contains(&expected_init, Duration::from_secs(10))
                .expect("poll initial size"),
            "initial Windows PTY size {expected_init} not observed"
        );
    }
    #[cfg(not(windows))]
    {
        let expected_init = format!("{} {}", init_resp.rows, init_resp.cols);
        sess.send_line("stty size").expect("send stty size");
        assert!(
            sess.run_until_contains(&expected_init, Duration::from_secs(8))
                .expect("poll initial stty size"),
            "initial Unix PTY size {expected_init} not observed"
        );
    }

    // Resize viewport to 480x320 over 20 frames (0.33s) so debounce triggers cleanly.
    sess.harness.set_viewport_size(480.0, 320.0);
    let resize_outcomes = sess.harness.run_frames(20, 1.0 / 60.0);
    let new_resp = &resize_outcomes
        .last()
        .expect("at least one resize frame")
        .terminal_response;
    assert!(new_resp.cols < init_resp.cols && new_resp.rows < init_resp.rows);

    #[cfg(windows)]
    {
        let expected_new = (u32::from(new_resp.cols) * 1000 + u32::from(new_resp.rows)).to_string();
        sess.send_line(
            "powershell -NoLogo -NoProfile -Command [Console]::WindowWidth*1000+[Console]::WindowHeight",
        )
        .expect("send resized Windows probe");
        assert!(
            sess.run_until_contains(&expected_new, Duration::from_secs(10))
                .expect("poll resized size"),
            "resized Windows PTY size {expected_new} not observed"
        );
    }
    #[cfg(not(windows))]
    {
        let expected_new = format!("{} {}", new_resp.rows, new_resp.cols);
        sess.send_line("stty size").expect("send resized stty size");
        assert!(
            sess.run_until_contains(&expected_new, Duration::from_secs(8))
                .expect("poll resized stty size"),
            "resized Unix PTY size {expected_new} not observed"
        );
    }

    sess.send_line("exit").expect("send exit");
    let closed = sess.harness.run_until(300, 1.0 / 60.0, |harness, _| {
        std::thread::sleep(Duration::from_millis(10));
        harness.runtime.input().is_closed()
    });
    assert!(closed, "PTY did not transition to is_closed() after exit");
}

#[test]
fn w9_wasm_safe_trait_driven_echo_console() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use egui::{Key, Modifiers};
    use egui_xterm::{TerminalConfig, TerminalEvent};
    use egui_xterm_core::Theme;
    use egui_xterm_e2e::{HeadlessTermHarness, LocalRcInput, ScriptedInput};

    let cfg = TerminalConfig {
        theme: Theme::light(),
        ..Default::default()
    };

    #[cfg(feature = "serde")]
    let cfg = {
        let json = serde_json::to_string(&cfg).expect("serialize TerminalConfig");
        let decoded: TerminalConfig =
            serde_json::from_str(&json).expect("deserialize TerminalConfig");
        let theme_json = serde_json::to_string(&decoded.theme).expect("serialize Theme");
        let decoded_theme: Theme = serde_json::from_str(&theme_json).expect("deserialize Theme");
        assert_eq!(decoded_theme, decoded.theme);
        decoded
    };

    let shared = Rc::new(RefCell::new(ScriptedInput::new()));
    let local = LocalRcInput::new(Rc::clone(&shared));
    let mut h = HeadlessTermHarness::with_config(local, cfg);

    shared.borrow_mut().push_str("$ \x1b]0;WebConsole\x07");
    let f1 = h.run_frame();
    assert_eq!(h.runtime.terminal().title(), "WebConsole");
    assert!(
        f1.terminal_response
            .events
            .contains(&TerminalEvent::TitleChanged("WebConsole".to_owned()))
    );

    // DSR cursor position report (\x1b[6n) -> CPR at row 1, col 3 (\x1b[1;3R).
    shared.borrow_mut().push_str("\x1b[6n");
    let _ = h.run_frame();
    assert_eq!(shared.borrow_mut().take_written(), b"\x1b[1;3R");

    h.focus_terminal();
    shared.borrow_mut().clear_written();
    h.inject_text("help");
    h.tap_key(Key::Enter, Modifiers::NONE);
    let _ = h.run_frame();
    assert_eq!(shared.borrow_mut().take_written(), b"help\r");

    shared.borrow_mut().push_str("help\r\nCOMMANDS: clear\r\n");
    let echoed = h.run_frame().inspect_shapes();
    assert_eq!(
        h.runtime.terminal().visible_line_text(1).as_deref(),
        Some("COMMANDS: clear")
    );
    assert!(!echoed.text_mesh_vertices().is_empty());

    shared.borrow_mut().push_str("\x1b[2J\x1b[H$ ");
    let cleared = h.run_frame().inspect_shapes();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0),
        Some("$".to_owned())
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(1),
        Some(String::new())
    );
    assert!(cleared.text_mesh_vertices().len() < echoed.text_mesh_vertices().len());
}

#[test]
fn w10_mouse_enabled_pager_shift_bypass_selection() {
    use egui::{Modifiers, MouseWheelUnit, PointerButton, Vec2};
    use egui_xterm_e2e::HeadlessTermHarness;

    let mut h = HeadlessTermHarness::scripted();
    h.runtime.input_mut().push_str(
        "\x1b[?1049h\x1b[1;20r\x1b[?1003h\x1b[?1006h\x1b[?1007h\
         PAGER_LINE_0\r\nPAGER_LINE_1\r\nPAGER_LINE_2",
    );
    h.focus_terminal();
    h.runtime.input_mut().clear_written();

    let hover_pos = h.cell_center(4, 1).expect("cell (4, 1)");
    h.inject_pointer_move(hover_pos);
    let _ = h.run_frame();
    h.runtime.input_mut().clear_written();

    // Step 2a: ?1003h active -> SGR wheel takes precedence over 1007.
    h.inject_scroll(MouseWheelUnit::Line, Vec2::new(0.0, 1.0), Modifiers::NONE);
    let _ = h.run_frame();
    h.inject_scroll(MouseWheelUnit::Line, Vec2::new(0.0, -1.0), Modifiers::NONE);
    let _ = h.run_frame();
    let wheel_sgr = h.runtime.input().written_str();
    assert!(
        wheel_sgr.contains("\x1b[<64;5;2M") && wheel_sgr.contains("\x1b[<65;5;2M"),
        "expected SGR wheel precedence: {wheel_sgr:?}"
    );
    assert!(!wheel_sgr.contains("\x1b[A") && !wheel_sgr.contains("\x1b[B"));
    h.runtime.input_mut().clear_written();

    // Step 2b: disable ?1003l -> 1007 alternate scroll emits Up/Down arrows.
    h.runtime.input_mut().push_str("\x1b[?1003l");
    let _ = h.run_frame();
    h.inject_scroll(MouseWheelUnit::Line, Vec2::new(0.0, 1.0), Modifiers::NONE);
    let _ = h.run_frame();
    h.inject_scroll(MouseWheelUnit::Line, Vec2::new(0.0, -1.0), Modifiers::NONE);
    let _ = h.run_frame();
    let alt_scroll = h.runtime.input().written_str();
    assert!(
        alt_scroll.contains("\x1b[A") && alt_scroll.contains("\x1b[B"),
        "expected 1007 alt scroll arrows: {alt_scroll:?}"
    );

    // Step 3: re-enable ?1003h -> pointer move & click report SGR 35 and 0.
    h.runtime.input_mut().push_str("\x1b[?1003h");
    let _ = h.run_frame();
    h.runtime.input_mut().clear_written();
    let p_click = h.cell_center(3, 0).expect("cell (3, 0)");
    h.inject_pointer_move(p_click);
    let _ = h.run_frame();
    h.inject_click(p_click, PointerButton::Primary, Modifiers::NONE);
    let mouse_ops = h.runtime.input().written_str();
    assert!(
        mouse_ops.contains("\x1b[<35;4;1M"),
        "missing 1003 any-event motion: {mouse_ops:?}"
    );
    assert!(
        mouse_ops.contains("\x1b[<0;4;1M") && mouse_ops.contains("\x1b[<0;4;1m"),
        "missing SGR click: {mouse_ops:?}"
    );
    h.runtime.input_mut().clear_written();

    // Step 4: Shift+drag bypasses mouse reporting to create local selection + CopyText.
    let from = h.cell_center(0, 0).expect("cell (0, 0)");
    let to = h.cell_center(11, 0).expect("cell (11, 0)");
    h.inject_drag_steps(from, to, PointerButton::Primary, Modifiers::SHIFT, 3);
    assert!(h.runtime.input().written_bytes().is_empty());
    assert_eq!(
        h.runtime.terminal().selection_text().as_deref(),
        Some("PAGER_LINE_0")
    );
    assert!(
        h.last_outcome()
            .expect("Shift+drag settled")
            .settled_selections()
            .iter()
            .any(|s| s == "PAGER_LINE_0")
    );

    h.inject_copy();
    let copy_out = h.run_frame();
    assert_eq!(copy_out.copied_texts(), vec!["PAGER_LINE_0".to_owned()]);
}
