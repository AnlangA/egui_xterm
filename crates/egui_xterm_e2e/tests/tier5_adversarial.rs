//! Tier 5 — Adversarial Hardening E2E Tests (22 opaque-box specs).
//!
//! Covers core engine edge cases (encoding, parser overflow, UTF-8 maximal subparts,
//! C1 split chunks, grapheme/1-col reflow, DECSTBM/DECOM/DEC2026, block selection,
//! palette overrides / `set_theme` / `DEC 2026` latching / `serde` compat,
//! search/hyperlinks/shell-integration/serde hardening) and `egui_xterm` runtime/renderer
//! edge cases (theme switching, OSC 4/12, vector-glyphs with extras, tiny-container
//! click isolation, slow-path BiDi/ZWJ alignment, Kitty Flag 2 fallback, lost pointer
//! release, 2-byte budget + font reset, HiDPI AutoScaleToFit SGR mouse, and PTY burst).

use egui::{
    Color32, Context, Event, FontDefinitions, Modifiers, PointerButton, Pos2, Rect, UiBuilder,
    pos2, vec2,
};
use egui_xterm::{
    ColorScheme, CoreConfig, PaletteOverrides, SelectionMode, Terminal, TerminalConfig,
    TerminalRuntime, Theme,
};
use egui_xterm_core::{C1Policy, PackedColor};
use egui_xterm_e2e::{HeadlessTermHarness, ScriptedInput, run_headless_ui};

// ---------------------------------------------------------------------------
// Core Engine Adversarial Specs (12 tests)
// ---------------------------------------------------------------------------

