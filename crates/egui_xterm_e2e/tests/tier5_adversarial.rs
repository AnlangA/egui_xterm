//! Tier 5 — Adversarial Hardening E2E Tests (31 opaque-box specs).
//!
//! Covers core engine edge cases (encoding, parser overflow, UTF-8 maximal subparts,
//! C1 split chunks, grapheme/1-col reflow, DECSTBM/DECOM/DEC2026, block selection,
//! palette overrides / `set_theme` / `DEC 2026` latching / `serde` compat,
//! search/hyperlinks/shell-integration/serde hardening), M2 protocol/VT hardening
//! (malformed `OSC 9;4`/`66`/`99`, out-of-bounds `DECCRA`/`DECFRA`/`DECSERA`,
//! `?2004` `ESC[201~` paste breakout sanitization), M4 inline graphics hardening
//! (corrupted PNG CRCs, truncated Adam7 PNGs, decompression bombs, Kitty `t=f`/`t=t`/`t=s`
//! sandboxing & path traversal, relative placement `ECYCLE`/`ETOODEEP`, concurrent chunk
//! overflow, out-of-bounds `a=c` compose, oversized Sixel `Ph;Pv` & repeat caps), and
//! `egui_xterm` runtime/renderer edge cases (theme switching, OSC 4/12, vector-glyphs
//! with extras, tiny-container click isolation, slow-path BiDi/ZWJ alignment, Kitty
//! Flag 2 fallback, lost pointer release, 2-byte budget + font reset, HiDPI AutoScaleToFit
//! SGR mouse, and PTY burst).

