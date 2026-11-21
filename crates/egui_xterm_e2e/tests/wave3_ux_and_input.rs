//! Wave 3 opaque-box E2E tests for egui Widget, UX & Input features (Item #11 / #7):
//! 8 Tier 1, 8 Tier 2, 4 Tier 3, and 1 Tier 4 workload (`w_ux_01_interactive_ide_terminal_session_ux_and_pty`).

#![forbid(unsafe_code)]

use egui::{
    Color32, CursorIcon, Event, FontDefinitions, FontFamily, Key, Modifiers, PointerButton, Rect,
    pos2,
};
use egui_xterm::{
    ClipboardSelection, Osc52ReadPolicy, ProgressIndicator, ScaleMode, TerminalConfig,
    TerminalEvent, TerminalIoError,
};
use egui_xterm_e2e::{HeadlessTermHarness, MultiPaneHarness, ScriptedInput, ShapeInspection};

fn rgb_to_c32(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

fn progress_bar_fills(insp: &ShapeInspection, term_rect: Rect) -> Vec<(Rect, Color32)> {
    const NORMAL_C: Color32 = Color32::from_rgb(66, 165, 245);
    const ERROR_C: Color32 = Color32::from_rgb(239, 83, 80);
    const PAUSED_C: Color32 = Color32::from_rgb(255, 183, 77);
    insp.rect_fills
        .iter()
        .copied()
        .filter(|&(r, c)| {
            let is_track = c.a() == 28 && r.width() >= (term_rect.width() - 1.5).max(1.0);
            let is_fill = c == NORMAL_C || c == ERROR_C || c == PAUSED_C;
            (is_track || is_fill)
                && r.min.y >= term_rect.min.y - 0.5
                && r.max.y <= term_rect.min.y + 6.0
                && r.height() <= 5.0
        })
        .collect()
}

fn covers_term_rect(rect: Rect, term_rect: Rect) -> bool {
    (rect.width() - term_rect.width()).abs() <= 2.0
        && (rect.height() - term_rect.height()).abs() <= 2.0
}

fn visual_bell_flash_colors(insp: &ShapeInspection, term_rect: Rect) -> Vec<Color32> {
    insp.rect_fills
        .iter()
        .filter_map(|(r, c)| {
            (covers_term_rect(*r, term_rect) && c.a() > 0 && c.a() < 255).then_some(*c)
        })
        .collect()
}

fn visual_bell_flash_alphas(insp: &ShapeInspection, term_rect: Rect) -> Vec<u8> {
    visual_bell_flash_colors(insp, term_rect)
        .into_iter()
        .map(|c| c.a())
        .collect()
}

fn assert_bell_tint_and_alpha(
    actual: Color32,
    expected_rgb: Color32,
    min_alpha: u8,
    max_alpha: u8,
) {
    let alpha = actual.a();
    assert!(
        (min_alpha..=max_alpha).contains(&alpha),
        "expected visual bell alpha in {min_alpha}..={max_alpha}, got {alpha}"
    );
    let expected = Color32::from_rgba_unmultiplied(
        expected_rgb.r(),
        expected_rgb.g(),
        expected_rgb.b(),
        alpha,
    );
    assert_eq!(
        actual,
        expected,
        "expected visual bell RGB ({}, {}, {}) at alpha {alpha}, got {actual:?}",
        expected_rgb.r(),
        expected_rgb.g(),
        expected_rgb.b()
    );
}

// ===========================================================================
// Item #11 — Tier 1: Equivalence-Class Happy-Path Tests (8 tests)
// ===========================================================================

mod item11_tier1 {
    use super::*;

    #[test]
    fn t1_ux_01_bold_and_faux_bold_weight_rendering() {
        let cfg = TerminalConfig {
            cursor_blink: false,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.runtime.input_mut().push_str("AAAA\r\n\x1b[1mAAAA\x1b[0m");
        let out = h.run_frame();
        let origin = out.terminal_response.response.rect.min;
        let term_w = out.terminal_response.response.rect.width();
        let cell_h = out.terminal_response.cell_size.y;
        let insp = out.inspect_shapes();

        let row0_rect = Rect::from_min_max(
            pos2(origin.x, origin.y + 0.1),
            pos2(origin.x + term_w, origin.y + cell_h - 0.1),
        );
        let row1_rect = Rect::from_min_max(
            pos2(origin.x, origin.y + cell_h + 0.1),
            pos2(origin.x + term_w, origin.y + 2.0 * cell_h - 0.1),
        );
        let normal_verts = insp.text_mesh_vertices_in_rect(row0_rect);
        let faux_bold_verts = insp.text_mesh_vertices_in_rect(row1_rect);
        assert_eq!(
            normal_verts.len(),
            16,
            "4 normal 'A' chars must emit 4 quads (16 verts)"
        );
        assert_eq!(
            faux_bold_verts.len(),
            32,
            "4 faux-bold 'A' chars must double-stamp 8 quads (32 verts)"
        );

        // Register a dedicated `monospace_bold` family -> renderer switches to single-pass bold font.
        let mut defs = FontDefinitions::default();
        if let Some(mono) = defs.families.get(&FontFamily::Monospace).cloned() {
            defs.families
                .insert(FontFamily::Name("monospace_bold".into()), mono);
        }
        h.ctx.set_fonts(defs);
        let out_fam = h.run_frame();
        let insp_fam = out_fam.inspect_shapes();
        let fam_bold_verts = insp_fam.text_mesh_vertices_in_rect(row1_rect);
        assert_eq!(
            fam_bold_verts.len(),
            16,
            "monospace_bold FontFamily must emit 1 quad per bold glyph (16 verts)"
        );

        // Bold-is-bright behavior for indexed ANSI fg (SGR 1;31).
        h.runtime
            .input_mut()
            .push_str("\x1b[2J\x1b[H\x1b[1;31mR\x1b[0m");
        h.config.theme.bold_is_bright = true;
        let bright_insp = h.run_frame().inspect_shapes();
        assert!(bright_insp.has_text_fg_color(rgb_to_c32(h.config.theme.scheme.ansi[9])));

        h.config.theme.bold_is_bright = false;
        let base_insp = h.run_frame().inspect_shapes();
        assert!(base_insp.has_text_fg_color(rgb_to_c32(h.config.theme.scheme.ansi[1])));
    }

    #[test]
    fn t1_ux_02_visual_bell_config_and_decay() {
        let cfg = TerminalConfig {
            cursor_blink: false,
            idle_poll_interval_ms: 0,
            visual_bell_duration_ms: 200,
            visual_bell_color: None,
            visual_bell_opacity: 0.18,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        let default_fg = rgb_to_c32(h.config.theme.scheme.foreground);
        let _ = h.run_frame();

        h.runtime.input_mut().push_str("\x07");
        let peak = h.run_frame();
        assert!(peak.terminal_response.events.contains(&TerminalEvent::Bell));
        assert!(peak.repaint_requested);
        let term_rect = peak.terminal_response.response.rect;
        let peak_colors = visual_bell_flash_colors(&peak.inspect_shapes(), term_rect);
        assert_eq!(peak_colors.len(), 1);
        assert_bell_tint_and_alpha(peak_colors[0], default_fg, 40, 52);

        // At t=0.10 (halfway through 200ms duration), flash decays strictly monotonically.
        h.advance_time(0.10);
        let mid = h.run_frame();
        let mid_colors = visual_bell_flash_colors(&mid.inspect_shapes(), term_rect);
        assert_eq!(mid_colors.len(), 1);
        assert_bell_tint_and_alpha(mid_colors[0], default_fg, 1, peak_colors[0].a() - 1);

        // At t=0.25 (past 200ms), flash disappears completely.
        h.advance_time(0.15);
        let done = h.run_frame();
        assert!(visual_bell_flash_colors(&done.inspect_shapes(), term_rect).is_empty());

        // Custom visual_bell_color & visual_bell_opacity render with the configured RGB tint & alpha.
        let custom_rgb = Color32::from_rgb(255, 60, 40);
        assert_ne!(custom_rgb, default_fg);
        h.config.visual_bell_color = Some(custom_rgb);
        h.config.visual_bell_opacity = 0.45;
        h.runtime.input_mut().push_str("\x07");
        let custom_peak = h.run_frame();
        assert!(
            custom_peak
                .terminal_response
                .events
                .contains(&TerminalEvent::Bell)
        );
        assert!(custom_peak.repaint_requested);
        let custom_peak_colors = visual_bell_flash_colors(&custom_peak.inspect_shapes(), term_rect);
        assert_eq!(custom_peak_colors.len(), 1);
        assert_bell_tint_and_alpha(custom_peak_colors[0], custom_rgb, 110, 120);

        h.advance_time(0.10);
        let custom_mid = h.run_frame();
        let custom_mid_colors = visual_bell_flash_colors(&custom_mid.inspect_shapes(), term_rect);
        assert_eq!(custom_mid_colors.len(), 1);
        assert_bell_tint_and_alpha(
            custom_mid_colors[0],
            custom_rgb,
            1,
            custom_peak_colors[0].a() - 1,
        );

        // Updating visual_bell_color & visual_bell_opacity dynamically applies the new tint/alpha.
        let live_rgb = Color32::from_rgb(40, 210, 120);
        assert_ne!(live_rgb, custom_rgb);
        assert_ne!(live_rgb, default_fg);
        h.config.visual_bell_color = Some(live_rgb);
        h.config.visual_bell_opacity = 0.70;
        h.runtime.input_mut().push_str("\x07");
        let live_peak = h.run_frame();
        let live_colors = visual_bell_flash_colors(&live_peak.inspect_shapes(), term_rect);
        assert_eq!(live_colors.len(), 1);
        assert_bell_tint_and_alpha(live_colors[0], live_rgb, 174, 184);

        h.advance_time(0.25);
        let custom_done = h.run_frame();
        assert!(visual_bell_flash_colors(&custom_done.inspect_shapes(), term_rect).is_empty());

        #[cfg(feature = "serde")]
        {
            let json = serde_json::to_string(&h.config).expect("serialize TerminalConfig");
            let roundtrip: TerminalConfig =
                serde_json::from_str(&json).expect("deserialize TerminalConfig");
            assert_eq!(roundtrip.visual_bell_color, Some(live_rgb));
            assert!((roundtrip.visual_bell_opacity - 0.70).abs() < 1e-6);
        }
    }

    #[test]
    fn t1_ux_03_osc_9_4_progress_bar_ui_all_states() {
        let cfg = TerminalConfig {
            cursor_blink: false,
            idle_poll_interval_ms: 0,
            show_progress_bar: true,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        let warm = h.run_frame();
        let term_rect = warm.terminal_response.response.rect;
        assert!(progress_bar_fills(&warm.inspect_shapes(), term_rect).is_empty());

        let normal_c = Color32::from_rgb(66, 165, 245);
        let error_c = Color32::from_rgb(239, 83, 80);
        let paused_c = Color32::from_rgb(255, 183, 77);

        // State 1: Normal 50%.
        h.runtime.input_mut().push_str("\x1b]9;4;1;50\x07");
        let f1 = h.run_frame();
        assert_eq!(
            h.runtime.progress(),
            Some(ProgressIndicator {
                state: 1,
                progress: 50
            })
        );
        let bars1 = progress_bar_fills(&f1.inspect_shapes(), term_rect);
        assert_eq!(bars1.len(), 2);
        assert_eq!(bars1[1].1, normal_c);
        assert!((bars1[1].0.width() - term_rect.width() * 0.5).abs() <= 1.5);
        assert_eq!(f1.inspect_shapes().non_snapped_rect_count, 0);

        // State 2: Error (bare 0 => full width; explicit 40 => 40%).
        h.runtime.input_mut().push_str("\x1b]9;4;2\x07");
        let f2 = h.run_frame();
        let bars2 = progress_bar_fills(&f2.inspect_shapes(), term_rect);
        assert_eq!(bars2.len(), 2);
        assert_eq!(bars2[1].1, error_c);
        assert!((bars2[1].0.width() - term_rect.width()).abs() <= 1.5);

        // State 3: Indeterminate (animates sliding bar and requests repaint).
        h.runtime.input_mut().push_str("\x1b]9;4;3\x07");
        let f3a = h.run_frame();
        assert!(f3a.repaint_requested);
        let bars3a = progress_bar_fills(&f3a.inspect_shapes(), term_rect);
        assert_eq!(bars3a[1].1, normal_c);
        h.advance_time(0.25);
        let f3b = h.run_frame();
        let bars3b = progress_bar_fills(&f3b.inspect_shapes(), term_rect);
        assert_ne!(bars3a[1].0.min.x, bars3b[1].0.min.x);

        // State 4: Paused 30%.
        h.runtime.input_mut().push_str("\x1b]9;4;4;30\x07");
        let f4 = h.run_frame();
        let bars4 = progress_bar_fills(&f4.inspect_shapes(), term_rect);
        assert_eq!(bars4[1].1, paused_c);
        assert!((bars4[1].0.width() - term_rect.width() * 0.3).abs() <= 1.5);

        // Toggling show_progress_bar=false hides UI while preserving runtime.progress().
        h.config.show_progress_bar = false;
        let hidden = h.run_frame();
        assert!(h.runtime.progress().is_some());
        assert!(progress_bar_fills(&hidden.inspect_shapes(), term_rect).is_empty());

        // State 0 clears indicator completely.
        h.config.show_progress_bar = true;
        h.runtime.input_mut().push_str("\x1b]9;4;0\x07");
        let f0 = h.run_frame();
        assert_eq!(h.runtime.progress(), None);
        assert!(progress_bar_fills(&f0.inspect_shapes(), term_rect).is_empty());
    }

    #[test]
    fn t1_ux_04_osc_22_pointer_cursor_vs_osc_8_hyperlink_precedence() {
        let mut h = HeadlessTermHarness::scripted();
        h.focus_terminal();
        h.runtime.input_mut().push_str(
            "\x1b]22;crosshair\x07\x1b]8;;https://example.org\x07LINK\x1b]8;;\x07  PLAIN",
        );
        let _ = h.run_frame();

        let plain_pos = h.cell_center(7, 0).expect("plain cell");
        let link_pos = h.cell_center(1, 0).expect("link cell");

        // Plain cell uses OSC 22 cursor (`Crosshair`).
        h.inject_pointer_move(plain_pos);
        let plain_frame = h.run_frame();
        assert_eq!(
            plain_frame.platform_output.cursor_icon,
            CursorIcon::Crosshair
        );

        #[cfg(feature = "hyperlinks")]
        {
            // Hyperlink hover overrides OSC 22 with `PointingHand`.
            h.inject_pointer_move(link_pos);
            let link_frame = h.run_frame();
            assert_eq!(
                link_frame.platform_output.cursor_icon,
                CursorIcon::PointingHand
            );

            // With `hyperlink_require_ctrl=true` and Ctrl NOT held, OSC 22 Crosshair stays active.
            h.config.hyperlink_require_ctrl = true;
            let no_ctrl = h.run_frame();
            assert_eq!(no_ctrl.platform_output.cursor_icon, CursorIcon::Crosshair);

            // With Ctrl held over link, PointingHand overrides OSC 22 again.
            h.inject_pointer_button(link_pos, PointerButton::Secondary, false, Modifiers::CTRL);
            let with_ctrl = h.run_frame();
            assert_eq!(
                with_ctrl.platform_output.cursor_icon,
                CursorIcon::PointingHand
            );
            h.config.hyperlink_require_ctrl = false;
        }
        #[cfg(not(feature = "hyperlinks"))]
        let _ = link_pos;

        // Moving back to plain cell keeps OSC 22 Crosshair; OSC 22 default restores Default.
        h.inject_pointer_move(plain_pos);
        let back_plain = h.run_frame();
        assert_eq!(
            back_plain.platform_output.cursor_icon,
            CursorIcon::Crosshair
        );

        h.runtime.input_mut().push_str("\x1b]22;default\x07");
        let reset_frame = h.run_frame();
        assert_eq!(reset_frame.platform_output.cursor_icon, CursorIcon::Default);
    }

    #[test]
    fn t1_ux_05_modify_other_keys_numpad_clear_and_lock_modifiers() {
        let mut h = HeadlessTermHarness::scripted();
        h.focus_terminal();
        let _ = h.run_frame(); // settle focus lock filter
        h.runtime.input_mut().clear_written();

        // Numpad `Key::Clear` (KpBegin) in legacy mode -> `\x1b[E` and `\x1b[1;5E` with Ctrl.
        h.tap_key(Key::Clear, Modifiers::NONE);
        h.tap_key(Key::Clear, Modifiers::CTRL);
        let _ = h.run_frame();
        assert_eq!(h.runtime.input().written_str(), "\x1b[E\x1b[1;5E");
        h.runtime.input_mut().clear_written();

        // `modifyOtherKeys` level 1 & level 2.
        h.runtime.input_mut().push_str("\x1b[>4;2m");
        let _ = h.run_frame();
        assert_eq!(h.runtime.terminal().modify_other_keys_level(), 2);

        h.tap_key(Key::Enter, Modifiers::SHIFT);
        h.tap_key(Key::Enter, Modifiers::CTRL);
        h.tap_key(Key::A, Modifiers::ALT);
        let _ = h.run_frame();
        assert_eq!(
            h.runtime.input().written_str(),
            "\x1b[27;2;13~\x1b[27;5;13~\x1b[27;3;97~"
        );
        h.runtime.input_mut().clear_written();

        // Lock modifier API + key toggles.
        assert!(!h.runtime.caps_lock() && !h.runtime.num_lock());
        h.runtime.set_lock_modifiers(true, true);
        assert!(h.runtime.caps_lock() && h.runtime.num_lock());

        #[cfg(feature = "kitty-keyboard")]
        {
            // Under Kitty Flag 1 (disambiguate), lock bits (64 | 128) encode with active modifiers
            // (`Shift+Enter -> ;194u`); under Flag 8 (`REPORT_ALL`), locks emit alone (`;193u`).
            h.runtime.input_mut().push_str("\x1b[>1u");
            let _ = h.run_frame();
            h.tap_key(Key::Clear, Modifiers::NONE);
            h.tap_key(Key::Enter, Modifiers::SHIFT);
            let _ = h.run_frame();
            assert_eq!(h.runtime.input().written_str(), "\x1b[57427u\x1b[13;194u");
            h.runtime.input_mut().clear_written();

            h.runtime.input_mut().push_str("\x1b[=9u");
            let _ = h.run_frame();
            h.tap_key(Key::Clear, Modifiers::NONE);
            let _ = h.run_frame();
            assert_eq!(h.runtime.input().written_str(), "\x1b[57427;193u");
            h.runtime.input_mut().push_str("\x1b[<u");
            let _ = h.run_frame();
            h.runtime.input_mut().clear_written();

            // Toggling CapsLock / NumLock keys clears lock bits cleanly.
            h.tap_key(Key::CapsLock, Modifiers::NONE);
            h.tap_key(Key::NumLock, Modifiers::NONE);
            let _ = h.run_frame();
            assert!(!h.runtime.caps_lock() && !h.runtime.num_lock());
        }
    }

    #[test]
    fn t1_ux_06_revision_texture_invalidation_and_dynamic_cell_pixel_size() {
        let cfg = TerminalConfig {
            font_size: 12.0,
            resize_debounce_secs: 0.0,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.set_pixels_per_point(1.0);
        let f1 = h.run_frame();
        let px1 = h.runtime.terminal().cell_pixel_size();
        assert_eq!(
            px1,
            (
                (f1.terminal_response.cell_size.x * 1.0).round().max(1.0) as u16,
                (f1.terminal_response.cell_size.y * 1.0).round().max(1.0) as u16,
            )
        );

        // Increase font_size and DPI -> dynamic `cell_pixel_size` updates immediately.
        h.config.font_size = 22.0;
        h.set_pixels_per_point(2.0);
        let f2 = h.run_frame();
        let px2 = h.runtime.terminal().cell_pixel_size();
        assert!(px2.0 > px1.0 && px2.1 > px1.1);
        assert_eq!(
            px2,
            (
                (f2.terminal_response.cell_size.x * 2.0).round().max(1.0) as u16,
                (f2.terminal_response.cell_size.y * 2.0).round().max(1.0) as u16,
            )
        );

        #[cfg(feature = "kitty-graphics")]
        {
            // Upload Kitty image i=5 (2x2 solid red) -> initial texture upload.
            let red_b64 = encode_base64(&[255, 0, 0, 255].repeat(4));
            h.runtime.input_mut().push_str(format!(
                "\x1b_Ga=T,i=5,p=1,c=2,r=2,f=32,s=2,v=2,C=1;{red_b64}\x1b\\"
            ));
            let up1 = h.run_frame();
            let rev1 = h.runtime.terminal().image_data(5).expect("img 5").revision;
            let tex_id_1 = up1
                .inspect_shapes()
                .mesh_texture_ids
                .into_iter()
                .find(|id| *id != epaint::TextureId::default())
                .expect("kitty texture id");
            let entry1 = up1
                .texture_deltas
                .set_entries
                .iter()
                .find(|e| e.texture_id == tex_id_1)
                .copied()
                .expect("initial texture delta");

            // Idle frame emits zero texture deltas.
            let idle = h.run_frame();
            assert!(idle.texture_deltas.is_empty());

            // Replace i=5 with same dimensions (green pixels) -> revision increments & same TextureId updates.
            let green_b64 = encode_base64(&[0, 255, 0, 255].repeat(4));
            h.runtime.input_mut().push_str(format!(
                "\x1b_Ga=T,i=5,p=1,c=2,r=2,f=32,s=2,v=2,C=1;{green_b64}\x1b\\"
            ));
            let up2 = h.run_frame();
            let rev2 = h
                .runtime
                .terminal()
                .image_data(5)
                .expect("img 5 rev2")
                .revision;
            assert!(rev2 > rev1);
            let entry2 = up2
                .texture_deltas
                .set_entries
                .iter()
                .find(|e| e.texture_id == tex_id_1)
                .copied()
                .expect("updated texture delta on same TextureId");
            assert_ne!(entry1.pixel_hash, entry2.pixel_hash);
        }
    }

    #[test]
    fn t1_ux_07_terminal_input_io_error_surfacing_and_recovery() {
        let cfg = TerminalConfig {
            resize_debounce_secs: 0.0,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.focus_terminal();
        assert!(h.runtime.last_io_error().is_none());

        // 1. poll_read error
        h.runtime
            .input_mut()
            .fail_next_read(TerminalIoError::message("EIO_READ"));
        let r_err = h.run_frame();
        assert!(
            r_err
                .terminal_response
                .events
                .contains(&TerminalEvent::IoError("EIO_READ".to_owned()))
        );
        assert_eq!(
            h.runtime.last_io_error(),
            Some(&TerminalIoError::Message("EIO_READ".to_owned()))
        );
        assert_eq!(
            h.runtime.take_io_error(),
            Some(TerminalIoError::Message("EIO_READ".to_owned()))
        );
        assert!(h.runtime.last_io_error().is_none());

        // 2. write_bytes error
        h.runtime
            .input_mut()
            .fail_next_write(TerminalIoError::message("EPIPE_WRITE"));
        h.inject_text("cmd");
        let w_err = h.run_frame();
        assert!(
            w_err
                .terminal_response
                .events
                .contains(&TerminalEvent::IoError("EPIPE_WRITE".to_owned()))
        );
        assert_eq!(
            h.runtime.take_io_error(),
            Some(TerminalIoError::Message("EPIPE_WRITE".to_owned()))
        );

        // 3. resize error
        h.runtime
            .input_mut()
            .fail_next_resize(TerminalIoError::message("ENOTTY_RESIZE"));
        h.set_viewport_size(400.0, 300.0);
        let sz_err = h.run_frame();
        assert!(
            sz_err
                .terminal_response
                .events
                .contains(&TerminalEvent::IoError("ENOTTY_RESIZE".to_owned()))
        );
        assert_eq!(
            h.runtime.take_io_error(),
            Some(TerminalIoError::Message("ENOTTY_RESIZE".to_owned()))
        );

        // 4. Subsequent I/O succeeds normally after transient errors.
        h.runtime.input_mut().push_str("RECOVERED");
        h.inject_text("ok");
        let ok = h.run_frame();
        assert!(
            ok.terminal_response
                .events
                .iter()
                .all(|e| !matches!(e, TerminalEvent::IoError(_)))
        );
        assert_eq!(
            h.runtime.terminal().visible_line_text(0).as_deref(),
            Some("RECOVERED")
        );
        assert_eq!(h.runtime.input().written_str(), "ok");
    }

    #[test]
    fn t1_ux_08_osc52_policy_flow_and_osc6_osc99_runtime() {
        let cfg = TerminalConfig {
            osc52_write_enabled: true,
            osc52_read_policy: Osc52ReadPolicy::Deny,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);

        // OSC 52 write enabled copies to platform clipboard; disabled suppresses copy.
        h.runtime.input_mut().push_str("\x1b]52;c;aGVsbG8=\x07");
        let w_on = h.run_frame();
        assert_eq!(w_on.copied_texts(), vec!["hello".to_owned()]);

        h.config.osc52_write_enabled = false;
        h.runtime.input_mut().push_str("\x1b]52;c;d29ybGQ=\x07");
        let w_off = h.run_frame();
        assert!(w_off.copied_texts().is_empty());
        assert!(
            w_off
                .terminal_response
                .events
                .contains(&TerminalEvent::ClipboardWrite("world".to_owned()))
        );

        // OSC 52 read under Deny ignores `?`.
        h.runtime.input_mut().push_str("\x1b]52;c;?\x07");
        let deny_f = h.run_frame();
        assert_eq!(h.runtime.pending_osc52_read(), None);
        assert!(h.runtime.input().written_bytes().is_empty());
        assert!(!deny_f.terminal_response.events.iter().any(|e| matches!(
            e,
            TerminalEvent::ClipboardReadRequested { .. }
                | TerminalEvent::ClipboardReadPrompt { .. }
        )));

        // Allow policy emits ClipboardReadRequested immediately.
        h.config.osc52_read_policy = Osc52ReadPolicy::Allow;
        let _ = h.run_frame();
        h.runtime.input_mut().push_str("\x1b]52;c;?\x07");
        let allow_f = h.run_frame();
        assert!(allow_f.terminal_response.events.contains(
            &TerminalEvent::ClipboardReadRequested {
                selection: ClipboardSelection::Clipboard
            }
        ));

        // Prompt policy surfaces pending_osc52_read -> deny -> re-request -> approve_osc52_read.
        h.config.osc52_read_policy = Osc52ReadPolicy::Prompt;
        let _ = h.run_frame();
        h.runtime.input_mut().push_str("\x1b]52;c;?\x07");
        let prompt_f = h.run_frame();
        assert!(
            prompt_f
                .terminal_response
                .events
                .contains(&TerminalEvent::ClipboardReadPrompt {
                    selection: ClipboardSelection::Clipboard
                })
        );
        assert_eq!(
            h.runtime.pending_osc52_read(),
            Some(ClipboardSelection::Clipboard)
        );
        assert!(h.runtime.deny_osc52_read());
        assert_eq!(h.runtime.pending_osc52_read(), None);
        assert!(h.runtime.input().written_bytes().is_empty());

        h.runtime.input_mut().push_str("\x1b]52;p;?\x07");
        let _ = h.run_frame();
        assert_eq!(
            h.runtime.pending_osc52_read(),
            Some(ClipboardSelection::Primary)
        );
        assert!(h.runtime.approve_osc52_read("secret_clip"));
        assert_eq!(h.runtime.pending_osc52_read(), None);
        assert_eq!(
            h.runtime.input().written_str(),
            "\x1b]52;p;c2VjcmV0X2NsaXA=\x1b\\"
        );

        // OSC 6 document_uri() and OSC 99 notification events.
        h.runtime.input_mut().push_str(
            "\x1b]6;file:///workspace/src/main.rs\x07\x1b]99;i=n1:p=title:d=0;Build\x1b\\\x1b]99;i=n1:p=body:d=1;Green\x1b\\",
        );
        let meta_f = h.run_frame();
        assert_eq!(
            h.runtime.document_uri(),
            Some("file:///workspace/src/main.rs")
        );
        assert!(
            meta_f
                .terminal_response
                .events
                .contains(&TerminalEvent::DocumentChanged(Some(
                    "file:///workspace/src/main.rs".to_owned()
                )))
        );
        assert!(
            meta_f
                .terminal_response
                .events
                .contains(&TerminalEvent::Notification {
                    title: "Build".to_owned(),
                    body: "Green".to_owned(),
                })
        );
    }
}

// ===========================================================================
// Item #11 — Tier 2: Boundary Value & Edge-Case Tests (8 tests)
// ===========================================================================

mod item11_tier2 {
    use super::*;

    #[test]
    fn t2_ux_01_visual_bell_zero_duration_and_nan_opacity_boundaries() {
        let cfg = TerminalConfig {
            cursor_blink: false,
            idle_poll_interval_ms: 0,
            visual_bell_duration_ms: 0,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        let _ = h.run_frame();

        h.runtime.input_mut().push_str("\x07");
        let disabled = h.run_frame();
        assert!(
            disabled
                .terminal_response
                .events
                .contains(&TerminalEvent::Bell)
        );
        assert!(
            visual_bell_flash_alphas(
                &disabled.inspect_shapes(),
                disabled.terminal_response.response.rect
            )
            .is_empty()
        );

        // Setting duration=0 mid-flash immediately cancels active flash.
        h.config.visual_bell_duration_ms = 500;
        h.runtime.input_mut().push_str("\x07");
        let active = h.run_frame();
        let term_rect = active.terminal_response.response.rect;
        assert_eq!(
            visual_bell_flash_alphas(&active.inspect_shapes(), term_rect).len(),
            1
        );

        h.config.visual_bell_duration_ms = 0;
        h.advance_time(0.05);
        let cancelled = h.run_frame();
        assert!(visual_bell_flash_alphas(&cancelled.inspect_shapes(), term_rect).is_empty());

        // Extreme / NaN opacity values with custom color clamp cleanly and never cause panic.
        let boundary_rgb = Color32::from_rgb(30, 180, 240);
        h.config.visual_bell_color = Some(boundary_rgb);
        for (opacity, expected_alpha_range) in [
            (-1.0_f32, None),
            (0.0, None),
            (2.5, Some((255_u8, 255_u8))),
            (f32::NAN, Some((40_u8, 52_u8))),
        ] {
            h.config.visual_bell_duration_ms = 100;
            h.config.visual_bell_opacity = opacity;
            h.runtime.input_mut().push_str("\x07");
            let f = h.run_frame();
            assert!(f.terminal_response.cols >= 1);
            let insp = f.inspect_shapes();
            let flashes: Vec<Color32> = insp
                .rect_fills
                .iter()
                .filter_map(|(r, c)| (covers_term_rect(*r, term_rect) && c.a() > 0).then_some(*c))
                .collect();
            match expected_alpha_range {
                None => assert!(
                    flashes.is_empty(),
                    "opacity {opacity} should suppress visual bell flash"
                ),
                Some((min_a, max_a)) => {
                    assert_eq!(flashes.len(), 1);
                    assert_bell_tint_and_alpha(flashes[0], boundary_rgb, min_a, max_a);
                }
            }
            h.advance_time(0.15);
        }
    }

    #[test]
    fn t2_ux_02_progress_bar_edge_clamping_tiny_rect_and_toggle() {
        let mut h = HeadlessTermHarness::scripted();
        h.set_viewport_size(14.0, 10.0);
        h.runtime
            .input_mut()
            .push_str("\x1b]9;4;1;255\x07\x1b]9;4;99;50\x07");
        let out = h.run_frame();
        assert_eq!(
            h.runtime.progress(),
            Some(ProgressIndicator {
                state: 1,
                progress: 100
            })
        );
        assert_eq!(out.inspect_shapes().non_snapped_rect_count, 0);

        // Zero-area clip or hidden rect never panics.
        h.set_viewport_size(2.0, 2.0);
        let tiny = h.run_frame();
        assert_eq!(tiny.inspect_shapes().non_snapped_rect_count, 0);

        // Non-zero bar clips inside available terminal rect at normal size.
        h.set_viewport_size(240.0, 120.0);
        let normal = h.run_frame();
        let term_rect = normal.terminal_response.response.rect;
        let bars = progress_bar_fills(&normal.inspect_shapes(), term_rect);
        assert_eq!(bars.len(), 2);
        assert!(bars[1].0.max.x <= term_rect.max.x + 0.5);
    }

    #[test]
    fn t2_ux_03_osc_22_overlay_suppression_and_unknown_cursor_names() {
        let mut h = HeadlessTermHarness::scripted();
        h.focus_terminal();

        // Populate scrollback so scrollbar is visible on right edge.
        let mut lines = String::new();
        for i in 0..80 {
            lines.push_str(&format!("line {i}\r\n"));
        }
        lines.push_str("\x1b]22;not-allowed\x07");
        h.runtime.input_mut().push_str(lines);
        let warm = h.run_frame();
        let term_rect = warm.terminal_response.response.rect;

        // Over grid -> NotAllowed.
        let grid_pos = h.cell_center(4, 4).expect("grid cell");
        h.inject_pointer_move(grid_pos);
        let grid_f = h.run_frame();
        assert_eq!(grid_f.platform_output.cursor_icon, CursorIcon::NotAllowed);

        // Over scrollbar track (rightmost edge of term_rect) -> OSC 22 suppressed.
        let sb_pos = pos2(term_rect.max.x - 2.0, term_rect.center().y);
        h.inject_pointer_move(sb_pos);
        let sb_f = h.run_frame();
        assert_ne!(sb_f.platform_output.cursor_icon, CursorIcon::NotAllowed);

        #[cfg(feature = "search")]
        {
            // Over open search bar (top-right inside term_rect) -> OSC 22 suppressed.
            h.runtime.open_search();
            let _ = h.run_frame();
            let search_pos = pos2(term_rect.max.x - 30.0, term_rect.min.y + 18.0);
            h.inject_pointer_move(search_pos);
            let search_f = h.run_frame();
            assert_ne!(search_f.platform_output.cursor_icon, CursorIcon::NotAllowed);
            h.runtime.close_search();
            let _ = h.run_frame();
        }

        // Unknown OSC 22 cursor name clears override cleanly back to Default.
        h.inject_pointer_move(grid_pos);
        h.runtime
            .input_mut()
            .push_str("\x1b]22;totally-unknown\x07");
        let unknown_f = h.run_frame();
        assert_eq!(unknown_f.platform_output.cursor_icon, CursorIcon::Default);
    }

    #[test]
    fn t2_ux_04_modify_other_keys_mac_cmd_and_lock_edge_cases() {
        let mut h = HeadlessTermHarness::scripted();
        h.focus_terminal();
        let _ = h.run_frame();
        h.runtime.input_mut().push_str("\x1b[>4;2m");
        let _ = h.run_frame();
        h.runtime.input_mut().clear_written();

        // `Modifiers::MAC_CMD`, modifier-only keys, and unmodified keys must not emit CSI 27.
        h.tap_key(Key::A, Modifiers::MAC_CMD);
        h.tap_key(Key::ShiftLeft, Modifiers::SHIFT);
        h.tap_key(Key::A, Modifiers::NONE);
        let _ = h.run_frame();
        assert!(
            !h.runtime.input().written_str().contains("\x1b[27;"),
            "unexpected CSI 27 for mac_cmd/unmodified/modifier key: {:?}",
            h.runtime.input().written_str()
        );

        // Repeat Key::CapsLock while held must NOT double-toggle lock state.
        h.runtime.set_lock_modifiers(false, false);
        h.queue_event(Event::Key {
            key: Key::CapsLock,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
        let _ = h.run_frame();
        assert!(h.runtime.caps_lock());
        h.queue_event(Event::Key {
            key: Key::CapsLock,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: Modifiers::NONE,
        });
        let _ = h.run_frame();
        assert!(h.runtime.caps_lock());
        h.queue_event(Event::Key {
            key: Key::CapsLock,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
        let _ = h.run_frame();
        assert!(h.runtime.caps_lock());
        h.runtime.set_lock_modifiers(false, false);

        // Sending lowercase vs uppercase paired Event::Text without Shift updates caps_lock heuristic.
        h.queue_event(Event::Key {
            key: Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
        h.inject_text("A");
        let _ = h.run_frame();
        assert!(h.runtime.caps_lock());

        h.queue_event(Event::Key {
            key: Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
        h.inject_text("a");
        let _ = h.run_frame();
        assert!(!h.runtime.caps_lock());
    }

    #[test]
    fn t2_ux_05_io_error_closed_isolation_overwrite_and_empty_prompt() {
        let mut h = HeadlessTermHarness::scripted();
        h.focus_terminal();
        let _ = h.run_frame();

        // Approve / deny when no OSC 52 prompt is pending safely returns false.
        assert!(!h.runtime.approve_osc52_read("ignored"));
        assert!(!h.runtime.deny_osc52_read());

        // Overwriting successive non-fatal errors surfaces latest in last_io_error & all in events.
        h.runtime
            .input_mut()
            .fail_next_read(TerminalIoError::message("ERR_1"));
        h.runtime
            .input_mut()
            .fail_next_write(TerminalIoError::message("ERR_2"));
        h.inject_text("x");
        let both = h.run_frame();
        assert!(
            both.terminal_response
                .events
                .contains(&TerminalEvent::IoError("ERR_1".to_owned()))
        );
        assert!(
            both.terminal_response
                .events
                .contains(&TerminalEvent::IoError("ERR_2".to_owned()))
        );
        assert_eq!(
            h.runtime.take_io_error(),
            Some(TerminalIoError::Message("ERR_2".to_owned()))
        );
        assert_eq!(h.runtime.take_io_error(), None);

        // approve_osc52_read surfaces write_bytes error cleanly.
        h.config.osc52_read_policy = Osc52ReadPolicy::Prompt;
        let _ = h.run_frame();
        h.runtime.input_mut().push_str("\x1b]52;c;?\x07");
        let _ = h.run_frame();
        h.runtime
            .input_mut()
            .fail_next_write(TerminalIoError::message("ERR_APPROVE"));
        assert!(h.runtime.approve_osc52_read("abc"));
        assert_eq!(
            h.runtime.take_io_error(),
            Some(TerminalIoError::Message("ERR_APPROVE".to_owned()))
        );

        // Closed stream does NOT trigger last_io_error or TerminalEvent::IoError.
        h.runtime.input_mut().close();
        let closed_f = h.run_frame();
        assert!(h.runtime.last_io_error().is_none());
        assert!(
            closed_f
                .terminal_response
                .events
                .iter()
                .all(|e| !matches!(e, TerminalEvent::IoError(_)))
        );
    }

    #[test]
    fn t2_ux_06_osc52_policy_transition_clears_pending_prompt() {
        let cfg = TerminalConfig {
            osc52_read_policy: Osc52ReadPolicy::Prompt,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.runtime.input_mut().push_str("\x1b]52;c;?\x07");
        let _ = h.run_frame();
        assert_eq!(
            h.runtime.pending_osc52_read(),
            Some(ClipboardSelection::Clipboard)
        );

        // Switching to Deny clears pending prompt automatically on next frame.
        h.config.osc52_read_policy = Osc52ReadPolicy::Deny;
        let _ = h.run_frame();
        assert_eq!(h.runtime.pending_osc52_read(), None);
        assert!(!h.runtime.approve_osc52_read("leaked"));
        assert!(h.runtime.input().written_bytes().is_empty());

        // Malformed OSC 6 / OSC 52 invalid base64 leaves state clean.
        h.runtime
            .input_mut()
            .push_str("\x1b]52;c;!!!bad!!!\x07\x1b]6;\x07");
        let _ = h.run_frame();
        assert_eq!(h.runtime.document_uri(), None);
    }

    #[test]
    fn t2_ux_07_dynamic_cell_size_extreme_dpi_and_sixel_clamp() {
        let mut h = HeadlessTermHarness::scripted();
        for (w, h_px, ppp) in [
            (800.0, 600.0, 1.0_f32),
            (640.0, 360.0, 1.25),
            (500.0, 300.0, 1.75),
            (1200.0, 720.0, 2.5),
            (8.0, 8.0, 1.0),
        ] {
            h.set_viewport_size(w, h_px);
            h.set_pixels_per_point(ppp);
            let f = h.run_frame();
            let (cw_px, ch_px) = h.runtime.terminal().cell_pixel_size();
            assert!(cw_px >= 1 && ch_px >= 1);
            assert_eq!(f.inspect_shapes().non_snapped_rect_count, 0);
        }

        // AutoScaleToFit dynamically scales cell_pixel_size when viewport shrinks.
        h.config.scale_mode = ScaleMode::AutoScaleToFit { cols: 40, rows: 12 };
        h.set_viewport_size(800.0, 480.0);
        h.set_pixels_per_point(1.0);
        let _ = h.run_frame();
        let px_large = h.runtime.terminal().cell_pixel_size();
        h.set_viewport_size(320.0, 200.0);
        let _ = h.run_frame();
        let px_small = h.runtime.terminal().cell_pixel_size();
        assert!(px_small.0 < px_large.0 && px_small.1 < px_large.1);
        h.config.scale_mode = ScaleMode::ReflowGrid;

        #[cfg(feature = "sixel")]
        {
            // Oversized 8192x6 Sixel must clamp safely to egui max_texture_side without panic.
            h.set_viewport_size(800.0, 600.0);
            h.set_pixels_per_point(1.0);
            h.runtime.input_mut().push_str("\x1bPq!999999~\x1b\\");
            let out = h.run_frame();
            let max_side = h.ctx.input(|i| i.max_texture_side);
            assert!(out.texture_deltas.set_entries.iter().any(|e| {
                e.texture_id != epaint::TextureId::default()
                    && e.size[0] <= max_side
                    && e.size[1] <= max_side
            }));
        }
    }

    #[test]
    fn t2_ux_08_bold_mode_font_switch_and_faux_bold_column_alignment() {
        let cfg = TerminalConfig {
            cursor_blink: false,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.runtime.input_mut().push_str("\x1b[1mABCD\x1b[0m\r\nABCD");
        let o1 = h.run_frame();
        let origin = o1.terminal_response.response.rect.min;
        let term_w = o1.terminal_response.response.rect.width();
        let cell_h = o1.terminal_response.cell_size.y;
        let i1 = o1.inspect_shapes();

        let r0_rect = Rect::from_min_max(
            pos2(origin.x, origin.y + 0.1),
            pos2(origin.x + term_w, origin.y + cell_h - 0.1),
        );
        let r1_rect = Rect::from_min_max(
            pos2(origin.x, origin.y + cell_h + 0.1),
            pos2(origin.x + term_w, origin.y + 2.0 * cell_h - 0.1),
        );
        let r0_v = i1.text_mesh_vertices_in_rect(r0_rect);
        let r1_v = i1.text_mesh_vertices_in_rect(r1_rect);
        assert_eq!(r0_v.len(), 32);
        assert_eq!(r1_v.len(), 16);

        // Verify column 3 ('D') origin x in FauxBold matches normal row 1 'D' within 0.5px.
        let bold_d_x = r0_v[24..28]
            .iter()
            .map(|v| v.pos.x)
            .fold(f32::INFINITY, f32::min);
        let norm_d_x = r1_v[12..16]
            .iter()
            .map(|v| v.pos.x)
            .fold(f32::INFINITY, f32::min);
        assert!((bold_d_x - norm_d_x).abs() <= 0.5);

        // Switch to monospace_bold then back to default fonts -> vertex count switches 32 -> 16 -> 32.
        let mut bold_defs = FontDefinitions::default();
        if let Some(mono) = bold_defs.families.get(&FontFamily::Monospace).cloned() {
            bold_defs
                .families
                .insert(FontFamily::Name("monospace_bold".into()), mono);
        }
        h.ctx.set_fonts(bold_defs);
        let o2 = h.run_frame();
        assert_eq!(
            o2.inspect_shapes()
                .text_mesh_vertices_in_rect(r0_rect)
                .len(),
            16
        );

        h.ctx.set_fonts(FontDefinitions::default());
        let o3 = h.run_frame();
        assert_eq!(
            o3.inspect_shapes()
                .text_mesh_vertices_in_rect(r0_rect)
                .len(),
            32
        );
    }
}

// ===========================================================================
// Item #11 — Tier 3: Pairwise / Cross-Feature Combinations (4 tests)
// ===========================================================================

mod item11_tier3 {
    use super::*;

    #[test]
    fn t3_ux_01_osc9_4_progress_with_search_bar_scrollbar_and_visual_bell() {
        let cfg = TerminalConfig {
            cursor_blink: false,
            idle_poll_interval_ms: 0,
            visual_bell_duration_ms: 200,
            show_progress_bar: true,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        let mut payload = String::new();
        for i in 0..80 {
            payload.push_str(&format!("log_line_{i} target_token\r\n"));
        }
        payload.push_str("\x1b]9;4;3\x07\x07");
        h.runtime.input_mut().push_str(payload);

        #[cfg(feature = "search")]
        {
            h.runtime.open_search();
            h.runtime.search_bar_mut().query_text = "target_token".to_owned();
        }
        let f1 = h.run_frame();
        let term_rect = f1.terminal_response.response.rect;
        let insp1 = f1.inspect_shapes();
        let bars1 = progress_bar_fills(&insp1, term_rect);
        assert_eq!(bars1.len(), 2);
        assert!(bars1.iter().all(|(r, _)| {
            (r.min.x - r.min.x.round()).abs() <= 1e-3
                && (r.min.y - r.min.y.round()).abs() <= 1e-3
                && (r.max.x - r.max.x.round()).abs() <= 1e-3
                && (r.max.y - r.max.y.round()).abs() <= 1e-3
        }));
        #[cfg(not(feature = "search"))]
        assert_eq!(insp1.non_snapped_rect_count, 0);
        assert_eq!(visual_bell_flash_alphas(&insp1, term_rect).len(), 1);
        assert!(f1.repaint_requested);

        // Visual bell expires after 250ms while indeterminate progress bar stays active.
        h.advance_time(0.25);
        let f2 = h.run_frame();
        let insp2 = f2.inspect_shapes();
        assert!(visual_bell_flash_alphas(&insp2, term_rect).is_empty());
        assert_eq!(progress_bar_fills(&insp2, term_rect).len(), 2);
        assert!(f2.repaint_requested);
        #[cfg(feature = "search")]
        {
            assert!(insp2.contains_text("0/"));
            h.runtime.close_search();
            let f3 = h.run_frame();
            assert_eq!(f3.inspect_shapes().non_snapped_rect_count, 0);
        }
    }

    #[test]
    fn t3_ux_02_osc22_osc8_hover_with_mouse_tracking_and_alt_screen() {
        let mut h = HeadlessTermHarness::scripted();
        h.focus_terminal();
        h.runtime.input_mut().push_str(
            "\x1b]22;grab\x07\x1b[?1000;1006h\x1b]8;;https://example.org/app\x07OPEN\x1b]8;;\x07  IDLE",
        );
        let _ = h.run_frame();

        let link_pos = h.cell_center(1, 0).expect("link cell");
        let idle_pos = h.cell_center(6, 0).expect("idle cell");

        // Under ?1000h mouse capture without Shift, clicking OPEN emits SGR mouse instead of opening URL.
        h.advance_time(0.5);
        h.inject_click(link_pos, PointerButton::Primary, Modifiers::NONE);
        let captured = h.last_outcome().expect("captured frame");
        assert!(captured.opened_urls().is_empty());
        assert!(h.runtime.input().written_str().contains("\x1b[<0;2;1M"));
        h.runtime.input_mut().clear_written();

        // Alt-screen switch keeps OSC 22 pointer cursor active on hover.
        h.runtime.input_mut().push_str("\x1b[?1049hALT_BODY");
        h.inject_pointer_move(idle_pos);
        let alt_f = h.run_frame();
        assert!(h.runtime.terminal().is_alt_screen());
        assert_eq!(alt_f.platform_output.cursor_icon, CursorIcon::Grab);

        // Exit alt-screen and disable mouse tracking -> hyperlink activation & PointingHand work.
        h.runtime.input_mut().push_str("\x1b[?1049l\x1b[?1000l");
        let _ = h.run_frame();
        #[cfg(feature = "hyperlinks")]
        {
            h.inject_pointer_move(link_pos);
            let hover_link = h.run_frame();
            assert_eq!(
                hover_link.platform_output.cursor_icon,
                CursorIcon::PointingHand
            );
            h.advance_time(0.5);
            h.inject_click(link_pos, PointerButton::Primary, Modifiers::NONE);
            let clicked = h.last_outcome().expect("clicked link");
            assert_eq!(
                clicked.opened_urls(),
                vec!["https://example.org/app".to_owned()]
            );
        }
    }

    #[test]
    fn t3_ux_03_modify_other_keys_kitty_precedence_lock_mods_and_multipane() {
        let mut panes = MultiPaneHarness::new(ScriptedInput::new(), ScriptedInput::new());
        panes.set_viewport_size(900.0, 500.0);
        let _ = panes.run_split_frame();

        panes.left.input_mut().push_str("\x1b[>4;2m");
        panes.left.set_lock_modifiers(true, false);
        #[cfg(feature = "kitty-keyboard")]
        panes.right.input_mut().push_str("\x1b[>1u");
        #[cfg(not(feature = "kitty-keyboard"))]
        panes.right.input_mut().push_str("\x1b[>4;1m");
        panes.right.set_lock_modifiers(false, true);
        let _ = panes.run_split_frame();

        // Focus left pane and type Ctrl+Enter + Key::Clear.
        panes.queue_event(Event::PointerMoved(pos2(120.0, 120.0)));
        panes.queue_event(Event::PointerButton {
            pos: pos2(120.0, 120.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        panes.queue_event(Event::PointerButton {
            pos: pos2(120.0, 120.0),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        let _ = panes.run_split_frame();
        panes.left.input_mut().clear_written();
        panes.right.input_mut().clear_written();

        panes.queue_event(Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::CTRL,
        });
        panes.queue_event(Event::Key {
            key: Key::Clear,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
        let _ = panes.run_split_frame();
        assert_eq!(panes.left.input().written_str(), "\x1b[27;5;13~\x1b[E");
        assert!(panes.right.input().written_bytes().is_empty());

        // Focus right pane and inject Key::Clear + Ctrl+Enter.
        panes.queue_event(Event::PointerMoved(pos2(680.0, 120.0)));
        panes.queue_event(Event::PointerButton {
            pos: pos2(680.0, 120.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        panes.queue_event(Event::PointerButton {
            pos: pos2(680.0, 120.0),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        let _ = panes.run_split_frame();
        panes.left.input_mut().clear_written();
        panes.right.input_mut().clear_written();

        panes.queue_event(Event::Key {
            key: Key::Clear,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
        panes.queue_event(Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::CTRL,
        });
        let _ = panes.run_split_frame();
        assert!(panes.left.input().written_bytes().is_empty());
        #[cfg(feature = "kitty-keyboard")]
        assert_eq!(panes.right.input().written_str(), "\x1b[57427u\x1b[13;133u");
        #[cfg(not(feature = "kitty-keyboard"))]
        assert_eq!(panes.right.input().written_str(), "\x1b[E\x1b[27;5;13~");
    }

    #[test]
    fn t3_ux_04_io_error_osc52_prompt_and_inline_image_revision_recovery() {
        let cfg = TerminalConfig {
            resize_debounce_secs: 0.0,
            osc52_read_policy: Osc52ReadPolicy::Prompt,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.set_viewport_size(640.0, 360.0);
        let _ = h.run_frame();

        h.runtime.input_mut().push_str("\x1b]52;c;?\x07");
        h.runtime
            .input_mut()
            .fail_next_read(TerminalIoError::message("TEMP_READ_ERR"));
        let err_f = h.run_frame();
        assert!(
            err_f
                .terminal_response
                .events
                .contains(&TerminalEvent::IoError("TEMP_READ_ERR".to_owned()))
        );
        assert!(h.runtime.take_io_error().is_some());

        // Drain the queued OSC 52 prompt now that read succeeds.
        let _ = h.run_frame();
        assert_eq!(
            h.runtime.pending_osc52_read(),
            Some(ClipboardSelection::Clipboard)
        );

        // Fail approve write once -> pending prompt consumed and error surfaced; next prompt succeeds.
        h.runtime
            .input_mut()
            .fail_next_write(TerminalIoError::message("TEMP_WRITE_ERR"));
        assert!(h.runtime.approve_osc52_read("first"));
        assert_eq!(
            h.runtime.take_io_error(),
            Some(TerminalIoError::Message("TEMP_WRITE_ERR".to_owned()))
        );

        h.runtime.input_mut().push_str("\x1b]52;c;?\x07");
        let _ = h.run_frame();
        assert!(h.runtime.approve_osc52_read("ok"));
        assert_eq!(h.runtime.input().written_str(), "\x1b]52;c;b2s=\x1b\\");

        #[cfg(feature = "kitty-graphics")]
        {
            let blue_b64 = encode_base64(&[0, 0, 255, 255].repeat(4));
            h.runtime.input_mut().push_str(format!(
                "\x1b_Ga=T,i=9,p=1,c=2,r=2,f=32,s=2,v=2,C=1;{blue_b64}\x1b\\"
            ));
            let o1 = h.run_frame();
            let tex_id = o1
                .inspect_shapes()
                .mesh_texture_ids
                .into_iter()
                .find(|id| *id != epaint::TextureId::default())
                .expect("kitty texture id");
            let hash1 = o1
                .texture_deltas
                .set_entries
                .iter()
                .find(|e| e.texture_id == tex_id)
                .expect("first upload")
                .pixel_hash;

            // Change DPI + replace image data -> dynamic cell_pixel_size updates and revision re-uploads.
            h.set_pixels_per_point(2.0);
            let white_b64 = encode_base64(&[255, 255, 255, 255].repeat(4));
            h.runtime.input_mut().push_str(format!(
                "\x1b_Ga=T,i=9,p=1,c=2,r=2,f=32,s=2,v=2,C=1;{white_b64}\x1b\\"
            ));
            let o2 = h.run_frame();
            let hash2 = o2
                .texture_deltas
                .set_entries
                .iter()
                .find(|e| e.texture_id == tex_id)
                .expect("revision upload")
                .pixel_hash;
            assert_ne!(hash1, hash2);
            assert_eq!(o2.inspect_shapes().non_snapped_rect_count, 0);
        }
    }
}

// ===========================================================================
// Item #11 — Tier 4: Real-World Interactive IDE Terminal & PTY Workload (1 test)
// ===========================================================================

#[test]
fn w_ux_01_interactive_ide_terminal_session_ux_and_pty() {
    let cfg = TerminalConfig {
        cursor_blink: false,
        idle_poll_interval_ms: 0,
        resize_debounce_secs: 0.0,
        visual_bell_duration_ms: 180,
        show_progress_bar: true,
        osc52_write_enabled: true,
        osc52_read_policy: Osc52ReadPolicy::Prompt,
        ..TerminalConfig::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.set_viewport_size(960.0, 540.0);
    h.set_pixels_per_point(1.25);
    h.focus_terminal();

    // Phase 1: IDE session starts — OSC 6 doc URI, bold banner, OSC 9;4 build progress,
    // OSC 99 notification, visual bell, OSC 22 crosshair + OSC 8 link.
    h.runtime.input_mut().push_str(concat!(
        "\x1b]6;file:///repo/Cargo.toml\x07",
        "\x1b[1;32mIDE_BUILD\x1b[0m ",
        "\x1b]8;;https://doc.rust-lang.org\x07DOCS\x1b]8;;\x07\r\n",
        "\x1b]9;4;1;60\x07",
        "\x1b]99;i=ide:p=title:d=0;Cargo\x1b\\",
        "\x1b]99;i=ide:p=body:d=1;Compiling\x1b\\",
        "\x1b]22;crosshair\x07",
        "\x07",
    ));
    let link_pos = h.cell_center(11, 0).expect("DOCS link cell");
    let plain_pos = h.cell_center(2, 2).expect("plain grid cell");
    h.inject_pointer_move(plain_pos);
    let p1 = h.run_frame();
    let term_rect = p1.terminal_response.response.rect;
    let insp1 = p1.inspect_shapes();

    assert_eq!(h.runtime.document_uri(), Some("file:///repo/Cargo.toml"));
    assert_eq!(
        h.runtime.progress(),
        Some(ProgressIndicator {
            state: 1,
            progress: 60
        })
    );
    assert_eq!(p1.platform_output.cursor_icon, CursorIcon::Crosshair);
    assert!(!insp1.text_mesh_vertices().is_empty());
    assert_eq!(progress_bar_fills(&insp1, term_rect).len(), 2);
    assert_eq!(visual_bell_flash_alphas(&insp1, term_rect).len(), 1);
    assert!(p1.terminal_response.events.contains(&TerminalEvent::Bell));
    assert!(
        p1.terminal_response
            .events
            .contains(&TerminalEvent::Notification {
                title: "Cargo".to_owned(),
                body: "Compiling".to_owned()
            })
    );

    // Phase 2: Visual bell expires, OSC 8 hyperlink hover overrides OSC 22, OSC 52 Prompt approves.
    h.advance_time(0.25);
    h.inject_pointer_move(link_pos);
    h.runtime
        .input_mut()
        .push_str("\x1b]9;4;0\x07\x1b]52;c;?\x07\x1b[>4;2m");
    let p2 = h.run_frame();
    let insp2 = p2.inspect_shapes();
    assert!(visual_bell_flash_alphas(&insp2, term_rect).is_empty());
    assert!(progress_bar_fills(&insp2, term_rect).is_empty());
    #[cfg(feature = "hyperlinks")]
    assert_eq!(p2.platform_output.cursor_icon, CursorIcon::PointingHand);
    assert!(h.runtime.approve_osc52_read("ide_clip"));
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b]52;c;aWRlX2NsaXA=\x1b\\"
    );
    h.runtime.input_mut().clear_written();

    // Phase 3: modifyOtherKeys + Key::Clear + transient I/O error recovery + Kitty revision bump.
    h.runtime.set_lock_modifiers(true, false);
    h.tap_key(Key::Enter, Modifiers::SHIFT);
    h.tap_key(Key::Clear, Modifiers::NONE);
    h.runtime
        .input_mut()
        .fail_next_read(TerminalIoError::message("IDE_TRANSIENT_EIO"));
    let p3 = h.run_frame();
    assert!(
        p3.terminal_response
            .events
            .contains(&TerminalEvent::IoError("IDE_TRANSIENT_EIO".to_owned()))
    );
    assert!(h.runtime.take_io_error().is_some());
    assert_eq!(h.runtime.input().written_str(), "\x1b[27;2;13~\x1b[E");

    #[cfg(feature = "kitty-graphics")]
    {
        let img1 = encode_base64(&[10, 200, 30, 255].repeat(4));
        h.runtime.input_mut().push_str(format!(
            "\x1b_Ga=T,i=42,p=1,c=2,r=2,f=32,s=2,v=2,C=1;{img1}\x1b\\"
        ));
        let k1 = h.run_frame();
        let tex_id = k1
            .inspect_shapes()
            .mesh_texture_ids
            .into_iter()
            .find(|id| *id != epaint::TextureId::default())
            .expect("kitty texture");
        let h1 = k1
            .texture_deltas
            .set_entries
            .iter()
            .find(|e| e.texture_id == tex_id)
            .expect("delta 1")
            .pixel_hash;

        let img2 = encode_base64(&[200, 10, 30, 255].repeat(4));
        h.runtime.input_mut().push_str(format!(
            "\x1b_Ga=T,i=42,p=1,c=2,r=2,f=32,s=2,v=2,C=1;{img2}\x1b\\"
        ));
        let k2 = h.run_frame();
        let h2 = k2
            .texture_deltas
            .set_entries
            .iter()
            .find(|e| e.texture_id == tex_id)
            .expect("delta 2")
            .pixel_hash;
        assert_ne!(h1, h2);
    }

    // Phase 4 (desktop + `pty` feature): Live PTY session streaming UX OSCs + graceful exit.
    #[cfg(all(not(target_arch = "wasm32"), feature = "pty"))]
    {
        use std::time::Duration;

        use egui_xterm::pty::{PtyBackend, PtyConfig};
        use egui_xterm::{TerminalInput, TerminalSize};
        use egui_xterm_e2e::PtyTestSession;

        let mut sess = PtyTestSession::spawn_interactive_shell(80, 24).expect("spawn shell");
        sess.harness.set_viewport_size(800.0, 480.0);
        let _ = sess.harness.run_frame();

        #[cfg(windows)]
        sess.send_line("echo PTY_UX_READY")
            .expect("send echo to Windows PTY");
        #[cfg(not(windows))]
        sess.send_line(
            "printf '\\033]6;file:///tmp/pty.rs\\007\\033]9;4;1;75\\007\\033]22;wait\\007\\033[1mPTY%s\\033[0m\\n' '_UX_READY'",
        )
        .expect("send OSC/bold command to Unix PTY");

        assert!(
            sess.run_until_contains("PTY_UX_READY", Duration::from_secs(8))
                .expect("poll PTY_UX_READY"),
            "live PTY did not emit PTY_UX_READY"
        );
        #[cfg(not(windows))]
        {
            assert_eq!(
                sess.harness.runtime.document_uri(),
                Some("file:///tmp/pty.rs")
            );
            assert_eq!(
                sess.harness.runtime.progress(),
                Some(ProgressIndicator {
                    state: 1,
                    progress: 75
                })
            );
        }
        let pty_out = sess.harness.run_frame();
        assert!(!pty_out.inspect_shapes().text_mesh_vertices().is_empty());

        sess.send_line("exit").expect("exit shell");
        let closed = sess.harness.run_until(300, 1.0 / 60.0, |harness, _| {
            std::thread::sleep(Duration::from_millis(10));
            harness.runtime.input().is_closed()
        });
        assert!(closed, "PTY did not close after exit");
        assert!(sess.harness.runtime.last_io_error().is_none());

        // Spawning with a nonexistent working directory returns an error cleanly without panic.
        let bad_spawn = PtyBackend::spawn(PtyConfig {
            shell: None,
            args: Vec::new(),
            cwd: Some(std::path::PathBuf::from(
                "/nonexistent_egui_xterm_wave3_ux_dir",
            )),
            env: Vec::new(),
            initial_size: TerminalSize::new(80, 24),
        });
        assert!(bad_spawn.is_err());
    }
}

#[cfg(feature = "kitty-graphics")]
fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(char::from(TABLE[((n >> 18) & 63) as usize]));
        out.push(char::from(TABLE[((n >> 12) & 63) as usize]));
        out.push(if chunk.len() > 1 {
            char::from(TABLE[((n >> 6) & 63) as usize])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(TABLE[(n & 63) as usize])
        } else {
            '='
        });
    }
    out
}