#[cfg(feature = "kitty-keyboard")]
#[test]
fn t5_f14_kitty_lock_modifiers_preserve_legacy_keys() {
    use egui_xterm::{
        KittyEventType, KittyKey, KittyKeyEvent, KittyKeyboardFlags, KittyModifiers,
        encode_kitty_key,
    };

    let flags = KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES;
    let lock_cases = [
        KittyModifiers {
            num_lock: true,
            ..KittyModifiers::default()
        },
        KittyModifiers {
            caps_lock: true,
            ..KittyModifiers::default()
        },
        KittyModifiers {
            num_lock: true,
            caps_lock: true,
            ..KittyModifiers::default()
        },
    ];

    let key_expectations: &[(KittyKey, &[u8])] = &[
        (KittyKey::Enter, b"\r"),
        (KittyKey::Tab, b"\t"),
        (KittyKey::Backspace, b"\x7f"),
        (KittyKey::Space, b" "),
        (KittyKey::Up, b"\x1b[A"),
        (KittyKey::F(1), b"\x1bOP"),
        (KittyKey::F(3), b"\x1b[13~"),
        (KittyKey::Escape, b"\x1b[27u"),
    ];

    for mods in lock_cases {
        for &(key, expected) in key_expectations {
            let ev = KittyKeyEvent {
                key,
                event_type: KittyEventType::Press,
                modifiers: mods,
                shifted_key: None,
                base_layout_key: None,
                associated_text: None,
            };
            assert_eq!(
                encode_kitty_key(flags, &ev).as_deref(),
                Some(expected),
                "lock modifiers {mods:?} should preserve legacy/disambiguate output for {key:?}"
            );
        }
    }

    // REPORT_ALL_KEYS_AS_ESCAPE_CODES still reports lock bits when active.
    let all_flags = KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES
        | KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
    let ev = KittyKeyEvent {
        key: KittyKey::Enter,
        event_type: KittyEventType::Press,
        modifiers: KittyModifiers {
            num_lock: true,
            ..KittyModifiers::default()
        },
        shifted_key: None,
        base_layout_key: None,
        associated_text: None,
    };
    assert_eq!(
        encode_kitty_key(all_flags, &ev).as_deref(),
        Some(b"\x1b[13;129u".as_slice())
    );

    // Legacy mode (`KittyKeyboardFlags::empty()`) must mask out lock-key bits
    // (64 = CapsLock, 128 = NumLock) when non-lock modifiers are held.
    let legacy_cases: &[(KittyKey, KittyModifiers, &[u8])] = &[
        (
            KittyKey::Up,
            KittyModifiers {
                num_lock: true,
                ..KittyModifiers::default()
            },
            b"\x1b[A",
        ),
        (
            KittyKey::Up,
            KittyModifiers {
                ctrl: true,
                num_lock: true,
                ..KittyModifiers::default()
            },
            b"\x1b[1;5A",
        ),
        (
            KittyKey::Up,
            KittyModifiers {
                ctrl: true,
                caps_lock: true,
                ..KittyModifiers::default()
            },
            b"\x1b[1;5A",
        ),
        (
            KittyKey::Up,
            KittyModifiers {
                ctrl: true,
                caps_lock: true,
                num_lock: true,
                ..KittyModifiers::default()
            },
            b"\x1b[1;5A",
        ),
        (
            KittyKey::Up,
            KittyModifiers {
                shift: true,
                ctrl: true,
                num_lock: true,
                ..KittyModifiers::default()
            },
            b"\x1b[1;6A",
        ),
        (
            KittyKey::F(1),
            KittyModifiers {
                ctrl: true,
                caps_lock: true,
                ..KittyModifiers::default()
            },
            b"\x1b[1;5P",
        ),
        (
            KittyKey::Char('a'),
            KittyModifiers {
                ctrl: true,
                num_lock: true,
                ..KittyModifiers::default()
            },
            b"\x01",
        ),
    ];

    for &(key, modifiers, expected) in legacy_cases {
        let legacy_ev = KittyKeyEvent {
            key,
            event_type: KittyEventType::Press,
            modifiers,
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        assert_eq!(
            encode_kitty_key(KittyKeyboardFlags::empty(), &legacy_ev).as_deref(),
            Some(expected),
            "legacy encoding failed for key={key:?}, mods={modifiers:?}"
        );
    }

    // Legacy fallback under REPORT_EVENT_TYPES on Press for Ctrl+Char+NumLock still emits Ctrl+A.
    let fallback_ev = KittyKeyEvent {
        key: KittyKey::Char('a'),
        event_type: KittyEventType::Press,
        modifiers: KittyModifiers {
            ctrl: true,
            num_lock: true,
            ..KittyModifiers::default()
        },
        shifted_key: None,
        base_layout_key: None,
        associated_text: None,
    };
    assert_eq!(
        encode_kitty_key(KittyKeyboardFlags::REPORT_EVENT_TYPES, &fallback_ev).as_deref(),
        Some(b"\x01".as_slice())
    );
}

#[test]
fn t5_f7_decstr_resets_reverse_video_and_bumps_damage_epoch() {
    let mut term = Terminal::new(20, 4, CoreConfig::default());
    term.advance(b"hello\x1b[?5h");
    assert!(term.modes().reverse_video);
    let e0 = term.damage_epoch();
    let snap_e0 = term.active_snapshot().damage_epoch;
    assert_eq!(e0, snap_e0);

    term.advance(b"\x1b[!p");
    assert!(!term.modes().reverse_video);
    assert!(term.damage_epoch() > e0);
    assert!(term.active_snapshot().damage_epoch > snap_e0);
    assert_eq!(term.visible_line_text(0).as_deref(), Some("hello"));
}

#[test]
fn t5_f6_full_text_trims_trailing_spaces_on_erased_soft_wrap_tail() {
    let mut term = Terminal::new(5, 3, CoreConfig::default());
    term.advance(b"ab   c\x1b[2;1H\x1b[2K");
    assert_eq!(term.visible_line_text(0).as_deref(), Some("ab"));
    assert_eq!(term.full_text(), "ab");

    term.resize(10, 3);
    assert_eq!(term.full_text(), "ab");

    // Mid-buffer erased soft-wrap continuation with another non-empty line below (`end_idx > 1`).
    let mut mid = Terminal::new(5, 4, CoreConfig::default());
    mid.advance(b"ab   c\x1b[2;1H\x1b[2K\x1b[3;1Hxyz");
    assert_eq!(mid.full_text(), "ab\nxyz");
    mid.resize(10, 4);
    assert_eq!(mid.full_text(), "ab\nxyz");

    // Non-default BCE-colored trailing spaces on soft-wrapped row remain preserved across reflow.
    let mut bce = Terminal::new(5, 4, CoreConfig::default());
    bce.advance(b"\x1b[41mab   \x1b[0mc\x1b[2;1H\x1b[2K\x1b[3;1Hxyz");
    assert_eq!(bce.full_text(), "ab   \nxyz");
    bce.resize(10, 4);
    assert_eq!(bce.full_text(), "ab   \nxyz");

    // Completely blank soft-wrapped logical line before non-empty line and at buffer tail.
    let mut blank_mid = Terminal::new(5, 4, CoreConfig::default());
    blank_mid.advance(b"      \x1b[2;1H\x1b[2K\x1b[3;1Hxyz");
    assert_eq!(blank_mid.full_text(), "\nxyz");
    blank_mid.resize(10, 4);
    assert_eq!(blank_mid.full_text(), "\nxyz");

    let mut blank_tail = Terminal::new(5, 4, CoreConfig::default());
    blank_tail.advance(b"ab\r\n      \x1b[3;1H\x1b[2K");
    assert_eq!(blank_tail.full_text(), "ab");
    blank_tail.resize(10, 4);
    assert_eq!(blank_tail.full_text(), "ab");
}

#[test]
fn t5_f8_f9_f15_set_theme_palette_overrides_dec2026_and_serde() {
    let mut term = Terminal::new(20, 4, CoreConfig::default());
    let mut light = Theme::light();
    light.scheme.selection_fg = Some([12, 34, 56]);

    // 1. Apply OSC 10/11/12/4 overrides (both base ANSI slot 1 and extended 256 slot 196).
    term.advance(
        b"\x1b]10;#112233\x07\x1b]11;#445566\x07\x1b]12;#778899\x07\x1b]4;1;rgb:aa/bb/cc\x07\x1b]4;196;rgb:10/20/30\x07",
    );
    assert_eq!(
        *term.palette_overrides(),
        PaletteOverrides {
            foreground: true,
            background: true,
            cursor: true,
            ansi: {
                let mut a = [false; 16];
                a[1] = true;
                a
            },
        }
    );
    assert_eq!(term.palette().foreground, [0x11, 0x22, 0x33]);
    assert_eq!(term.palette().background, [0x44, 0x55, 0x66]);
    assert_eq!(term.palette().cursor, [0x77, 0x88, 0x99]);
    assert_eq!(term.palette().ansi[1], [0xAA, 0xBB, 0xCC]);
    assert_eq!(term.extended_palette()[196 - 16], Some([0x10, 0x20, 0x30]));
    assert_eq!(
        term.active_snapshot().resolve_indexed_color(196),
        [0x10, 0x20, 0x30]
    );

    // 2. `set_theme` updates non-overridden slots while preserving explicit OSC overrides.
    term.set_theme(light.clone());
    assert_eq!(term.palette().foreground, [0x11, 0x22, 0x33]);
    assert_eq!(term.palette().background, [0x44, 0x55, 0x66]);
    assert_eq!(term.palette().cursor, [0x77, 0x88, 0x99]);
    assert_eq!(term.palette().ansi[1], [0xAA, 0xBB, 0xCC]);
    assert_eq!(term.palette().ansi[2], light.scheme.ansi[2]);
    assert_eq!(term.palette().cursor_text, light.scheme.cursor_text);
    assert_eq!(term.palette().selection_bg, light.scheme.selection_bg);
    assert_eq!(term.palette().selection_fg, light.scheme.selection_fg);
    assert_eq!(term.extended_palette()[196 - 16], Some([0x10, 0x20, 0x30]));

    // 3. OSC 110/111/112/104 reset overrides back to the active theme (`light`).
    term.advance(b"\x1b]110\x07\x1b]111\x07\x1b]112\x07\x1b]104;1\x07\x1b]104;196\x07");
    assert_eq!(*term.palette_overrides(), PaletteOverrides::default());
    assert_eq!(term.palette().foreground, light.scheme.foreground);
    assert_eq!(term.palette().background, light.scheme.background);
    assert_eq!(term.palette().cursor, light.scheme.cursor);
    assert_eq!(term.palette().ansi[1], light.scheme.ansi[1]);
    assert_eq!(term.extended_palette()[196 - 16], None);
    assert_eq!(
        term.active_snapshot().resolve_indexed_color(196),
        ColorScheme::xterm_256_color(196, &light.scheme.ansi)
    );

    // 4. DEC 2026 latching freezes snapshot palette, palette_overrides, and extended_palette.
    term.advance_with_time(b"\x1b[?2026h", 1.0);
    assert!(term.is_synchronized_output());
    term.set_theme(Theme::dark());
    term.advance_with_time(b"\x1b]4;196;rgb:de/ad/be\x07\x1b]12;#abcdef\x07", 1.05);
    {
        let frozen = term.active_snapshot();
        assert_eq!(frozen.palette.background, light.scheme.background);
        assert_eq!(frozen.palette.cursor, light.scheme.cursor);
        assert!(!frozen.palette_overrides.cursor);
        assert_eq!(frozen.extended_palette[196 - 16], None);
        assert_eq!(
            frozen.resolve_indexed_color(196),
            ColorScheme::xterm_256_color(196, &light.scheme.ansi)
        );
    }
    term.advance_with_time(b"\x1b[?2026l", 1.1);
    assert!(!term.is_synchronized_output());
    {
        let live = term.active_snapshot();
        assert_eq!(live.palette.background, Theme::dark().scheme.background);
        assert_eq!(live.palette.cursor, [0xAB, 0xCD, 0xEF]);
        assert!(live.palette_overrides.cursor);
        assert_eq!(live.extended_palette[196 - 16], Some([0xDE, 0xAD, 0xBE]));
        assert_eq!(live.resolve_indexed_color(196), [0xDE, 0xAD, 0xBE]);
    }

    // 5. `serde` roundtrip and backward compatibility when `palette_overrides` is omitted.
    #[cfg(feature = "serde")]
    {
        use egui_xterm::TerminalSnapshot;

        term.advance(b"\x1b]4;3;rgb:01/02/03\x07");
        let snap = term.export_snapshot();
        let json = serde_json::to_string(&snap).expect("serialize snapshot");
        let roundtrip: TerminalSnapshot = serde_json::from_str(&json).expect("roundtrip snapshot");
        let restored = Terminal::from_snapshot(roundtrip, CoreConfig::default());
        assert_eq!(restored.palette_overrides(), term.palette_overrides());
        assert_eq!(restored.extended_palette(), term.extended_palette());
        assert_eq!(restored.palette(), term.palette());

        let mut val: serde_json::Value = serde_json::from_str(&json).expect("parse json value");
        let obj = val.as_object_mut().expect("snapshot json object");
        obj.remove("palette_overrides");
        obj.insert(
            "extended_palette".to_owned(),
            serde_json::json!([[5, [1, 2, 3]], [200, [4, 5, 6]]]),
        );
        let compat_snap: TerminalSnapshot =
            serde_json::from_value(val).expect("backward-compat snapshot");
        let compat = Terminal::from_snapshot(compat_snap, CoreConfig::default());
        assert_eq!(*compat.palette_overrides(), PaletteOverrides::default());
        assert_eq!(compat.extended_palette()[200 - 16], Some([4, 5, 6]));
        assert_ne!(compat.palette().ansi[5], [1, 2, 3]);
    }

    // 6. ESC c (RIS) resets palette, palette_overrides, and extended_palette.
    term.advance(b"\x1bc");
    assert_eq!(*term.palette_overrides(), PaletteOverrides::default());
    assert!(term.extended_palette().iter().all(Option::is_none));
    assert_eq!(*term.palette(), Theme::dark().scheme);
}

#[test]
fn t5_f3_parser_overflow_utf8_invalid_subparts_and_c1_split_chunks() {
    // 1. 200-param CSI and 10-intermediate CSI overflow cleanly without corrupting subsequent text.
    let mut term = Terminal::new(80, 6, CoreConfig::default());
    let many_params = vec!["1"; 200].join(";");
    let csi_overflow = format!("\x1b[{many_params}mOK\x1b[1          mNO");
    term.advance(csi_overflow.as_bytes());
    assert_eq!(term.visible_line_text(0).as_deref(), Some("OKNO"));

    // 2. UTF-8 maximal-subpart U+FFFD counts: 2 + 3 + 3 + 4 = 12 replacement chars.
    let mut utf8_term = Terminal::new(40, 4, CoreConfig::default());
    utf8_term.advance(b"\xC0\xAF\xE0\x80\xAF\xED\xA0\x80\xF4\x90\x80\x80");
    let line = utf8_term.visible_line_text(0).unwrap_or_default();
    assert_eq!(line.chars().count(), 12);
    assert!(line.chars().all(|c| c == '\u{FFFD}'));

    // 3. Oversized OSC exceeds max_osc_bytes=64 and is ignored cleanly.
    let osc_cfg = CoreConfig {
        max_osc_bytes: 64,
        ..CoreConfig::default()
    };
    let mut osc_term = Terminal::new(40, 4, osc_cfg);
    let long_title = "x".repeat(120);
    osc_term.advance(format!("\x1b]2;keep\x07\x1b]2;{long_title}\x07after").as_bytes());
    assert_eq!(osc_term.title(), "keep");
    assert_eq!(osc_term.visible_line_text(0).as_deref(), Some("after"));

    // 4. Byte-by-byte C1Policy::Vt500EightBit stream.
    let c1_cfg = CoreConfig {
        c1_policy: C1Policy::Vt500EightBit,
        ..CoreConfig::default()
    };
    let mut c1_term = Terminal::new(40, 4, c1_cfg);
    let c1_stream = b"\x9D2;c1\x9C\x9B1;3H\x90qpayload\x9CHi\x85Next";
    for &b in c1_stream {
        c1_term.advance(&[b]);
    }
    assert_eq!(c1_term.title(), "c1");
    assert_eq!(c1_term.visible_line_text(0).as_deref(), Some("  Hi"));
    assert_eq!(c1_term.visible_line_text(1).as_deref(), Some("Next"));
}

#[test]
fn t5_f4_f6_grapheme_zwj_ri_zalgo_cap_and_1col_reflow_roundtrip() {
    let assert_wide_invariants = |term: &Terminal| {
        let (cols, rows) = term.size();
        for r in 0..rows {
            let mut c = 0u16;
            while c < cols {
                let cell = term.cell_at(c, r).expect("valid cell");
                assert!(!cell.is_wide_spacer, "orphan wide spacer at ({c}, {r})");
                if cell.is_wide {
                    assert!(cols >= 2, "wide lead cannot exist at 1 col");
                    let next = term.cell_at(c + 1, r).expect("wide spacer present");
                    assert!(
                        next.is_wide_spacer,
                        "missing wide spacer at ({}, {r})",
                        c + 1
                    );
                    c += 2;
                } else {
                    c += 1;
                }
            }
        }
    };

    let mut term = Terminal::new(20, 4, CoreConfig::default());
    let mut payload = String::from("e\u{0301}👩\u{200D}🔬🇺🇸\u{2764}\u{FE0F}a");
    for _ in 0..50 {
        payload.push('\u{0301}');
    }
    term.advance(payload.as_bytes());

    let zalgo_cell = term.cell_at(7, 0).expect("zalgo cell");
    assert_eq!(zalgo_cell.ch, 'a');
    assert!(
        zalgo_cell.grapheme.chars().count() <= 33,
        "combining cap exceeded: {}",
        zalgo_cell.grapheme.chars().count()
    );

    let initial_text = term.full_text();
    assert_wide_invariants(&term);

    term.resize(1, 8);
    assert_wide_invariants(&term);

    term.resize(20, 4);
    assert_wide_invariants(&term);
    assert_eq!(term.full_text(), initial_text);
}

#[test]
fn t5_f7_f9_decstbm_decom_alt_screen_and_dec2026_watchdog_interaction() {
    let mut term = Terminal::new(20, 6, CoreConfig::default());
    term.advance(b"\x1b[2;4r\x1b[?6h\x1b[1;1Hpri");
    assert_eq!(term.visible_line_text(1).as_deref(), Some("pri"));

    term.advance_with_time(b"\x1b[?2026h\x1b[?1049halt", 2.0);
    assert!(term.is_synchronized_output());
    assert_eq!(term.render_snapshot_line_text(1).as_deref(), Some("pri"));

    term.advance_with_time(b"", f64::NAN);
    assert!(term.is_synchronized_output());

    term.advance_with_time(b"", 2.26);
    assert!(!term.is_synchronized_output());
    assert!(term.is_alt_screen());
    assert_eq!(term.render_snapshot_line_text(0).as_deref(), Some("alt"));

    term.advance(b"\x1b[?1049l");
    assert!(!term.is_alt_screen());
    assert_eq!(term.visible_line_text(1).as_deref(), Some("pri"));
}

#[test]
fn t5_f10_f6_wide_block_selection_and_trailing_blank_reflow() {
    let mut term = Terminal::new(10, 4, CoreConfig::default());
    term.advance("中文ab\r\ncd测试".as_bytes());
    term.selection_block(0, 1);
    term.selection_update(1, 2);
    assert_eq!(term.selection_text().as_deref(), Some("中文\nd测"));

    let mut t2 = Terminal::new(10, 4, CoreConfig::default());
    t2.advance(b"hello\x1b[1;9H");
    t2.selection_start(0, 6, SelectionMode::Stream);
    t2.selection_update(0, 9);
    let before = t2.selection_text();
    t2.resize(4, 4);
    t2.resize(10, 4);
    assert_eq!(t2.selection_text(), before);
}

#[cfg(feature = "search")]
#[test]
fn t5_f11_search_combining_wide_wrap_unicode_ci_and_eviction() {
    use egui_xterm::SearchQuery;

    let cfg = CoreConfig {
        scrollback_limit: 2,
        ..CoreConfig::default()
    };
    let mut term = Terminal::new(10, 4, cfg);
    term.advance("e\u{0301}中文测试ab\r\nkelvin \u{212A} k\r\n".as_bytes());

    let comb = term
        .search(&SearchQuery::literal("e\u{0301}"))
        .expect("valid literal");
    assert_eq!(comb.len(), 1);
    assert_eq!(
        (comb[0].line, comb[0].start_col, comb[0].end_col),
        (0, 0, 1)
    );

    let wrapped = term
        .search(&SearchQuery::literal("测试ab"))
        .expect("valid literal");
    assert_eq!(wrapped.len(), 1);
    assert_eq!(wrapped[0].line, 0);
    assert_eq!(wrapped[0].end_line, 1);

    let ci = term
        .search(&SearchQuery {
            pattern: "k".to_owned(),
            is_regex: false,
            case_sensitive: false,
        })
        .expect("valid ci literal");
    assert_eq!(ci.len(), 3, "expected 2 ASCII 'k' + Kelvin U+212A: {ci:?}");

    let zero_w = term
        .search(&SearchQuery::regex("^"))
        .expect("valid zero-width regex");
    assert!(zero_w.is_empty());

    // Focus search match, then evict line 0 from scrollback_limit=2.
    assert!(term.search(&SearchQuery::literal("e\u{0301}")).is_ok());
    assert!(term.next_search_match().is_some());
    term.advance(b"1\r\n2\r\n3\r\n4\r\n5\r\n");
    assert!(term.active_search_match().is_none());
    assert!(term.next_search_match().is_none());
}

#[cfg(feature = "hyperlinks")]
#[test]
fn t5_f12_osc8_id_isolation_invalid_scheme_neighbor_and_dec2026_gc() {
    let mut term = Terminal::new(60, 6, CoreConfig::default());
    term.advance(
        b"\x1b]8;id=same;https://one.test\x07ONE\x1b]8;;\x07\x1b]8;id=same;https://two.test\x07TWO\x1b]8;;\x07\r\n",
    );
    let (info1, spans1) = term
        .hyperlink_visible_spans_at(0, 0)
        .expect("first link span");
    assert_eq!(info1.uri, "https://one.test");
    assert_eq!(spans1.len(), 1);
    assert_eq!((spans1[0].start_col, spans1[0].end_col), (0, 3));

    let (info2, spans2) = term
        .hyperlink_visible_spans_at(3, 0)
        .expect("second link span");
    assert_eq!(info2.uri, "https://two.test");
    assert_eq!(spans2.len(), 1);
    assert_eq!((spans2[0].start_col, spans2[0].end_col), (3, 6));

    // Invalid adjacent URL must not swallow valid neighbor URL.
    term.advance(b"https://.invalid https://ok.example/a\r\n");
    assert!(term.hyperlink_at(0, 1).is_none());
    assert_eq!(
        term.hyperlink_at(17, 1).map(|h| h.uri.as_str()),
        Some("https://ok.example/a")
    );

    // Freeze a link inside a DEC 2026 snapshot while churning 400 unique OSC 8 links.
    term.advance(b"\x1b]8;;https://frozen.test\x07FROZEN\x1b]8;;\x07");
    assert_eq!(
        term.hyperlink_at(0, 2).map(|h| h.uri.as_str()),
        Some("https://frozen.test")
    );
    term.advance_with_time(b"\x1b[?2026h\x1b[3;1H", 1.0);
    for i in 0..400 {
        term.advance_with_time(
            format!("\x1b[3;1H\x1b]8;;https://churn.test/{i}\x07L\x1b]8;;\x07").as_bytes(),
            1.01,
        );
    }
    assert_eq!(term.render_snapshot_line_text(2).as_deref(), Some("FROZEN"));
    assert!(term.hyperlink_pool().len() <= 320);
    term.advance_with_time(b"\x1b[?2026l", 1.02);
    assert!(!term.is_synchronized_output());
}

#[cfg(feature = "shell-integration")]
#[test]
fn t5_f13_osc133_dense_1400_zones_compaction_and_malformed() {
    use egui_xterm::TerminalEvent;

    let cfg = CoreConfig {
        scrollback_limit: 50,
        ..CoreConfig::default()
    };
    let mut term = Terminal::new(80, 10, cfg);
    for i in 0..1400 {
        term.advance(
            format!(
                "\x1b]133;A\x07p$ \x1b]133;B\x07cmd{i}\r\n\x1b]133;C\x07out{i}\r\n\x1b]133;D;0\x07"
            )
            .as_bytes(),
        );
    }
    let _ = term.drain_events();

    term.advance(b"\x1b]133\x07\x1b]133;Z\x07\x1b]133;D;999999999999999\x07\x1b]133;D;-42\x07");
    let events = term.drain_events();
    let exit_codes: Vec<Option<i32>> = events
        .into_iter()
        .filter_map(|e| match e {
            TerminalEvent::CommandFinished { exit_code } => Some(exit_code),
            _ => None,
        })
        .collect();
    assert_eq!(exit_codes, vec![None, Some(-42)]);

    term.resize(40, 12);
    term.resize(80, 10);

    let total = term.total_lines();
    let zones = term.shell_zones();
    assert!(zones.len() <= 256, "zones len {} exceeded cap", zones.len());
    for z in zones {
        assert!(z.start_line <= z.end_line && z.end_line < total);
    }
}

#[cfg(feature = "serde")]
#[test]
fn t5_f15_serde_tampered_snapshot_json_sanitization() {
    use egui_xterm::TerminalSnapshot;

    let mut term = Terminal::new(20, 6, CoreConfig::default());
    term.advance(b"hello\r\nworld");
    let snap = term.export_snapshot();
    let mut val = serde_json::to_value(&snap).expect("serialize snapshot");

    val["cols"] = serde_json::json!(0);
    val["rows"] = serde_json::json!(0);
    val["scroll_top"] = serde_json::json!(99);
    val["scroll_bottom"] = serde_json::json!(1);
    val["cursor"]["row"] = serde_json::json!(500);
    val["cursor"]["col"] = serde_json::json!(500);
    val["modes"]["sync_output"] = serde_json::json!(true);

    let tampered: TerminalSnapshot =
        serde_json::from_value(val).expect("deserialize tampered snapshot");
    let mut restored = Terminal::from_snapshot(tampered, CoreConfig::default());
    let (cols, rows) = restored.size();
    assert!(cols >= 1 && rows >= 1);
    let (cx, cy) = restored.cursor_pos();
    assert!(cx < cols && cy < rows);
    assert!(!restored.is_synchronized_output());

    restored.advance(b"\r\nok");
    restored.resize(40, 10);
    assert_eq!(restored.size(), (40, 10));
}

// ---------------------------------------------------------------------------
// egui_xterm Runtime / Renderer / PTY Adversarial Specs (10 tests)
// ---------------------------------------------------------------------------

#[test]
fn t5_c2_01_theme_switch_light_to_dark_and_indexed_ansi_bg() {
    let light_bg = Color32::from_rgb(250, 250, 248);
    let light_red = Color32::from_rgb(207, 34, 46);
    let dark_bg = Color32::from_rgb(20, 22, 28);
    let dark_red = Color32::from_rgb(224, 108, 117);

    let cfg = TerminalConfig {
        theme: Theme::light(),
        ..Default::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.runtime.input_mut().push_str("\x1b[31;41mX\x1b[0m");

    let s1 = h.run_frame().inspect_shapes();
    assert!(s1.has_rect_or_mesh_color(light_bg));
    assert!(s1.has_rect_or_mesh_color(light_red));
    assert!(s1.has_text_fg_color(light_red));

    // Switch Light -> Dark via show_with_config.
    h.config.theme = Theme::dark();
    let s2 = h.run_frame().inspect_shapes();
    assert!(s2.has_rect_or_mesh_color(dark_bg));
    assert!(s2.has_rect_or_mesh_color(dark_red));
    assert!(s2.has_text_fg_color(dark_red));
    assert!(!s2.has_rect_or_mesh_color(light_bg));
    assert!(!s2.has_rect_or_mesh_color(light_red));

    // Reverse switch Dark -> Light must also update SGR 41 indexed bg.
    h.config.theme = Theme::light();
    let s3 = h.run_frame().inspect_shapes();
    assert!(s3.has_rect_or_mesh_color(light_bg));
    assert!(s3.has_rect_or_mesh_color(light_red));
    assert!(!s3.has_rect_or_mesh_color(dark_bg));
    assert!(!s3.has_rect_or_mesh_color(dark_red));
}

#[test]
fn t5_c2_02_osc12_cursor_and_osc4_extended_256_palette_render() {
    let mut h = HeadlessTermHarness::scripted();
    h.focus_terminal();
    h.runtime
        .input_mut()
        .push_str("\x1b]12;#00ff00\x07\x1b]4;196;rgb:00/ff/00\x07\x1b[48;5;196mX\x1b[0m");

    let shapes = h.run_frame().inspect_shapes();
    let green = Color32::from_rgb(0, 255, 0);
    let red_196 = Color32::from_rgb(255, 0, 0);
    assert!(shapes.has_rect_or_mesh_color(green));
    assert!(!shapes.has_rect_or_mesh_color(red_196));

    // Confirm both cell (0,0) and cursor at (1,0) emit green mesh quads (>= 8 green vertices).
    let green_verts = shapes.mesh_colors().iter().filter(|&&c| c == green).count();
    assert!(
        green_verts >= 8,
        "expected >= 8 green vertices (cell 196 + cursor), got {green_verts}"
    );

    // Reset via OSC 112 and OSC 104;196 restores defaults.
    h.runtime
        .input_mut()
        .push_str("\x1b]112\x07\x1b]104;196\x07");
    let after_reset = h.run_frame().inspect_shapes();
    assert!(!after_reset.has_rect_or_mesh_color(green));
    assert!(after_reset.has_rect_or_mesh_color(red_196));
}

#[cfg(all(feature = "vector-glyphs", feature = "hyperlinks"))]
#[test]
fn t5_c2_03_vector_glyph_inside_osc8_and_sgr58_stays_vector() {
    let mut h = HeadlessTermHarness::scripted();
    h.runtime.input_mut().push_str(
        "\x1b]8;;https://example.com\x07\u{E0B0}\x1b]8;;\x07\x1b[4:1;58:2::255:0:0m\u{2588}\x1b[0m",
    );
    let shapes = h.run_frame().inspect_shapes();
    assert!(!shapes.contains_text("\u{E0B0}"));
    assert!(!shapes.contains_text("\u{2588}"));
    assert!(shapes.mesh_shape_count >= 2);
    assert!(shapes.has_rect_or_mesh_color(Color32::from_rgb(255, 0, 0)));
}

#[cfg(not(feature = "vector-glyphs"))]
#[test]
fn t5_c2_03_vector_glyph_disabled_falls_back_to_text() {
    let mut h = HeadlessTermHarness::scripted();
    h.runtime
        .input_mut()
        .push_str("\u{E0B0}\x1b[4:1;58:2::255:0:0m\u{2588}\x1b[0m");
    let shapes = h.run_frame().inspect_shapes();
    assert!(shapes.contains_text("\u{E0B0}"));
    assert!(shapes.contains_text("\u{2588}"));
    assert!(shapes.has_rect_or_mesh_color(Color32::from_rgb(255, 0, 0)));
}

#[test]
fn t5_c2_04_tiny_container_sibling_click_isolation() {
    let ctx = Context::default();
    ctx.enable_accesskit();
    let mut rt = TerminalRuntime::new(ScriptedInput::new());
    let screen = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 200.0));

    let mut sib_rect = Rect::NOTHING;
    let mut sib_clicked = false;
    let mut term_focused = false;

    let run_once = |events: Vec<Event>,
                    rt: &mut TerminalRuntime<ScriptedInput>,
                    sib_rect: &mut Rect,
                    sib_clicked: &mut bool,
                    term_focused: &mut bool| {
        let _ = run_headless_ui(&ctx, screen, 1.0, 0.0, true, events, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.scope_builder(
                    UiBuilder::new()
                        .max_rect(Rect::from_min_size(pos2(0.0, 0.0), vec2(3.0, 100.0))),
                    |ui| {
                        let resp = rt.show(ui);
                        *term_focused = resp.response.has_focus();
                    },
                );
                let btn = ui.button("Sibling");
                *sib_rect = btn.rect;
                *sib_clicked = btn.clicked();
            });
        });
    };

    run_once(
        Vec::new(),
        &mut rt,
        &mut sib_rect,
        &mut sib_clicked,
        &mut term_focused,
    );
    assert!(sib_rect.is_positive());

    let click_pt = pos2(sib_rect.min.x + 1.0, sib_rect.center().y);
    run_once(
        vec![
            Event::PointerMoved(click_pt),
            Event::PointerButton {
                pos: click_pt,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Event::PointerButton {
                pos: click_pt,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
        &mut rt,
        &mut sib_rect,
        &mut sib_clicked,
        &mut term_focused,
    );

    assert!(sib_clicked, "sibling button did not receive click");
    assert!(!term_focused, "tiny terminal stole focus from sibling");
}

#[test]
fn t5_c2_05_slow_path_bidi_and_combining_cluster_grid_alignment() {
    let mut h = HeadlessTermHarness::scripted();
    h.runtime
        .input_mut()
        .push_str("لاABCD\r\n  ABCD\r\n🏳\u{FE0F}\u{200D}🌈ABCD\r\n  ABCD");
    let shapes = h.run_frame().inspect_shapes();

    let mut entries = shapes.text_entries;
    entries.sort_by(|a, b| a.pos.y.total_cmp(&b.pos.y));
    assert_eq!(entries.len(), 4, "expected 4 rendered text rows");

    let extract_abcd_xs = |entry: &egui_xterm_e2e::RenderedTextEntry| -> Vec<f32> {
        entry.galley.rows[0]
            .row
            .glyphs
            .iter()
            .filter(|g| matches!(g.chr, 'A' | 'B' | 'C' | 'D'))
            .map(|g| entry.pos.x + g.pos.x)
            .collect()
    };

    let xs_row0 = extract_abcd_xs(&entries[0]);
    let xs_row1 = extract_abcd_xs(&entries[1]);
    let xs_row2 = extract_abcd_xs(&entries[2]);
    let xs_row3 = extract_abcd_xs(&entries[3]);

    assert_eq!(xs_row0.len(), 4);
    assert_eq!(xs_row1.len(), 4);
    assert_eq!(xs_row2.len(), 4);
    assert_eq!(xs_row3.len(), 4);

    for i in 0..4 {
        assert!(
            (xs_row0[i] - xs_row1[i]).abs() <= 0.5,
            "bidi row0 col {i}: {} vs ref {}",
            xs_row0[i],
            xs_row1[i]
        );
        assert!(
            (xs_row2[i] - xs_row3[i]).abs() <= 0.5,
            "zwj row2 col {i}: {} vs ref {}",
            xs_row2[i],
            xs_row3[i]
        );
        if i > 0 {
            assert!(xs_row0[i] > xs_row0[i - 1] + 1.0);
            assert!(xs_row2[i] > xs_row2[i - 1] + 1.0);
        }
    }
}

#[cfg(feature = "kitty-keyboard")]
#[test]
fn t5_c2_06_kitty_flag2_ctrl_punctuation_fallback() {
    use egui::Key;

    let mut h = HeadlessTermHarness::scripted();
    h.focus_terminal();
    h.runtime.input_mut().push_str("\x1b[>2u");
    let _ = h.run_frame();
    h.runtime.input_mut().clear_written();

    let cases = [
        (Key::Slash, 0x1Fu8),
        (Key::Minus, 0x1Fu8),
        (Key::Num2, 0x00u8),
        (Key::Num3, 0x1Bu8),
        (Key::Num4, 0x1Cu8),
        (Key::Num5, 0x1Du8),
        (Key::Num6, 0x1Eu8),
        (Key::Num7, 0x1Fu8),
    ];

    for (key, expected) in cases {
        h.runtime.input_mut().clear_written();
        h.inject_key(key, true, Modifiers::CTRL);
        let _ = h.run_frame();
        assert_eq!(
            h.runtime.input().written_bytes(),
            &[expected],
            "Ctrl+{key:?} under Kitty Flag 2 failed"
        );
        // Clean up key release without leaking to later cases.
        h.inject_key(key, false, Modifiers::CTRL);
        let _ = h.run_frame();
    }
}

#[test]
fn t5_c2_07_lost_pointer_release_does_not_hover_select() {
    let mut h = HeadlessTermHarness::scripted();
    h.runtime
        .input_mut()
        .push_str("Line0_AAAA\r\nLine1_BBBB\r\n");
    let _ = h.run_frame();

    let p0 = h.cell_center(0, 0).expect("cell (0,0)");
    let p1 = h.cell_center(4, 1).expect("cell (4,1)");

    // Frame 1: Primary button pressed inside grid.
    h.inject_pointer_move(p0);
    h.inject_pointer_button(p0, PointerButton::Primary, true, Modifiers::NONE);
    let _ = h.run_frame();

    // Frame 2: Release happens while terminal is hidden/unmounted (lost release event).
    let screen = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
    let _ = run_headless_ui(
        &h.ctx,
        screen,
        1.0,
        h.sim_time() + 0.016,
        true,
        vec![Event::PointerButton {
            pos: p0,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        |_ui| {},
    );

    // Frame 3: Terminal renders again and receives only PointerMoved (pure hover).
    h.advance_time(0.032);
    h.inject_pointer_move(p1);
    let _ = h.run_frame();

    let sel = h.runtime.terminal().selection_text();
    assert!(
        sel.as_deref().unwrap_or("").is_empty(),
        "hover after missed release mutated selection: {sel:?}"
    );
}

#[test]
fn t5_c2_08_two_byte_read_budget_splits_utf8_and_csi_and_font_reset() {
    let cfg = TerminalConfig {
        read_budget_bytes_per_frame: 2,
        ..Default::default()
    };
    let mut h = HeadlessTermHarness::scripted_with_config(cfg);
    h.runtime
        .input_mut()
        .push_str("\x1b[38;2;12;34;56m中文🚀\x1b[0m");

    let _ = h.run_frames(30, 1.0 / 60.0);
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("中文🚀")
    );
    assert_eq!(
        h.runtime.terminal().cell_at(0, 0).expect("cell (0,0)").fg,
        PackedColor::Rgb(12, 34, 56)
    );

    h.ctx.set_fonts(FontDefinitions::default());
    let _ = h.run_frame();
    let outcome = h.run_frame();
    let shapes = outcome.inspect_shapes();
    assert!(shapes.contains_text("中文🚀"));
    for entry in &shapes.text_entries {
        assert!(!entry.galley.rows.is_empty());
        let row = &entry.galley.rows[0].row;
        for v in &row.visuals.mesh.vertices {
            assert!(v.uv.x.is_finite() && v.uv.y.is_finite());
        }
    }
}

#[cfg(feature = "hyperlinks")]
#[test]
fn t5_c2_09_nonzero_origin_hidpi_autoscale_and_sgr_mouse_alignment() {
    use egui_xterm::ScaleMode;

    let cfg = TerminalConfig {
        scale_mode: ScaleMode::AutoScaleToFit { cols: 40, rows: 12 },
        ..Default::default()
    };
    let ctx = Context::default();
    ctx.enable_accesskit();
    let mut rt = TerminalRuntime::with_config(
        ScriptedInput::from_bytes(
            b"\x1b[?1000h\x1b[?1006h\x1b]8;;https://example.com\x07LINK\x1b]8;;\x07",
        ),
        cfg,
    );

    let screen = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 500.0));
    let pane_rect = Rect::from_min_size(pos2(123.5, 67.5), vec2(500.0, 300.0));
    let mut sim_time = 0.0;

    let mut step = |rt: &mut TerminalRuntime<ScriptedInput>, events: Vec<Event>| {
        sim_time += 1.0 / 60.0;
        let mut resp_out = None;
        let meta = run_headless_ui(&ctx, screen, 1.5, sim_time, true, events, |ui| {
            ui.scope_builder(UiBuilder::new().max_rect(pane_rect), |ui| {
                resp_out = Some(rt.show(ui));
            });
        });
        (resp_out.expect("terminal shown"), meta)
    };

    let (warm_resp, _) = step(&mut rt, Vec::new());
    assert_eq!((warm_resp.cols, warm_resp.rows), (40, 12));
    let term_rect = warm_resp.response.rect;
    let cell_size = warm_resp.cell_size;
    let target = pos2(
        term_rect.min.x + cell_size.x * 2.5,
        term_rect.min.y + cell_size.y * 0.5,
    );

    rt.input_mut().clear_written();
    let _ = step(
        &mut rt,
        vec![
            Event::PointerMoved(target),
            Event::PointerButton {
                pos: target,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Event::PointerButton {
                pos: target,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    let written = rt.input().written_str();
    assert!(
        written.contains("\x1b[<0;3;1M") && written.contains("\x1b[<0;3;1m"),
        "unexpected SGR mouse output: {written:?}"
    );

    // Shift+click bypasses mouse tracking and activates OSC 8 hyperlink.
    let (_, shift_meta) = step(
        &mut rt,
        vec![
            Event::PointerMoved(target),
            Event::PointerButton {
                pos: target,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::SHIFT,
            },
            Event::PointerButton {
                pos: target,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::SHIFT,
            },
        ],
    );
    assert_eq!(shift_meta.opened_urls(), vec!["https://example.com"]);
}

#[cfg(all(feature = "pty", not(target_arch = "wasm32")))]
#[test]
fn t5_c2_10_pty_fast_exit_burst_small_budget_and_extreme_initial_size() {
    use std::time::{Duration, Instant};

    use egui_xterm::pty::{PtyBackend, PtyConfig};
    use egui_xterm::{TerminalInput, TerminalSize};

    #[cfg(windows)]
    let (shell, args) = (
        "cmd".to_owned(),
        vec![
            "/C".to_owned(),
            "for /L %i in (1,1,200) do @echo PTY_LINE_%i".to_owned(),
        ],
    );
    #[cfg(not(windows))]
    let (shell, args) = (
        "sh".to_owned(),
        vec![
            "-c".to_owned(),
            "i=1; while [ $i -le 200 ]; do echo PTY_LINE_$i; i=$((i+1)); done".to_owned(),
        ],
    );

    let backend = PtyBackend::spawn(PtyConfig {
        shell: Some(shell),
        args,
        cwd: None,
        env: Vec::new(),
        initial_size: TerminalSize::new(u16::MAX, u16::MAX),
    })
    .expect("spawn burst PTY");

    let cfg = TerminalConfig {
        read_budget_bytes_per_frame: 128,
        scrollback_limit: 1000,
        ..Default::default()
    };
    let mut h = HeadlessTermHarness::with_config(backend, cfg);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        h.advance_time(1.0 / 60.0);
        let _ = h.run_frame();
        if h.runtime.input().is_closed() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = h.run_frames(2, 1.0 / 60.0);

    assert!(
        h.runtime.input().is_closed(),
        "PTY did not reach clean closed state after exit"
    );
    let text = h.runtime.terminal().full_text();
    for i in 1..=200 {
        let needle = format!("PTY_LINE_{i}");
        assert!(
            text.contains(&needle),
            "missing {needle} in drained PTY output"
        );
    }
}