use egui::{
    Color32, Context, Event, FontDefinitions, Modifiers, PointerButton, Pos2, Rect, UiBuilder,
    pos2, vec2,
};
use egui_xterm::{
    ColorScheme, CoreConfig, PaletteOverrides, SelectionMode, Terminal, TerminalConfig,
    TerminalEvent, TerminalRuntime, Theme,
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
    let c1_stream = b"\x9D2;c1\x9C\x9B1;3H\x90|payload\x9CHi\x85Next";
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
            whole_word: false,
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

    let out = h.run_frame();
    let shapes = out.inspect_shapes();
    let green = Color32::from_rgb(0, 255, 0);
    let red_196 = Color32::from_rgb(255, 0, 0);
    assert!(shapes.has_rect_or_mesh_color(green));
    assert!(!shapes.has_rect_or_mesh_color(red_196));

    // Confirm cell (0,0) emits a green bg mesh quad (>= 4 vertices) and cursor at (1,0) emits a green Shape::Rect.
    let green_verts = shapes
        .non_text_mesh_colors()
        .iter()
        .filter(|&&c| c == green)
        .count();
    assert!(
        green_verts >= 4,
        "expected >= 4 green bg mesh vertices for cell 196, got {green_verts}"
    );
    let origin = out.terminal_response.response.rect.min;
    let cell_size = out.terminal_response.cell_size;
    let cursor_cell = Rect::from_min_size(pos2(origin.x + cell_size.x, origin.y), cell_size);
    assert_eq!(
        shapes.rect_fills_in_rect(cursor_cell, green).len(),
        1,
        "expected green cursor rect at cell (1,0)"
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
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("\u{E0B0}\u{2588}")
    );
    assert!(!shapes.text_mesh_vertices().is_empty());
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
    let out = h.run_frame();
    let origin = out.terminal_response.response.rect.min;
    let term_w = out.terminal_response.response.rect.width();
    let cell_h = out.terminal_response.cell_size.y;
    let shapes = out.inspect_shapes();

    let extract_abcd_xs = |row: usize| -> Vec<f32> {
        let row_rect = Rect::from_min_max(
            pos2(origin.x, origin.y + row as f32 * cell_h + 0.1),
            pos2(
                origin.x + term_w,
                origin.y + (row + 1) as f32 * cell_h - 0.1,
            ),
        );
        let verts = shapes.text_mesh_vertices_in_rect(row_rect);
        assert!(
            verts.len() >= 16,
            "expected at least 4 trailing ASCII quads (16 verts) on row {row}, got {}",
            verts.len()
        );
        let tail = &verts[verts.len() - 16..];
        tail.as_chunks::<4>()
            .0
            .iter()
            .map(|q| q.iter().map(|v| v.pos.x).fold(f32::INFINITY, f32::min))
            .collect()
    };

    let xs_row0 = extract_abcd_xs(0);
    let xs_row1 = extract_abcd_xs(1);
    let xs_row2 = extract_abcd_xs(2);
    let xs_row3 = extract_abcd_xs(3);

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
    assert!(!shapes.text_mesh_vertices().is_empty());
    for v in shapes.text_mesh_vertices() {
        assert!(v.uv.x.is_finite() && v.uv.y.is_finite());
        assert!((0.0..=1.0).contains(&v.uv.x) && (0.0..=1.0).contains(&v.uv.y));
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

// ---------------------------------------------------------------------------
// M2 & M4 Adversarial Security / Robustness Specs (9 tests)
// ---------------------------------------------------------------------------

#[cfg(feature = "kitty-graphics")]
mod m4_adversarial_helpers {
    pub fn encode_base64(input: &[u8]) -> String {
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

    pub fn ieee_crc32(kind: &[u8; 4], data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in kind.iter().chain(data.iter()) {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
            }
        }
        !crc
    }

    pub fn push_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&ieee_crc32(kind, data).to_be_bytes());
    }

    pub fn push_png_chunk_with_len(out: &mut Vec<u8>, len: u32, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&ieee_crc32(kind, data).to_be_bytes());
    }

    pub fn make_ihdr(
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

    pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(0x78);
        out.push(0x01);

        let mut pos = 0;
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

    /// Generates a valid fixed-Huffman RFC 1950 zlib stream repeating `byte` `1 + 258 * repeat_258` times.
    pub fn zlib_fixed_huffman_bomb(byte: u8, repeat_258: usize) -> Vec<u8> {
        struct BitWriter {
            bytes: Vec<u8>,
            cur: u8,
            bits: u8,
        }
        impl BitWriter {
            fn push_bit(&mut self, bit: u8) {
                self.cur |= (bit & 1) << self.bits;
                self.bits += 1;
                if self.bits == 8 {
                    self.bytes.push(self.cur);
                    self.cur = 0;
                    self.bits = 0;
                }
            }
            fn push_lsb(&mut self, val: u32, count: u8) {
                for i in 0..count {
                    self.push_bit(((val >> i) & 1) as u8);
                }
            }
            fn push_msb(&mut self, val: u32, count: u8) {
                for i in (0..count).rev() {
                    self.push_bit(((val >> i) & 1) as u8);
                }
            }
            fn flush_byte(&mut self) {
                if self.bits > 0 {
                    self.bytes.push(self.cur);
                    self.cur = 0;
                    self.bits = 0;
                }
            }
        }

        let mut bw = BitWriter {
            bytes: vec![0x78, 0x9C],
            cur: 0,
            bits: 0,
        };
        // BFINAL = 1, BTYPE = 01 (fixed Huffman).
        bw.push_lsb(1, 1);
        bw.push_lsb(1, 2);
        // Literal byte (0..=143 -> 8-bit fixed Huffman code 0x30 + b).
        bw.push_msb(0x30 + u32::from(byte), 8);
        // Repeated (len=258 [sym 285 -> 0xC5], dist=1 [dist 0 -> 0b00000]).
        for _ in 0..repeat_258 {
            bw.push_msb(0xC5, 8);
            bw.push_msb(0, 5);
        }
        // End-of-block (sym 256 -> 7-bit 0).
        bw.push_msb(0, 7);
        bw.flush_byte();

        // Exact O(1) Adler-32 for constant byte repeated `n` times.
        let n = 1u128 + 258u128 * (repeat_258 as u128);
        let v = u128::from(byte);
        let s1 = (1u128 + n * v) % 65521;
        let s2 = (n + v * n * (n + 1) / 2) % 65521;
        let adler = u32::try_from((s2 << 16) | s1).unwrap_or(0);
        bw.bytes.extend_from_slice(&adler.to_be_bytes());
        bw.bytes
    }

    pub fn build_png_from_chunks(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        for &(kind, data) in chunks {
            push_png_chunk(&mut out, kind, data);
        }
        out
    }

    pub fn encode_adam7_rgba8_scanlines(
        width: usize,
        height: usize,
        pixel_at: impl Fn(usize, usize) -> [u8; 4],
    ) -> Vec<u8> {
        const ADAM7_PASSES: [(usize, usize, usize, usize); 7] = [
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ];

        let mut raw = Vec::new();
        for &(x0, y0, x_step, y_step) in &ADAM7_PASSES {
            if width <= x0 || height <= y0 {
                continue;
            }
            let pw = (width - x0).div_ceil(x_step);
            let ph = (height - y0).div_ceil(y_step);
            for py in 0..ph {
                let dst_y = y0 + py * y_step;
                raw.push(0); // Filter 0 (None)
                for px in 0..pw {
                    let dst_x = x0 + px * x_step;
                    raw.extend_from_slice(&pixel_at(dst_x, dst_y));
                }
            }
        }
        raw
    }

    pub fn kitty_apc(params: &str, raw: &[u8]) -> Vec<u8> {
        let b64 = encode_base64(raw);
        format!("\x1b_G{params};{b64}\x1b\\").into_bytes()
    }
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn t5_m4_png_corrupted_crc_and_malformed_chunks() {
    use m4_adversarial_helpers::*;

    let ihdr = make_ihdr(2, 2, 8, 6, 0);
    let scanlines = [
        0, 10, 20, 30, 255, 40, 50, 60, 255, // row 0
        0, 70, 80, 90, 255, 100, 110, 120, 255, // row 1
    ];
    let idat = zlib_stored(&scanlines);
    let valid_png = build_png_from_chunks(&[
        (b"IHDR", &ihdr),
        (b"tEXt", b"Title\0Test"),
        (b"IDAT", &idat),
        (b"IEND", &[]),
    ]);

    let mut term = Terminal::new(40, 12, CoreConfig::default());

    // 1. Corrupt IHDR CRC byte -> rejected with EINVAL, nothing stored.
    let mut bad_ihdr_crc = valid_png.clone();
    bad_ihdr_crc[29] ^= 0x01;
    term.advance(&kitty_apc("a=T,i=1,f=100", &bad_ihdr_crc));
    let out1 = String::from_utf8(term.drain_outgoing()).expect("utf8");
    assert!(out1.contains("i=1;EINVAL"), "got {out1:?}");
    assert!(term.image_data(1).is_none());
    assert!(term.image_placements().is_empty());

    // 2. Corrupt ancillary tEXt byte without updating CRC -> rejected with EINVAL.
    let mut bad_text_data = valid_png.clone();
    bad_text_data[42] ^= 0x40;
    term.advance(&kitty_apc("a=T,i=2,f=100", &bad_text_data));
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("i=2;EINVAL")
    );
    assert!(term.image_data(2).is_none());

    // 3. Corrupt IDAT byte without updating CRC -> rejected with EINVAL.
    let mut bad_idat = valid_png.clone();
    let mid = bad_idat.len() - 20;
    bad_idat[mid] ^= 0xFF;
    term.advance(&kitty_apc("a=T,i=3,f=100", &bad_idat));
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("i=3;EINVAL")
    );
    assert!(term.image_data(3).is_none());

    // 4. Corrupt IEND CRC byte -> rejected with EINVAL.
    let mut bad_iend_crc = valid_png.clone();
    let last = bad_iend_crc.len() - 1;
    bad_iend_crc[last] ^= 0x80;
    term.advance(&kitty_apc("a=T,i=4,f=100", &bad_iend_crc));
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("i=4;EINVAL")
    );
    assert!(term.image_data(4).is_none());

    // 5. Structural PNG attacks (even with valid CRCs):
    // non-alphabetic chunk type, unknown critical chunk, non-empty IEND, duplicate IHDR,
    // non-consecutive IDAT separated by tEXt, and unknown filter type 5.
    let (idat_a, idat_b) = idat.split_at(idat.len() / 2);
    let bad_filter_idat = zlib_stored(&[
        5, 10, 20, 30, 255, 40, 50, 60, 255, 0, 0, 0, 0, 255, 0, 0, 0, 255,
    ]);
    let structural_attacks: [Vec<u8>; 6] = [
        build_png_from_chunks(&[
            (b"IHDR", &ihdr),
            (b"tE1t", b"bad"),
            (b"IDAT", &idat),
            (b"IEND", &[]),
        ]),
        build_png_from_chunks(&[
            (b"IHDR", &ihdr),
            (b"CRIT", b"bad"),
            (b"IDAT", &idat),
            (b"IEND", &[]),
        ]),
        build_png_from_chunks(&[(b"IHDR", &ihdr), (b"IDAT", &idat), (b"IEND", &[0])]),
        build_png_from_chunks(&[
            (b"IHDR", &ihdr),
            (b"IHDR", &ihdr),
            (b"IDAT", &idat),
            (b"IEND", &[]),
        ]),
        build_png_from_chunks(&[
            (b"IHDR", &ihdr),
            (b"IDAT", idat_a),
            (b"tEXt", b"split\0gap"),
            (b"IDAT", idat_b),
            (b"IEND", &[]),
        ]),
        build_png_from_chunks(&[
            (b"IHDR", &ihdr),
            (b"IDAT", &bad_filter_idat),
            (b"IEND", &[]),
        ]),
    ];

    for (idx, bad_bytes) in structural_attacks.iter().enumerate() {
        let id = u32::try_from(10 + idx).expect("small id");
        term.advance(&kitty_apc(&format!("a=T,i={id},f=100"), bad_bytes));
        let reply = String::from_utf8(term.drain_outgoing()).expect("utf8");
        assert!(reply.contains("EINVAL"), "attack {idx} got {reply:?}");
        assert!(term.image_data(id).is_none());
    }
    assert!(term.image_placements().is_empty());

    // 6. Valid PNG still decodes and renders in HeadlessTermHarness after all rejections.
    let mut h = HeadlessTermHarness::scripted();
    h.runtime
        .input_mut()
        .push_bytes(kitty_apc("a=T,i=99,f=100", &valid_png));
    let outcome = h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b_Gi=99;OK\x1b\\");
    assert_eq!(
        h.runtime.terminal().image_data(99).expect("stored").rgba,
        vec![
            10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255
        ]
    );
    assert!(!outcome.texture_deltas.set_entries.is_empty());
    assert!(outcome.inspect_shapes().mesh_shape_count >= 1);
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn t5_m4_png_truncated_adam7_and_filter_corruption() {
    use m4_adversarial_helpers::*;

    let pixel_at = |x: usize, y: usize| -> [u8; 4] {
        [
            u8::try_from((x * 29 + 7) & 0xFF).unwrap_or(0),
            u8::try_from((y * 31 + 13) & 0xFF).unwrap_or(0),
            u8::try_from(((x + y) * 17) & 0xFF).unwrap_or(0),
            255,
        ]
    };
    let ihdr_adam7 = make_ihdr(9, 9, 8, 6, 1);
    let full_raw = encode_adam7_rgba8_scanlines(9, 9, pixel_at);
    let full_idat = zlib_stored(&full_raw);
    let valid_adam7 = build_png_from_chunks(&[
        (b"IHDR", &ihdr_adam7),
        (b"IDAT", &full_idat),
        (b"IEND", &[]),
    ]);

    let mut term = Terminal::new(40, 12, CoreConfig::default());

    // 1. Truncated raw PNG container boundaries (mid-IHDR, mid-IDAT, missing IEND, truncated IEND CRC).
    for cut in [
        12usize,
        28,
        valid_adam7.len() / 2,
        valid_adam7.len() - 12,
        valid_adam7.len() - 1,
    ] {
        term.advance(&kitty_apc("a=T,i=1,f=100", &valid_adam7[..cut]));
        let reply = String::from_utf8(term.drain_outgoing()).expect("utf8");
        assert!(reply.contains("EINVAL"), "cut={cut} got {reply:?}");
        assert!(term.image_data(1).is_none());
    }

    // 2. Valid PNG container + valid CRCs, but decompressed Adam7 stream is truncated
    // (missing final pass bytes), oversized by 1 byte, or has invalid filter byte in pass 7.
    let truncated_stream = build_png_from_chunks(&[
        (b"IHDR", &ihdr_adam7),
        (b"IDAT", &zlib_stored(&full_raw[..full_raw.len() - 1])),
        (b"IEND", &[]),
    ]);
    let mut extended_raw = full_raw.clone();
    extended_raw.push(0);
    let oversized_stream = build_png_from_chunks(&[
        (b"IHDR", &ihdr_adam7),
        (b"IDAT", &zlib_stored(&extended_raw)),
        (b"IEND", &[]),
    ]);
    let mut bad_pass7_raw = full_raw;
    let pass7_last_row_filter_offset = bad_pass7_raw.len() - (1 + 9 * 4);
    bad_pass7_raw[pass7_last_row_filter_offset] = 9;
    let bad_pass7_filter = build_png_from_chunks(&[
        (b"IHDR", &ihdr_adam7),
        (b"IDAT", &zlib_stored(&bad_pass7_raw)),
        (b"IEND", &[]),
    ]);

    for (idx, bad_png) in [truncated_stream, oversized_stream, bad_pass7_filter]
        .iter()
        .enumerate()
    {
        let id = u32::try_from(10 + idx).expect("small id");
        term.advance(&kitty_apc(&format!("a=T,i={id},f=100"), bad_png));
        let reply = String::from_utf8(term.drain_outgoing()).expect("utf8");
        assert!(reply.contains("EINVAL"), "case {idx} got {reply:?}");
        assert!(term.image_data(id).is_none());
    }
    assert!(term.image_placements().is_empty());

    // 3. Complete valid 9x9 Adam7 PNG succeeds and renders exact pixels.
    let mut h = HeadlessTermHarness::scripted();
    h.runtime
        .input_mut()
        .push_bytes(kitty_apc("a=T,i=77,f=100", &valid_adam7));
    let outcome = h.run_frame();
    assert_eq!(h.runtime.input().written_str(), "\x1b_Gi=77;OK\x1b\\");
    let stored = h.runtime.terminal().image_data(77).expect("adam7 stored");
    assert_eq!((stored.width, stored.height), (9, 9));
    for y in 0..9 {
        for x in 0..9 {
            let off = (y * 9 + x) * 4;
            assert_eq!(&stored.rgba[off..off + 4], &pixel_at(x, y));
        }
    }
    assert!(!outcome.texture_deltas.set_entries.is_empty());
    assert!(outcome.inspect_shapes().mesh_shape_count >= 1);
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn t5_m4_png_and_kitty_decompression_bomb_caps() {
    use m4_adversarial_helpers::*;

    let mut term = Terminal::new(40, 12, CoreConfig::default());

    // 1. IHDR integer overflow / dimension cap (> 8192 or > 64 MiB RGBA budget) rejected immediately.
    let tiny_idat = zlib_stored(&[0, 1, 2, 3, 4]);
    let dim_bombs = [
        make_ihdr(u32::MAX, u32::MAX, 8, 6, 0),
        make_ihdr(8193, 1, 8, 6, 0),
        make_ihdr(4097, 4097, 8, 6, 0), // 4097*4097*4 = 67.14 MiB > 64 MiB budget
        make_ihdr(0, 10, 8, 6, 0),
    ];
    for (idx, ihdr) in dim_bombs.iter().enumerate() {
        let png = build_png_from_chunks(&[(b"IHDR", ihdr), (b"IDAT", &tiny_idat), (b"IEND", &[])]);
        let id = u32::try_from(1 + idx).expect("small id");
        term.advance(&kitty_apc(&format!("a=T,i={id},f=100"), &png));
        let reply = String::from_utf8(term.drain_outgoing()).expect("utf8");
        assert!(reply.contains("EINVAL"), "dim bomb {idx} got {reply:?}");
        assert!(term.image_data(id).is_none());
    }

    // 2. Malformed PNG chunk length header claiming u32::MAX / 2 GiB without matching bytes.
    let ihdr_1x1 = make_ihdr(1, 1, 8, 6, 0);
    for claimed_len in [u32::MAX, 0x7FFF_FFFF] {
        let mut crafted = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        push_png_chunk(&mut crafted, b"IHDR", &ihdr_1x1);
        push_png_chunk_with_len(&mut crafted, claimed_len, b"IDAT", &[0u8; 8]);
        term.advance(&kitty_apc("a=T,i=10,f=100", &crafted));
        assert!(
            String::from_utf8(term.drain_outgoing())
                .expect("utf8")
                .contains("EINVAL")
        );
        assert!(term.image_data(10).is_none());
    }

    // 3. PNG IDAT DEFLATE decompression bomb: IHDR claims 1x1 (expected_raw = 5 bytes),
    // but IDAT contains a small fixed-Huffman stream expanding to > 50 KiB.
    let small_bomb = zlib_fixed_huffman_bomb(0x41, 200);
    assert!(small_bomb.len() < 1024);
    let png_bomb =
        build_png_from_chunks(&[(b"IHDR", &ihdr_1x1), (b"IDAT", &small_bomb), (b"IEND", &[])]);
    term.advance(&kitty_apc("a=T,i=20,f=100", &png_bomb));
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("EINVAL")
    );
    assert!(term.image_data(20).is_none());

    // 4. Kitty-level `o=z` DEFLATE bomb expanding past 64 MiB DEFAULT_IMAGE_MEMORY_BUDGET
    // from a compact ~431 KiB fixed-Huffman payload.
    let huge_bomb = zlib_fixed_huffman_bomb(0x41, 265_000);
    assert!(huge_bomb.len() < 1024 * 1024);
    term.advance(&kitty_apc("a=T,i=21,f=32,o=z,s=1,v=1", &huge_bomb));
    let bomb_reply = String::from_utf8(term.drain_outgoing()).expect("utf8");
    assert!(bomb_reply.contains("i=21;EINVAL"), "got {bomb_reply:?}");
    assert!(term.image_data(21).is_none());

    // 5. Raw RGBA budget overflow (`s=4097,v=4097` fails ENOMEM / truncated payload) + valid recovery.
    term.advance(&kitty_apc("a=T,i=22,f=32,s=4097,v=4097", &[1, 2, 3, 4]));
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("EINVAL")
    );
    assert!(term.image_placements().is_empty());

    term.advance(&kitty_apc("a=T,i=23,f=32,s=1,v=1", &[9, 8, 7, 255]));
    assert_eq!(
        String::from_utf8(term.drain_outgoing()).expect("utf8"),
        "\x1b_Gi=23;OK\x1b\\"
    );
    assert_eq!(
        term.image_data(23).expect("recovered").rgba,
        &[9, 8, 7, 255]
    );
}

#[cfg(all(feature = "kitty-graphics", not(target_arch = "wasm32")))]
#[test]
fn t5_m4_kitty_file_and_shm_sandbox_policy_enforcement() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use egui_xterm_core::{KittyFileMediumPolicy, KittyShmReader};
    use m4_adversarial_helpers::*;

    struct TempCleanup {
        paths: Vec<std::path::PathBuf>,
        dirs: Vec<std::path::PathBuf>,
    }
    impl Drop for TempCleanup {
        fn drop(&mut self) {
            for p in &self.paths {
                drop(std::fs::remove_file(p));
            }
            for d in &self.dirs {
                drop(std::fs::remove_dir_all(d));
            }
        }
    }

    struct SpyShmReader {
        calls: Arc<AtomicUsize>,
    }
    impl KittyShmReader for SpyShmReader {
        fn read_and_unlink(
            &self,
            name: &str,
            offset: u64,
            size: u64,
            _max_bytes: usize,
        ) -> Result<Vec<u8>, &'static str> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if name == "/kitty-valid-shm" && offset == 0 && size == 4 {
                Ok(vec![11, 22, 33, 255])
            } else {
                Err("EBADF:unexpected shm call")
            }
        }
    }

    let pid = std::process::id();
    let temp_root = std::env::temp_dir();
    let marker_dir = temp_root.join(format!("tty-graphics-protocol-e2e-{pid}"));
    std::fs::create_dir_all(&marker_dir).expect("create marker dir");

    let safe_temp_file = temp_root.join(format!("tty-graphics-protocol-t5-{pid}.rgba"));
    let non_marker_file = temp_root.join(format!("egui-xterm-t5-secret-{pid}.rgba"));
    let inside_marker_file = marker_dir.join("valid.rgba");
    std::fs::write(&safe_temp_file, [1, 2, 3, 255]).expect("write temp file");
    std::fs::write(&non_marker_file, [4, 5, 6, 255]).expect("write non-marker file");
    std::fs::write(&inside_marker_file, [7, 8, 9, 255]).expect("write marker dir file");

    #[cfg(unix)]
    let symlink_path = temp_root.join(format!("tty-graphics-protocol-link-{pid}.rgba"));
    let _cleanup = TempCleanup {
        paths: vec![
            safe_temp_file.clone(),
            non_marker_file.clone(),
            #[cfg(unix)]
            symlink_path.clone(),
        ],
        dirs: vec![marker_dir.clone()],
    };

    #[cfg(unix)]
    {
        drop(std::fs::remove_file(&symlink_path));
        std::os::unix::fs::symlink(&non_marker_file, &symlink_path).expect("create symlink");
    }

    let shm_calls = Arc::new(AtomicUsize::new(0));
    let mut term = Terminal::new(
        40,
        12,
        CoreConfig {
            kitty_file_medium_policy: KittyFileMediumPolicy::Disabled,
            ..CoreConfig::default()
        },
    );
    term.set_kitty_shm_reader(Some(Box::new(SpyShmReader {
        calls: Arc::clone(&shm_calls),
    })));

    // 1. Disabled policy rejects t=f, t=t (even with valid marker path), and t=s without calling SHM
    //    or unlinking the file; feature queries reflect disabled state.
    term.advance(
        b"\x1b_Ga=q,i=90,e=t,t=f\x1b\\\x1b_Ga=q,i=91,e=t,t=t\x1b\\\x1b_Ga=q,i=92,e=t,t=s\x1b\\",
    );
    let fq_disabled = String::from_utf8(term.drain_outgoing()).expect("utf8");
    assert_eq!(
        fq_disabled,
        "\x1b_Gi=90,e=t;OK\x1b\\\x1b_Gi=91,e=t;OK\x1b\\\x1b_Gi=92,e=t;OK\x1b\\"
    );

    term.advance(&kitty_apc(
        "a=T,i=1,t=f,f=32,s=1,v=1",
        non_marker_file.to_string_lossy().as_bytes(),
    ));
    term.advance(&kitty_apc(
        "a=T,i=2,t=t,f=32,s=1,v=1",
        safe_temp_file.to_string_lossy().as_bytes(),
    ));
    term.advance(&kitty_apc(
        "a=T,i=3,t=s,f=32,s=1,v=1,O=0,S=4",
        b"/kitty-valid-shm",
    ));
    let disabled_out = String::from_utf8(term.drain_outgoing()).expect("utf8");
    assert!(disabled_out.contains("i=1;EBADF"));
    assert!(disabled_out.contains("i=2;EBADF"));
    assert!(disabled_out.contains("i=3;EBADF"));
    assert!(safe_temp_file.exists(), "Disabled must not unlink file");
    assert_eq!(shm_calls.load(Ordering::SeqCst), 0);

    // 2. TempOnly policy rejects t=f, t=s, non-marker t=t, path traversal escaping marker dir,
    //    symlinks to non-marker targets, directory paths, NUL bytes, and /proc|/sys|/dev prefixes.
    term.set_kitty_file_medium_policy(KittyFileMediumPolicy::TempOnly);

    let traversal_path = format!(
        "{}/../{}",
        marker_dir.display(),
        non_marker_file
            .file_name()
            .expect("filename")
            .to_string_lossy()
    );
    let mut nul_path = safe_temp_file.to_string_lossy().as_bytes().to_vec();
    nul_path.push(0);

    let temp_only_rejects: Vec<(&str, Vec<u8>)> = vec![
        ("t=f", non_marker_file.to_string_lossy().as_bytes().to_vec()),
        ("t=t", non_marker_file.to_string_lossy().as_bytes().to_vec()),
        ("t=t", traversal_path.into_bytes()),
        ("t=t", marker_dir.to_string_lossy().as_bytes().to_vec()),
        ("t=t", nul_path),
        ("t=t", Vec::new()),
        ("t=t", b"/proc".to_vec()),
        ("t=t", b"/proc/self/environ".to_vec()),
        ("t=t", b"/sys/kernel/osrelease".to_vec()),
        ("t=t", b"/dev/zero".to_vec()),
        ("t=t", b"/dev/./zero".to_vec()),
        ("t=s", b"/kitty-valid-shm".to_vec()),
    ];
    for (idx, (medium, path_bytes)) in temp_only_rejects.iter().enumerate() {
        let id = u32::try_from(10 + idx).expect("id");
        term.advance(&kitty_apc(
            &format!("a=T,i={id},{medium},f=32,s=1,v=1,O=0,S=4"),
            path_bytes,
        ));
        let reply = String::from_utf8(term.drain_outgoing()).expect("utf8");
        assert!(
            reply.contains("EBADF"),
            "case {idx} ({medium}) got {reply:?}"
        );
        assert!(term.image_data(id).is_none());
    }

    #[cfg(unix)]
    {
        term.advance(&kitty_apc(
            "a=T,i=29,t=t,f=32,s=1,v=1",
            symlink_path.to_string_lossy().as_bytes(),
        ));
        let reply = String::from_utf8(term.drain_outgoing()).expect("utf8");
        assert!(reply.contains("EBADF"), "symlink attack got {reply:?}");
        assert!(
            non_marker_file.exists(),
            "symlink target must remain intact"
        );
    }

    assert!(
        non_marker_file.exists(),
        "TempOnly must not unlink rejected files"
    );
    assert_eq!(shm_calls.load(Ordering::SeqCst), 0);

    // Valid t=t inside TempOnly succeeds and unlinks the temp file.
    term.advance(&kitty_apc(
        "a=T,i=30,t=t,f=32,s=1,v=1",
        safe_temp_file.to_string_lossy().as_bytes(),
    ));
    assert_eq!(
        String::from_utf8(term.drain_outgoing()).expect("utf8"),
        "\x1b_Gi=30;OK\x1b\\"
    );
    assert_eq!(
        term.image_data(30).expect("temp file").rgba,
        &[1, 2, 3, 255]
    );
    assert!(!safe_temp_file.exists());

    // 3. LocalFiles still blocks /proc|/sys|/dev, out-of-bounds O/S slicing, and t=s traversal,
    //    while allowing legitimate t=f and t=s via HeadlessTermHarness.
    let mut h = HeadlessTermHarness::scripted();
    h.runtime
        .terminal_mut()
        .set_kitty_file_medium_policy(KittyFileMediumPolicy::LocalFiles);
    h.runtime
        .terminal_mut()
        .set_kitty_shm_reader(Some(Box::new(SpyShmReader {
            calls: Arc::clone(&shm_calls),
        })));

    let long_shm = vec![b'a'; 256];
    let non_marker_str = non_marker_file.to_string_lossy().into_owned();
    let local_rejects: Vec<(&str, &[u8])> = vec![
        ("t=f,O=0,S=4", b"/proc/self/cmdline"),
        ("t=f,O=0,S=4", b"/dev/./zero"),
        ("t=f,O=99,S=4", non_marker_str.as_bytes()),
        ("t=f,O=0,S=16", non_marker_str.as_bytes()),
        ("t=s,O=0,S=4", b"/../etc/passwd"),
        ("t=s,O=0,S=4", b"foo/../bar"),
        ("t=s,O=0,S=4", b""),
        ("t=s,O=0,S=4", b"/bad\0shm"),
        ("t=s,O=0,S=4", &long_shm),
        ("t=s,O=0,S=9999999999", b"/kitty-valid-shm"),
    ];
    for (idx, (params, payload)) in local_rejects.iter().enumerate() {
        let id = u32::try_from(40 + idx).expect("id");
        h.runtime.input_mut().push_bytes(kitty_apc(
            &format!("a=T,i={id},{params},f=32,s=1,v=1"),
            payload,
        ));
    }
    let _ = h.run_frame();
    let rejected_replies = h.runtime.input().written_str();
    for idx in 0..local_rejects.len() {
        let id = 40 + idx;
        assert!(
            rejected_replies.contains(&format!("i={id};EBADF")),
            "expected EBADF for i={id} in {rejected_replies:?}"
        );
    }
    assert_eq!(
        shm_calls.load(Ordering::SeqCst),
        0,
        "SHM callback must never run on rejected t=s names/bounds"
    );

    h.runtime.input_mut().clear_written();
    h.runtime.input_mut().push_bytes(kitty_apc(
        "a=T,i=60,t=f,f=32,s=1,v=1",
        non_marker_file.to_string_lossy().as_bytes(),
    ));
    h.runtime.input_mut().push_bytes(kitty_apc(
        "a=T,i=61,t=s,f=32,s=1,v=1,O=0,S=4",
        b"/kitty-valid-shm",
    ));
    let ok_frame = h.run_frame();
    assert_eq!(
        h.runtime.input().written_str(),
        "\x1b_Gi=60;OK\x1b\\\x1b_Gi=61;OK\x1b\\"
    );
    assert_eq!(shm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        h.runtime.terminal().image_data(60).expect("t=f").rgba,
        &[4, 5, 6, 255]
    );
    assert_eq!(
        h.runtime.terminal().image_data(61).expect("t=s").rgba,
        &[11, 22, 33, 255]
    );
    assert!(ok_frame.inspect_shapes().mesh_shape_count >= 2);
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn t5_m4_kitty_relative_cycles_depth_chunks_and_compose_oob() {
    use m4_adversarial_helpers::*;

    let mut term = Terminal::new(80, 24, CoreConfig::default());

    // Base 2x2 image (id=1).
    let px_2x2 = [
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
    ];
    term.advance(&kitty_apc("a=T,i=1,p=1,f=32,s=2,v=2", &px_2x2));
    assert_eq!(
        String::from_utf8(term.drain_outgoing()).expect("utf8"),
        "\x1b_Gi=1,p=1;OK\x1b\\"
    );

    // 1. Virtual + relative (EINVAL) & missing parent (ENOPARENT).
    term.advance(&kitty_apc(
        "a=T,i=2,p=1,U=1,P=1,f=32,s=1,v=1",
        &[1, 2, 3, 255],
    ));
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("EINVAL")
    );
    term.advance(b"\x1b_Ga=p,i=1,p=90,P=999,Q=1\x1b\\");
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("ENOPARENT")
    );

    // 2. Relative placement depth cap (`MAX_RELATIVE_PLACEMENT_DEPTH = 32` -> ETOODEEP at 33rd edge)
    //    and cycle detection (`ECYCLE` on self-loop and 3-node loop).
    for depth in 2..=33u32 {
        let parent = depth - 1;
        term.advance(
            format!("\x1b_Ga=p,i=1,p={depth},P=1,Q={parent},H=1,V=0,c=1,r=1\x1b\\").as_bytes(),
        );
        let reply = String::from_utf8(term.drain_outgoing()).expect("utf8");
        assert_eq!(reply, format!("\x1b_Gi=1,p={depth};OK\x1b\\"));
    }

    // Adding placement 34 parented to 33 exceeds MAX_RELATIVE_PLACEMENT_DEPTH=32 -> ETOODEEP.
    term.advance(b"\x1b_Ga=p,i=1,p=34,P=1,Q=33,c=1,r=1\x1b\\");
    let deep_err = String::from_utf8(term.drain_outgoing()).expect("utf8");
    assert!(deep_err.contains("ETOODEEP"), "got {deep_err:?}");
    assert!(!term.image_placements().iter().any(|p| p.placement_id == 34));

    // a=T exceeding relative depth also rejects before inserting the new image.
    term.advance(&kitty_apc(
        "a=T,i=55,p=1,P=1,Q=33,f=32,s=1,v=1",
        &[9, 9, 9, 255],
    ));
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("ETOODEEP")
    );
    assert!(term.image_data(55).is_none());

    // Self-cycle and multi-node cycle (`p=1` updated to point at `p=3`) return ECYCLE without
    // corrupting existing placements.
    term.advance(b"\x1b_Ga=p,i=1,p=2,P=1,Q=2\x1b\\");
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("ECYCLE")
    );
    term.advance(b"\x1b_Ga=p,i=1,p=1,P=1,Q=3\x1b\\");
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("ECYCLE")
    );
    assert_eq!(term.image_placements().len(), 33);

    // Deleting root p=1 cascade-deletes all 32 relative descendants cleanly.
    term.advance(b"\x1b_Ga=d,d=i,i=1,p=1\x1b\\");
    assert!(term.image_placements().is_empty());

    // 3. Concurrent chunk limit overflow: 16 active `m=1` transfers allowed, 17th fails `ENOMEM`.
    let px_b64 = encode_base64(&[42, 43, 44, 255]);
    let (c1, c2) = px_b64.split_at(4);
    for id in 101..=116u32 {
        term.advance(format!("\x1b_Ga=t,i={id},f=32,s=1,v=1,m=1;{c1}\x1b\\").as_bytes());
    }
    assert!(term.drain_outgoing().is_empty());
    term.advance(format!("\x1b_Ga=t,i=117,f=32,s=1,v=1,m=1;{c1}\x1b\\").as_bytes());
    let overflow_reply = String::from_utf8(term.drain_outgoing()).expect("utf8");
    assert!(
        overflow_reply.contains("i=117;ENOMEM"),
        "got {overflow_reply:?}"
    );

    // Completing i=101 frees a slot so i=117 can proceed; bad base64 in final chunk fails cleanly.
    term.advance(format!("\x1b_Gi=101,m=0;{c2}\x1b\\").as_bytes());
    assert_eq!(
        String::from_utf8(term.drain_outgoing()).expect("utf8"),
        "\x1b_Gi=101;OK\x1b\\"
    );
    assert!(term.image_data(101).is_some());

    term.advance(format!("\x1b_Ga=t,i=117,f=32,s=1,v=1,m=1;{c1}\x1b\\").as_bytes());
    term.advance(b"\x1b_Gi=117,m=0;!!!!\x1b\\");
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("i=117;EINVAL")
    );
    assert!(term.image_data(117).is_none());

    // Clean up remaining pending transfers 102..=116 via `d=i`, then verify 32 MiB pending byte cap.
    for id in 102..=116u32 {
        term.advance(format!("\x1b_Ga=d,d=i,i={id}\x1b\\").as_bytes());
    }
    let chunk_8m = "A".repeat(8 * 1024 * 1024);
    for id in 201..=204u32 {
        term.advance(format!("\x1b_Ga=t,i={id},f=32,s=1,v=1,m=1;{chunk_8m}\x1b\\").as_bytes());
    }
    assert!(term.drain_outgoing().is_empty());
    // 5th transfer pushes pending_kitty_bytes past 32 MiB -> ENOMEM.
    term.advance(b"\x1b_Ga=t,i=205,f=32,s=1,v=1,m=1;AAAA\x1b\\");
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("i=205;ENOMEM")
    );
    // Continuation chunk on i=204 pushing past 32 MiB evicts i=204 with ENOMEM.
    term.advance(b"\x1b_Gi=204,m=1;AAAA\x1b\\");
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("i=204;ENOMEM")
    );
    for id in 201..=203u32 {
        term.advance(format!("\x1b_Ga=d,d=i,i={id}\x1b\\").as_bytes());
    }

    // 4. `a=c` out-of-bounds compose, u32::MAX wraparound, missing frame/image, overlapping
    //    self-compose, and `a=f` out-of-bounds frame edit.
    term.advance(b"\x1b_Ga=f,i=1,z=50;\x1b\\");
    assert_eq!(
        String::from_utf8(term.drain_outgoing()).expect("utf8"),
        "\x1b_Gi=1;OK\x1b\\"
    );

    let oob_compose_cmds = [
        "a=c,i=999,r=1,c=2",
        "a=c,i=1,r=0,c=2",
        "a=c,i=1,r=1,c=9",
        "a=c,i=1,r=1,c=2,X=2,Y=0,x=0,y=0,w=1,h=1",
        "a=c,i=1,r=1,c=2,X=0,Y=2,x=0,y=0,w=1,h=1",
        "a=c,i=1,r=1,c=2,X=0,Y=0,x=2,y=0,w=1,h=1",
        "a=c,i=1,r=1,c=2,X=0,Y=0,x=0,y=2,w=1,h=1",
        "a=c,i=1,r=1,c=2,X=1,Y=0,x=0,y=0,w=2,h=1",
        "a=c,i=1,r=1,c=2,X=0,Y=0,x=1,y=1,w=2,h=2",
        "a=c,i=1,r=1,c=2,X=1,Y=0,x=0,y=0,w=4294967295,h=1",
        "a=c,i=1,r=1,c=1,X=0,Y=0,x=0,y=0,w=2,h=2",
    ];
    for cmd in oob_compose_cmds {
        term.advance(format!("\x1b_G{cmd}\x1b\\").as_bytes());
        let reply = String::from_utf8(term.drain_outgoing()).expect("utf8");
        assert!(
            reply.contains("EINVAL") || reply.contains("ENOENT"),
            "cmd {cmd} got {reply:?}"
        );
    }

    // `a=f` frame edit out-of-bounds (`x + in_w > img_w` or `y + in_h > img_h`).
    term.advance(&kitty_apc("a=f,i=1,r=2,x=2,y=0,s=1,v=1", &[1, 2, 3, 255]));
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("EINVAL")
    );
    term.advance(&kitty_apc("a=f,i=1,r=2,x=0,y=2,s=1,v=1", &[1, 2, 3, 255]));
    assert!(
        String::from_utf8(term.drain_outgoing())
            .expect("utf8")
            .contains("EINVAL")
    );
    // Original frame 1 pixels remain untouched.
    assert_eq!(term.image_data(1).expect("base img").rgba, px_2x2);
}

#[cfg(feature = "sixel")]
#[test]
fn t5_m4_sixel_oversized_raster_repeat_and_palette_caps() {
    // 1. Core `Terminal` caps `!999999` / `!999999999999` repeat width at MAX_SIXEL_DIM (8192)
    //    and rejects > MAX_SIXEL_PIXELS / > MAX_SIXEL_DIM rasters without OOM or panic.
    let mut term = Terminal::new(80, 24, CoreConfig::default());
    term.advance(b"\x1bPq#1!999999~#2!999999999999@\x1b\\");
    let capped_placements = term.image_placements().to_vec();
    assert_eq!(capped_placements.len(), 1);
    let capped_img = term
        .image_data(capped_placements[0].image_id)
        .expect("capped sixel stored");
    assert_eq!(capped_img.width, 8192);
    assert_eq!(capped_img.height, 6);

    // 2. Oversized Ph;Pv (> MAX_SIXEL_DIM=8192, > MAX_SIXEL_PIXELS=16Mi, u32::MAX overflow)
    //    must reject cleanly without OOM or panic inside HeadlessTermHarness.
    let mut h = HeadlessTermHarness::scripted();
    h.runtime.input_mut().push_str(concat!(
        "\x1bPq\"1;1;999999;999999#1~\x1b\\",
        "\x1bPq\"1;1;8192;8192#1~\x1b\\",
        "\x1bPq\"1;1;4294967295;4294967295#1~\x1b\\",
        "\x1bPq\"1;1;100;200000#1~\x1b\\",
    ));
    let _ = h.run_frame();
    assert!(h.runtime.terminal().image_placements().is_empty());

    // `!999999` after 350 `-` newlines (8192 * 2106 > 16 Mi pixels -> overflow aborted cleanly).
    let mut tall_repeat = String::from("\x1bPq");
    for _ in 0..350 {
        tall_repeat.push('-');
    }
    tall_repeat.push_str("#1!999999~\x1b\\");
    h.runtime.input_mut().push_str(tall_repeat);
    let _ = h.run_frame();
    assert!(h.runtime.terminal().image_placements().is_empty());

    // 3. Huge color register index and huge Pan;Pad aspect ratio clamp safely without overflow
    //    and render cleanly in HeadlessTermHarness.
    h.runtime
        .input_mut()
        .push_str("\x1bPq\"999999;999999;4;6#999999;2;100;0;0#999999~#1!2@\x1b\\");
    let ok_frame = h.run_frame();
    assert_eq!(h.runtime.terminal().image_placements().len(), 1);
    assert!(!ok_frame.texture_deltas.set_entries.is_empty());
    assert!(ok_frame.inspect_shapes().mesh_shape_count >= 1);
}

#[test]
fn t5_m2_malformed_osc_9_4_66_99_and_progress_widget() {
    use egui_xterm_core::TextSizing;

    let mut h = HeadlessTermHarness::scripted();
    assert!(h.config.show_progress_bar);
    let _ = h.run_frame();

    // 1. Malformed OSC 9;4 sequences must not emit Progress events or panic progress widget
    //    (bare `OSC 9;4` without state param falls back to an OSC 9 notification `"4"`).
    h.runtime.input_mut().push_str(concat!(
        "\x1b]9;4\x07",
        "\x1b]9;4;\x07",
        "\x1b]9;4;5;50\x07",
        "\x1b]9;4;255;50\x07",
        "\x1b]9;4;-1;50\x07",
        "\x1b]9;4;bad;50\x07",
        "\x1b]9;4;1;999999999999\x07",
        "\x1b]9;4;2;-5\x07",
        "\x1b]9;4;4;250\x07",
    ));
    let p_frame = h.run_frame();
    assert_eq!(
        p_frame.terminal_response.events,
        vec![
            TerminalEvent::Notification {
                title: String::new(),
                body: "4".to_owned(),
            },
            TerminalEvent::Progress {
                state: 1,
                progress: 0,
            },
            TerminalEvent::Progress {
                state: 2,
                progress: 0,
            },
            TerminalEvent::Progress {
                state: 4,
                progress: 100,
            },
        ]
    );
    // Reset progress bar to hidden (`state=0`).
    h.runtime.input_mut().push_str("\x1b]9;4;0\x07");
    let _ = h.run_frame();

    // 2. Malformed OSC 66 sequences (missing args, invalid UTF-8, >4096 text, embedded control chars,
    //    out-of-range metadata, missing `=`) must be rejected cleanly; valid clamping & bottom-right
    //    corner box placement must not panic during grid update or egui rendering.
    let huge_66 = format!("\x1b]66;s=2;{}\x07", "A".repeat(4097));
    let mut bad_utf8_66 = b"\x1b]66;s=2;\xff\xfe\x07".to_vec();
    bad_utf8_66.extend_from_slice(huge_66.as_bytes());
    bad_utf8_66.extend_from_slice(
        concat!(
            "\x1b]66\x07",
            "\x1b]66;s=2\x07",
            "\x1b]66;s=2;\x07",
            "\x1b]66;s=8;BAD\x07",
            "\x1b]66;s=256;BAD\x07",
            "\x1b]66;w=8;BAD\x07",
            "\x1b]66;n=16;BAD\x07",
            "\x1b]66;d=16;BAD\x07",
            "\x1b]66;badpair;BAD\x07",
            "\x1b]66;s=-1;BAD\x07",
        )
        .as_bytes(),
    );
    bad_utf8_66.extend_from_slice("\x1b]66;s=2;A\u{0085}B\x07".as_bytes());
    bad_utf8_66.extend_from_slice("\x1b]66;s=2;A\u{009B}B\x07".as_bytes());
    h.runtime.input_mut().push_bytes(bad_utf8_66);
    let _ = h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("")
    );

    // Valid edge: bottom-right corner with huge `s=7:w=7` wraps+scrolls cleanly with DECAWM on,
    // and clamps directly at `(cols-1, rows-1)` with `\x1b[?7l`; `n=5:d=3` resets invalid fraction.
    let rows = h.runtime.terminal().size().1;
    let cols = h.runtime.terminal().size().0;
    h.runtime.input_mut().push_str(format!(
        "\x1b[{rows};{cols}H\x1b]66;s=7:w=7:n=5:d=3;W\x07\x1b[?7l\x1b[{rows};{cols}H\x1b]66;s=7:w=7:n=5:d=3;X\x07\x1b[?7h"
    ));
    let edge66_frame = h.run_frame();
    let bottom_cells = h
        .runtime
        .terminal()
        .active_snapshot()
        .visible_rows
        .get(usize::from(rows - 1))
        .expect("bottom row")
        .cells();
    assert_eq!(bottom_cells[0].codepoint, 'W');
    let corner_cell = bottom_cells[usize::from(cols - 1)];
    assert_eq!(corner_cell.codepoint, 'X');
    assert_eq!(
        h.runtime
            .terminal()
            .cell_extra(corner_cell.extra_id)
            .and_then(|e| e.text_sizing),
        Some(TextSizing {
            scale: 7,
            width: 7,
            frac_num: 0,
            frac_den: 0,
            v_align: 0,
            h_align: 0,
        })
    );
    let edge66_insp = edge66_frame.inspect_shapes();
    assert!(!edge66_insp.text_mesh_vertices().is_empty());
    let edge66_bounds = edge66_insp
        .text_mesh_bounds()
        .expect("OSC 66 s=7 glyphs rendered");
    assert!(edge66_bounds.height() > edge66_frame.terminal_response.cell_size.y * 2.0);

    // 3. Malformed & flood OSC 99 payloads:
    //    bad IDs, >64-byte ID, invalid base64, invalid UTF-8 base64, >64 KiB multi-chunk overflow,
    //    and >16 pending IDs FIFO eviction.
    let long_id = "x".repeat(65);
    let big_33k = "B".repeat(33 * 1024);
    let mut osc99_attacks = Vec::new();
    osc99_attacks.extend_from_slice(
        concat!(
            "\x1b]99;\x1b\\",
            "\x1b]99;i=../escape:p=title:d=1;BadPath\x1b\\",
            "\x1b]99;i=bad/id:p=body:d=1;BadSlash\x1b\\",
            "\x1b]99;i=a b:p=title:d=1;BadSpace\x1b\\",
            "\x1b]99;i=badb64:p=body:e=1:d=1;not_base64!\x1b\\",
            "\x1b]99;i=badutf8:p=body:e=1:d=1;/w==\x1b\\",
            "\x1b]99;i=badu:p=title:u=99:o=bogus:d=0;Start\x1b\\",
            "\x1b]99;i=badu:p=unknown:d=0;Skip\x1b\\",
            "\x1b]99;i=badu:d=1;\x1b\\",
        )
        .as_bytes(),
    );
    osc99_attacks
        .extend_from_slice(format!("\x1b]99;i={long_id}:p=title:d=1;TooLongId\x1b\\").as_bytes());
    // Multi-chunk 33 KiB + 33 KiB = 66 KiB > MAX_OSC99_PAYLOAD_BYTES (64 KiB) aborts pending entry.
    osc99_attacks
        .extend_from_slice(format!("\x1b]99;i=bomb:p=title:d=0;{big_33k}\x1b\\").as_bytes());
    osc99_attacks
        .extend_from_slice(format!("\x1b]99;i=bomb:p=body:d=1;{big_33k}\x1b\\").as_bytes());
    // Flood 20 distinct incomplete IDs -> oldest 4 (`f00`..`f03`) are evicted (cap 16).
    for i in 0..20 {
        osc99_attacks
            .extend_from_slice(format!("\x1b]99;i=f{i:02}:p=title:d=0;T{i:02}\x1b\\").as_bytes());
    }
    osc99_attacks.extend_from_slice(b"\x1b]99;i=f00:d=1\x1b\\\x1b]99;i=f04:p=body:d=1;B04\x1b\\");
    h.runtime.input_mut().push_bytes(osc99_attacks);
    let mut notifications: Vec<(String, String)> = Vec::new();
    while !h.runtime.input().is_drained() {
        let frame = h.run_frame();
        notifications.extend(
            frame
                .terminal_response
                .events
                .into_iter()
                .filter_map(|e| match e {
                    TerminalEvent::Notification { title, body } => Some((title, body)),
                    _ => None,
                }),
        );
    }
    assert_eq!(
        notifications,
        vec![
            ("Start".to_owned(), String::new()),
            ("T04".to_owned(), "B04".to_owned()),
        ]
    );
}

#[test]
fn t5_m2_out_of_bounds_deccra_decfra_decsera_and_wide_invariants() {
    use egui_xterm_core::CellFlags;

    let assert_wide_invariants = |term: &Terminal| {
        let (cols, rows) = term.size();
        for r in 0..rows {
            let mut c = 0u16;
            while c < cols {
                let cell = term.cell_at(c, r).expect("valid cell");
                assert!(!cell.is_wide_spacer, "orphan wide spacer at ({c}, {r})");
                if cell.is_wide {
                    assert!(c + 1 < cols, "wide lead at right edge ({c}, {r})");
                    let next = term.cell_at(c + 1, r).expect("wide spacer");
                    assert!(next.is_wide_spacer, "missing wide spacer after ({c}, {r})");
                    c += 2;
                } else {
                    c += 1;
                }
            }
        }
    };

    let mut h = HeadlessTermHarness::scripted();
    h.set_viewport_size(140.0, 120.0);
    let warm = h.run_frame();
    let cols = warm.terminal_response.cols;
    let rows = warm.terminal_response.rows;
    assert!(cols >= 8 && rows >= 5);

    // Seed sentinel row 0, protected wide CJK on row 1, unprotected wide CJK on row 2.
    h.runtime.input_mut().push_str(concat!(
        "\x1b[1;1HABCDEFGH",
        "\x1b[2;1H\x1b[1\"q中文\x1b[0\"qXY",
        "\x1b[3;1H中文XY",
    ));
    let _ = h.run_frame();
    assert_wide_invariants(h.runtime.terminal());

    // 1. Out-of-bounds and invalid DECFRA (`$ x`): invalid Pch (0, BEL=7, CR=13, DEL=127, C1=128,
    //    256, 9999), inverted rects, OOB start coords, and clamped `65535`.
    h.runtime.input_mut().push_str(concat!(
        "\x1b[0;1;1;1;2$x",
        "\x1b[7;1;1;1;2$x",
        "\x1b[13;1;1;1;2$x",
        "\x1b[127;1;1;1;2$x",
        "\x1b[128;1;1;1;2$x",
        "\x1b[256;1;1;1;2$x",
        "\x1b[9999;1;1;1;2$x",
        "\x1b[46;4;1;2;3$x",
        "\x1b[46;1;5;2;2$x",
        "\x1b[46;999;1;999;2$x",
        "\x1b[46;1;999;2;999$x",
    ));
    let bad_fra = h.run_frame();
    assert!(
        !bad_fra
            .terminal_response
            .events
            .contains(&TerminalEvent::Bell)
    );
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("ABCDEFGH")
    );
    assert_wide_invariants(h.runtime.terminal());

    // DECFRA slicing the right half of wide '中' at row 3, col 2 and clamping `Pb=65535, Pr=65535`.
    h.runtime.input_mut().push_str("\x1b[46;3;2;65535;65535$x");
    let _ = h.run_frame();
    assert_wide_invariants(h.runtime.terminal());
    assert_eq!(
        h.runtime
            .terminal()
            .cell_at(0, 2)
            .expect("sliced wide left")
            .ch,
        ' '
    );
    assert_eq!(
        h.runtime.terminal().cell_at(1, 2).expect("filled dot").ch,
        '.'
    );

    // 2. Out-of-bounds DECSERA (`$ {`) and DECERA (`$ z`): inverted/OOB no-ops + clamped `65535`
    //    selectively preserves protected '中文' on row 2 while erasing unprotected 'XY'.
    h.runtime.input_mut().push_str(concat!(
        "\x1b[4;1;2;3${",
        "\x1b[1;5;2;2${",
        "\x1b[999;999;999;999${",
        "\x1b[999;999;999;999$z",
        "\x1b[2;1;65535;65535${",
    ));
    let _ = h.run_frame();
    assert_wide_invariants(h.runtime.terminal());
    let protected_lead = h.runtime.terminal().cell_at(0, 1).expect("中");
    assert_eq!(protected_lead.ch, '中');
    assert!(protected_lead.flags.contains(CellFlags::PROTECTED));
    assert_eq!(
        h.runtime.terminal().cell_at(4, 1).expect("erased X").ch,
        ' '
    );
    assert_eq!(
        h.runtime.terminal().cell_at(1, 2).expect("erased dot").ch,
        ' '
    );

    // 3. Out-of-bounds DECCRA (`$ v`): invalid page, inverted source, OOB source/dest,
    //    dest clipping at bottom-right corner, wide-char boundary slicing, and DECOM/DECLRMM bounds.
    h.runtime.input_mut().push_str(concat!(
        "\x1b[1;1;1;4;2;4;1;1$v",
        "\x1b[1;1;1;4;1;4;1;9$v",
        "\x1b[3;1;1;4;1;4;1;1$v",
        "\x1b[999;1;999;4;1;4;1;1$v",
        "\x1b[1;1;1;4;1;999;1;1$v",
        "\x1b[1;1;1;4;1;1;999;1$v",
    ));
    let _ = h.run_frame();
    assert_eq!(
        h.runtime.terminal().visible_line_text(3).as_deref(),
        Some("")
    );

    // Copy overlapping protected '中文' sliced at col 2..=3 into bottom-right corner `(rows, cols-1)`.
    h.runtime
        .input_mut()
        .push_str(format!("\x1b[2;2;2;4;1;{rows};{};1$v", cols - 1));
    let _ = h.run_frame();
    assert_wide_invariants(h.runtime.terminal());

    // Under DECOM + DECLRMM + DECSTBM, OOB DECCRA/DECFRA/DECSERA stay strictly inside margins.
    h.runtime.input_mut().push_str(concat!(
        "\x1b[2;4r\x1b[?69h\x1b[3;6s\x1b[?6h",
        "\x1b[35;1;1;99;99$x",
        "\x1b[1;1;99;99;1;2;2;1$v",
        "\x1b[2;2;99;99${",
    ));
    let final_frame = h.run_frame();
    assert_wide_invariants(h.runtime.terminal());
    assert_eq!(
        h.runtime.terminal().visible_line_text(0).as_deref(),
        Some("ABCDEFGH")
    );
    assert!(!final_frame.inspect_shapes().text_mesh_vertices().is_empty());
}

#[test]
fn t5_m2_bracketed_paste_breakout_and_control_smuggling_sanitization() {
    use egui_xterm_core::{Modes, format_paste_bytes, format_paste_bytes_into};

    let payloads = [
        "cmd\x1b[201~evil",
        "\x1b[200~nested\x1b[201~",
        "\x1b[20\x1b[201~1~whoami\r",
        "\x1b[2\x1b[20\x1b[201~1~01~rm -rf /\n",
        "\x1b[200\x1b[201~~\x1b[20\x1b[200~0~ls",
        "ok\x1b]52;c;AAAA\x07\x1b_Ga=T\x1b\\\x1bP$qm\x1b\\\x00\x03\x08\x18\x1a\x7f\u{0085}\u{009B}201~中文🚀\t\n\r",
    ];

    let assert_paste_safe = |bytes: &[u8], context: &str| {
        assert!(
            bytes.starts_with(b"\x1b[200~") && bytes.ends_with(b"\x1b[201~"),
            "{context}: missing bracket markers in {bytes:?}"
        );
        let inner = &bytes[6..bytes.len() - 6];
        assert!(
            !inner.contains(&0x1B),
            "{context}: raw ESC leaked into {inner:?}"
        );
        assert!(
            !inner
                .windows(6)
                .any(|w| w == b"\x1b[201~" || w == b"\x1b[200~"),
            "{context}: bracket marker leaked into {inner:?}"
        );
        assert!(
            !inner.contains(&0x7F),
            "{context}: DEL leaked into {inner:?}"
        );
        let inner_str = std::str::from_utf8(inner).expect("valid utf8 inside paste");
        for ch in inner_str.chars() {
            if ch.is_control() {
                assert!(
                    matches!(ch, '\t' | '\n' | '\r'),
                    "{context}: unsafe control {ch:?} leaked"
                );
            }
        }
    };

    // 1. Core `format_paste_bytes` & `format_paste_bytes_into` (including non-empty prefix buffer).
    let modes_on = Modes {
        bracketed_paste: true,
        ..Modes::default()
    };
    for payload in payloads {
        let formatted = format_paste_bytes(&modes_on, payload);
        assert_paste_safe(&formatted, payload);

        let mut buf = b"PREFIX:".to_vec();
        format_paste_bytes_into(&modes_on, payload, &mut buf);
        assert!(buf.starts_with(b"PREFIX:"));
        assert_paste_safe(&buf[7..], payload);
    }

    // Check specific preserved content after stripping triple-nested breakout + C1 CSI.
    let triple = format_paste_bytes(&modes_on, "\x1b[2\x1b[20\x1b[201~1~01~whoami\r");
    assert_eq!(triple, b"\x1b[200~whoami\r\x1b[201~");
    let mixed = format_paste_bytes(&modes_on, payloads[5]);
    let mixed_inner = std::str::from_utf8(&mixed[6..mixed.len() - 6]).expect("utf8");
    assert!(mixed_inner.contains("ok"));
    assert!(mixed_inner.contains("中文🚀\t\n\r"));
    assert!(!mixed_inner.contains('\u{009B}'));

    // 2. HeadlessTermHarness end-to-end `Event::Paste` under `?2004h` and `?2004l`.
    let mut h = HeadlessTermHarness::scripted();
    h.focus_terminal();
    h.runtime.input_mut().push_str("\x1b[?2004h");
    let _ = h.run_frame();
    h.runtime.input_mut().clear_written();

    for payload in payloads {
        h.inject_paste(payload);
        let _ = h.run_frame();
        let written = h.runtime.input_mut().take_written();
        assert_paste_safe(&written, payload);
    }

    // Disabling ?2004 disables bracketed paste markers; re-enabling ?2004 restores sanitization.
    h.runtime.input_mut().push_str("\x1b[?2004l");
    let _ = h.run_frame();
    h.runtime.input_mut().clear_written();
    h.inject_paste("plain");
    let _ = h.run_frame();
    assert_eq!(h.runtime.input_mut().take_written(), b"plain");

    h.runtime.input_mut().push_str("\x1b[?2004h");
    let _ = h.run_frame();
    h.runtime.input_mut().clear_written();
    h.inject_paste("safe\x1b[201~breakout");
    let _ = h.run_frame();
    assert_eq!(
        h.runtime.input().written_bytes(),
        b"\x1b[200~safebreakout\x1b[201~"
    );
}
