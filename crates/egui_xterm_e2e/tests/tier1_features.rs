//! Tier 1 — Category-Partition Happy-Path E2E Tests (`F2`–`F21`, 125 specs).
//!
//! Core tests exercise M1+M2 (`egui_xterm_core::{input, parser, color, grid, term}`),
//! while feature/target gates cover optional M3 features, `egui_xterm` headless
//! runtime/rendering, and desktop PTY tests.

// ===========================================================================
// F2 — TerminalInput Trait & Adapters (6 specs, ungated M1)
// ===========================================================================

use std::cell::RefCell;
use std::rc::Rc;

use egui_xterm::{
    ChannelInput, FnInput, RingBufferInput, TerminalInput, TerminalIoError, TerminalSize,
};
use egui_xterm_core::parser::{CsiParams, OscTerminator, Parser, State, VtHandler};
use egui_xterm_e2e::{LocalRcInput, ScriptedInput};

struct MinimalSource {
    data: &'static [u8],
    pos: usize,
}

impl MinimalSource {
    const fn new(data: &'static [u8]) -> Self {
        Self { data, pos: 0 }
    }
}

impl TerminalInput for MinimalSource {
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
        let remaining = &self.data[self.pos..];
        let n = remaining.len().min(buf.len());
        buf[..n].copy_from_slice(&remaining[..n]);
        self.pos += n;
        Ok(n)
    }
}

#[test]
fn t1_f2_01_minimal_single_method_custom_terminal_input() {
    let mut src = MinimalSource::new(b"hello_trait");
    let mut buf = [0u8; 32];
    let n = src.poll_read(&mut buf).expect("poll_read succeeds");
    assert_eq!(&buf[..n], b"hello_trait");

    assert_eq!(src.write_bytes(b"abc"), Ok(()));
    assert_eq!(
        src.resize(TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 640,
            pixel_height: 480,
        }),
        Ok(())
    );
    assert!(!src.is_closed());
}

#[test]
fn t1_f2_02_fn_input_closure_streams_chunks() {
    let chunks: [&[u8]; 2] = [b"chunk1-", b"chunk2"];
    let mut idx = 0usize;
    let mut input = FnInput::new(|buf: &mut [u8]| {
        if let Some(chunk) = chunks.get(idx) {
            idx += 1;
            let n = chunk.len().min(buf.len());
            buf[..n].copy_from_slice(&chunk[..n]);
            Ok(n)
        } else {
            Ok(0)
        }
    });

    let mut out = Vec::new();
    let mut buf = [0u8; 32];
    for _ in 0..3 {
        let n = input.poll_read(&mut buf).expect("poll_read ok");
        out.extend_from_slice(&buf[..n]);
    }
    assert_eq!(out, b"chunk1-chunk2");
    assert_eq!(input.poll_read(&mut buf), Ok(0));
    assert!(!input.is_closed());
}

#[test]
fn t1_f2_03_channel_input_non_blocking_transfer() {
    let (tx, mut input) = ChannelInput::pair();
    tx.send(b"alpha-".to_vec()).expect("send 1");
    tx.send(b"beta-".to_vec()).expect("send 2");
    tx.send(b"gamma".to_vec()).expect("send 3");

    let mut collected = Vec::new();
    let mut buf = [0u8; 64];
    loop {
        let n = input.poll_read(&mut buf).expect("non-blocking read");
        if n == 0 {
            break;
        }
        collected.extend_from_slice(&buf[..n]);
    }
    assert_eq!(collected, b"alpha-beta-gamma");
    assert_eq!(input.poll_read(&mut buf), Ok(0));
    assert!(!input.is_closed());
}

#[test]
fn t1_f2_04_ring_buffer_input_fifo_and_wraparound() {
    let mut ring = RingBufferInput::with_capacity(16);
    ring.push(b"0123456789");
    let mut first = [0u8; 6];
    assert_eq!(ring.poll_read(&mut first), Ok(6));
    assert_eq!(&first, b"012345");

    ring.push(b"ABCDEFGHIJ");
    let mut rest = [0u8; 32];
    assert_eq!(ring.poll_read(&mut rest), Ok(14));
    assert_eq!(&rest[..14], b"6789ABCDEFGHIJ");
    assert_eq!(ring.pending_len(), 0);
}

#[test]
fn t1_f2_05_blanket_impls_box_and_mut_ref() {
    let mut ring = RingBufferInput::from_bytes(b"box_and_ref");
    let sz1 = TerminalSize::new(90, 30);
    let sz2 = TerminalSize::new(120, 40);

    {
        let mut_dyn: &mut dyn TerminalInput = &mut ring;
        let mut buf = [0u8; 4];
        assert_eq!(mut_dyn.poll_read(&mut buf), Ok(4));
        assert_eq!(&buf, b"box_");
        assert_eq!(mut_dyn.write_bytes(b"via_ref "), Ok(()));
        assert_eq!(mut_dyn.resize(sz1), Ok(()));
        assert!(!mut_dyn.is_closed());
    }
    assert_eq!(ring.last_size(), Some(sz1));

    let mut boxed: Box<dyn TerminalInput> = Box::new(ring);
    let mut rest = [0u8; 16];
    assert_eq!(boxed.poll_read(&mut rest), Ok(7));
    assert_eq!(&rest[..7], b"and_ref");
    assert_eq!(boxed.write_bytes(b"via_box"), Ok(()));
    assert_eq!(boxed.resize(sz2), Ok(()));
    assert!(!boxed.is_closed());
}

#[test]
fn t1_f2_06_non_send_local_rc_input_compatibility() {
    fn drive<I: TerminalInput>(src: &mut I) -> Vec<u8> {
        let mut buf = [0u8; 16];
        let n = src.poll_read(&mut buf).expect("drive poll_read");
        src.write_bytes(b"ack").expect("drive write_bytes");
        src.resize(TerminalSize::new(80, 25)).expect("drive resize");
        buf[..n].to_vec()
    }

    let shared = Rc::new(RefCell::new(ScriptedInput::from_bytes(b"hello")));
    let mut rc_input = LocalRcInput::new(Rc::clone(&shared));
    let read_via_generic = drive(&mut rc_input);
    assert_eq!(read_via_generic, b"hello");

    shared.borrow_mut().push_str("_dyn");
    let dyn_ref: &mut dyn TerminalInput = &mut rc_input;
    let mut buf = [0u8; 16];
    assert_eq!(dyn_ref.poll_read(&mut buf), Ok(4));
    assert_eq!(&buf[..4], b"_dyn");

    let mut boxed: Box<dyn TerminalInput> = Box::new(rc_input);
    assert!(!boxed.is_closed());
    assert_eq!(boxed.write_bytes(b"!"), Ok(()));

    assert_eq!(shared.borrow().written_bytes(), b"ack!");
    assert_eq!(
        shared.borrow().last_resize(),
        Some(TerminalSize::new(80, 25))
    );
}

// ===========================================================================
// F3 — VT500 Parser & memchr Fast-Path (6 specs, ungated M1)
// ===========================================================================

#[derive(Default)]
struct RecordingHandler {
    printed: String,
    ascii_runs: Vec<String>,
    executed: Vec<u8>,
    csi_events: Vec<(Vec<Vec<u16>>, Vec<u8>, u8)>,
    osc_events: Vec<(Vec<Vec<u8>>, OscTerminator)>,
    esc_events: Vec<(Vec<u8>, u8)>,
    dcs_hooks: usize,
    dcs_puts: Vec<u8>,
    dcs_unhooks: usize,
}

impl VtHandler for RecordingHandler {
    fn print(&mut self, ch: char) {
        self.printed.push(ch);
    }

    fn print_ascii_run(&mut self, bytes: &[u8]) {
        let s = std::str::from_utf8(bytes)
            .expect("ASCII run is valid UTF-8")
            .to_owned();
        self.ascii_runs.push(s.clone());
        self.printed.push_str(&s);
    }

    fn execute(&mut self, byte: u8) {
        self.executed.push(byte);
    }

    fn hook(&mut self, _params: &CsiParams, _intermediates: &[u8], _action: u8) {
        self.dcs_hooks += 1;
    }

    fn put(&mut self, byte: u8) {
        self.dcs_puts.push(byte);
    }

    fn unhook(&mut self) {
        self.dcs_unhooks += 1;
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], terminator: OscTerminator) {
        let owned = params.iter().map(|p| p.to_vec()).collect();
        self.osc_events.push((owned, terminator));
    }

    fn csi_dispatch(&mut self, params: &CsiParams, intermediates: &[u8], action: u8) {
        let owned = params.iter().map(<[u16]>::to_vec).collect();
        self.csi_events
            .push((owned, intermediates.to_vec(), action));
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], action: u8) {
        self.esc_events.push((intermediates.to_vec(), action));
    }
}

#[test]
fn t1_f3_01_ascii_ground_fast_path_bulk_print() {
    let mut parser = Parser::new();
    let mut handler = RecordingHandler::default();
    let payload = b"The quick brown fox jumps over 12345 lazy dogs!";
    parser.advance(payload, &mut handler);

    assert_eq!(
        handler.printed,
        "The quick brown fox jumps over 12345 lazy dogs!"
    );
    assert_eq!(handler.printed.len(), 47);
    assert!(!handler.ascii_runs.is_empty());
    assert_eq!(
        handler.ascii_runs.iter().map(String::len).sum::<usize>(),
        47
    );
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn t1_f3_02_c0_control_characters_in_ground() {
    let mut parser = Parser::new();
    let mut handler = RecordingHandler::default();
    parser.advance(b"ABCD\x08X\r\n\tY\x07", &mut handler);

    assert_eq!(handler.printed, "ABCDXY");
    assert_eq!(handler.executed, vec![0x08, 0x0D, 0x0A, 0x09, 0x07]);
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn t1_f3_03_csi_dispatch_params_and_intermediates() {
    let mut parser = Parser::new();
    let mut handler = RecordingHandler::default();
    parser.advance(b"A\x1b[3;5HB\x1b[2C\x1b[?25h", &mut handler);

    assert_eq!(handler.printed, "AB");
    assert_eq!(
        handler.csi_events,
        vec![
            (vec![vec![3], vec![5]], Vec::new(), b'H'),
            (vec![vec![2]], Vec::new(), b'C'),
            (vec![vec![25]], b"?".to_vec(), b'h'),
        ]
    );
}

#[test]
fn t1_f3_04_osc_dispatch_bel_and_st() {
    let mut parser = Parser::new();
    let mut handler = RecordingHandler::default();
    parser.advance(b"\x1b]2;TitleOne\x07\x1b]2;TitleTwo\x1b\\", &mut handler);

    assert!(handler.printed.is_empty());
    assert_eq!(
        handler.osc_events,
        vec![
            (
                vec![b"2".to_vec(), b"TitleOne".to_vec()],
                OscTerminator::Bel
            ),
            (vec![b"2".to_vec(), b"TitleTwo".to_vec()], OscTerminator::St),
        ]
    );
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn t1_f3_05_multibyte_utf8_streaming_ground() {
    let mut parser = Parser::new();
    let mut handler = RecordingHandler::default();
    let text = "Héllo🚀世界";
    parser.advance(text.as_bytes(), &mut handler);

    assert_eq!(handler.printed, text);
    assert!(!handler.printed.contains('\u{FFFD}'));
}

#[test]
fn t1_f3_06_ignored_control_strings_dcs_apc_pm_sos() {
    let mut parser = Parser::new();
    let mut handler = RecordingHandler::default();
    parser.advance(
        b"A\x1bPignored dcs\x1b\\B\x1b_ignored apc\x1b\\C\x1b^pm\x1b\\D\x1bXsos\x1b\\E",
        &mut handler,
    );

    assert_eq!(handler.printed, "ABCDE");
    assert_eq!(handler.dcs_hooks, 1);
    assert_eq!(handler.dcs_unhooks, 1);
    assert_eq!(parser.state(), State::Ground);
}

// ===========================================================================
// F4 — Compact Cell & Sparse CellExtra (6 specs, ungated M2)
// ===========================================================================

mod stage_m2_f4 {
    use egui_xterm_core::{CoreConfig, PackedColor, Terminal, UnderlineStyle};

    #[test]
    fn t1_f4_01_ascii_and_latin1_single_width_cells() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance("café".as_bytes());
        assert_eq!(term.cursor_pos(), (4, 0));
        for (col, expected) in ['c', 'a', 'f', 'é'].into_iter().enumerate() {
            let cell = term
                .cell_at(u16::try_from(col).expect("col fits in u16"), 0)
                .expect("cell exists");
            assert_eq!(cell.ch, expected);
            assert_eq!(cell.width, 1);
            assert!(!cell.is_wide);
            assert!(!cell.is_wide_spacer);
        }
    }

    #[test]
    fn t1_f4_02_wide_cjk_ideograph_and_spacer_invariant() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance("中A".as_bytes());
        assert_eq!(term.cursor_pos(), (3, 0));

        let lead = term.cell_at(0, 0).expect("lead cell");
        assert_eq!(lead.ch, '中');
        assert_eq!(lead.width, 2);
        assert!(lead.is_wide);
        assert!(!lead.is_wide_spacer);

        let spacer = term.cell_at(1, 0).expect("spacer cell");
        assert!(!spacer.is_wide);
        assert!(spacer.is_wide_spacer);
        assert_eq!(spacer.width, 0);

        let trailing = term.cell_at(2, 0).expect("A cell");
        assert_eq!(trailing.ch, 'A');
        assert_eq!(trailing.width, 1);
    }

    #[test]
    fn t1_f4_03_combining_mark_grapheme_cluster() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance("e\u{0301}X".as_bytes());
        assert_eq!(term.cursor_pos(), (2, 0));

        let cell0 = term.cell_at(0, 0).expect("combining cell");
        assert_eq!(cell0.grapheme, "e\u{0301}");
        assert_eq!(cell0.width, 1);

        let cell1 = term.cell_at(1, 0).expect("X cell");
        assert_eq!(cell1.ch, 'X');
    }

    #[test]
    fn t1_f4_04_zwj_emoji_grapheme_cluster() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance("👨‍👩‍👧Z".as_bytes());
        assert_eq!(term.cursor_pos(), (3, 0));

        let lead = term.cell_at(0, 0).expect("emoji lead");
        assert_eq!(lead.grapheme, "👨‍👩‍👧");
        assert!(lead.is_wide);
        assert_eq!(lead.width, 2);

        let spacer = term.cell_at(1, 0).expect("emoji spacer");
        assert!(spacer.is_wide_spacer);

        let z = term.cell_at(2, 0).expect("Z cell");
        assert_eq!(z.ch, 'Z');
    }

    #[test]
    fn t1_f4_05_sgr_58_semicolon_underline_color() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"\x1b[4:3;58;2;255;128;0mA\x1b[58;5;196mB\x1b[59mC");

        let a = term.cell_at(0, 0).expect("cell A");
        assert_eq!(a.underline_style, UnderlineStyle::Curly);
        assert_eq!(a.underline_color, Some(PackedColor::rgb(255, 128, 0)));

        let b = term.cell_at(1, 0).expect("cell B");
        assert_eq!(b.underline_style, UnderlineStyle::Curly);
        assert_eq!(b.underline_color, Some(PackedColor::indexed(196)));

        let c = term.cell_at(2, 0).expect("cell C");
        assert_eq!(c.underline_style, UnderlineStyle::Curly);
        assert_eq!(c.underline_color, None);
    }

    #[test]
    fn t1_f4_06_sgr_58_colon_subparameter_syntax() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"\x1b[4;58:2::10:20:30mX\x1b[58:5:42mY");

        let x = term.cell_at(0, 0).expect("cell X");
        assert_eq!(x.underline_style, UnderlineStyle::Single);
        assert_eq!(x.underline_color, Some(PackedColor::rgb(10, 20, 30)));

        let y = term.cell_at(1, 0).expect("cell Y");
        assert_eq!(y.underline_style, UnderlineStyle::Single);
        assert_eq!(y.underline_color, Some(PackedColor::indexed(42)));
    }
}

// ===========================================================================
// F5 — Ring Scrollback & Row Pooling (6 specs, ungated M2)
// ===========================================================================

mod stage_m2_f5 {
    use egui_xterm_core::{CoreConfig, PackedColor, Terminal};

    #[test]
    fn t1_f5_01_scrolled_off_lines_enter_scrollback() {
        let cfg = CoreConfig {
            scrollback_limit: 100,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(20, 3, cfg);
        term.advance(b"L0\r\nL1\r\nL2\r\nL3\r\nL4");
        assert_eq!(term.visible_lines(), vec!["L2", "L3", "L4"]);
        assert_eq!(term.scrollback_len(), 2);

        term.scroll_display(2);
        assert_eq!(term.display_offset(), 2);
        assert_eq!(term.visible_lines(), vec!["L0", "L1", "L2"]);
    }

    #[test]
    fn t1_f5_02_scroll_display_up_and_down_navigation() {
        let cfg = CoreConfig {
            scrollback_limit: 100,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(20, 3, cfg);
        for i in 0..10 {
            if i > 0 {
                term.advance(b"\r\n");
            }
            term.advance(format!("L{i}").as_bytes());
        }
        assert_eq!(term.scrollback_len(), 7);

        term.scroll_display(4);
        assert_eq!(term.display_offset(), 4);
        assert_eq!(term.visible_lines(), vec!["L3", "L4", "L5"]);

        term.scroll_display(-2);
        assert_eq!(term.display_offset(), 2);
        assert_eq!(term.visible_lines(), vec!["L5", "L6", "L7"]);
    }

    #[test]
    fn t1_f5_03_scroll_to_bottom_resets_display_offset() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        for i in 0..10 {
            if i > 0 {
                term.advance(b"\r\n");
            }
            term.advance(format!("L{i}").as_bytes());
        }
        term.scroll_display(5);
        assert_eq!(term.display_offset(), 5);

        term.scroll_to_bottom();
        assert_eq!(term.display_offset(), 0);
        assert_eq!(term.visible_lines(), vec!["L7", "L8", "L9"]);
    }

    #[test]
    fn t1_f5_04_scrollback_limit_evicts_oldest_lines() {
        let cfg = CoreConfig {
            scrollback_limit: 5,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(20, 3, cfg);
        for i in 0..12 {
            if i > 0 {
                term.advance(b"\r\n");
            }
            term.advance(format!("L{i}").as_bytes());
        }
        assert_eq!(term.scrollback_len(), 5);
        assert_eq!(term.scrollback_line_text(0), Some("L4".to_owned()));
        assert_eq!(term.scrollback_line_text(4), Some("L8".to_owned()));

        term.scroll_display(100);
        assert_eq!(term.display_offset(), 5);
        assert_eq!(term.visible_lines(), vec!["L4", "L5", "L6"]);
    }

    #[test]
    fn t1_f5_05_row_pooling_resets_attributes_across_churn() {
        let cfg = CoreConfig {
            scrollback_limit: 10,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(20, 4, cfg);
        for i in 0..500 {
            term.advance(format!("\x1b[31;4:1;58;2;1;2;3mROW_{i}\r\n").as_bytes());
        }
        term.advance(b"\x1b[0m\x1b[2J\x1b[HP0\r\nP1\r\nP2\r\nP3");

        assert_eq!(term.visible_lines(), vec!["P0", "P1", "P2", "P3"]);
        for row in 0..4u16 {
            let cell = term.cell_at(0, row).expect("cell exists");
            assert_eq!(cell.fg, PackedColor::Default);
            assert_eq!(cell.bg, PackedColor::Default);
            assert_eq!(cell.underline_color, None);
        }
    }

    #[test]
    fn t1_f5_06_ed_3_clears_scrollback_leaving_viewport_intact() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        for i in 0..13 {
            if i > 0 {
                term.advance(b"\r\n");
            }
            term.advance(format!("L{i}").as_bytes());
        }
        assert_eq!(term.scrollback_len(), 10);

        term.advance(b"\x1b[3J");
        assert_eq!(term.scrollback_len(), 0);
        term.scroll_display(5);
        assert_eq!(term.display_offset(), 0);
        assert_eq!(term.visible_lines(), vec!["L10", "L11", "L12"]);
    }
}

// ===========================================================================
// F6 — Lossless Resize Reflow (6 specs, ungated M2)
// ===========================================================================

mod stage_m2_f6 {
    use egui_xterm_core::{CoreConfig, SelectionKind, Terminal};

    #[test]
    fn t1_f6_01_narrowing_wraps_long_logical_line() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"ABCDEFGHIJKL");
        term.resize(6, 5);

        assert_eq!(term.size(), (6, 5));
        let lines = term.visible_lines();
        assert_eq!(lines[0], "ABCDEF");
        assert_eq!(lines[1], "GHIJKL");
    }

    #[test]
    fn t1_f6_02_widening_unwraps_soft_wrapped_lines() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"ABCDEFGHIJKL");
        term.resize(6, 5);
        term.resize(20, 5);

        assert_eq!(term.size(), (20, 5));
        assert_eq!(term.visible_line_text(0), Some("ABCDEFGHIJKL".to_owned()));
    }

    #[test]
    fn t1_f6_03_hard_newlines_never_unwrap_on_widening() {
        let mut term = Terminal::new(6, 5, CoreConfig::default());
        term.advance(b"ABC\r\nDEF");
        term.resize(20, 5);

        let lines = term.visible_lines();
        assert_eq!(lines[0], "ABC");
        assert_eq!(lines[1], "DEF");
    }

    #[test]
    fn t1_f6_04_cursor_tracks_across_wrap_and_unwrap() {
        let mut term = Terminal::new(10, 5, CoreConfig::default());
        term.advance(b"012345X");
        assert_eq!(term.cursor_pos(), (7, 0));

        term.resize(4, 5);
        assert_eq!(term.cursor_pos(), (3, 1));

        term.resize(10, 5);
        assert_eq!(term.cursor_pos(), (7, 0));
    }

    #[test]
    fn t1_f6_05_reflow_propagates_across_scrollback_and_viewport() {
        let cfg = CoreConfig {
            scrollback_limit: 50,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(10, 3, cfg);
        term.advance(b"1234567890ABCD\r\nLINE2\r\nLINE3");
        let expected_full = term.full_text();

        term.resize(5, 3);
        assert_eq!(term.full_text(), expected_full);
        assert!(term.scrollback_len() >= 2);

        term.resize(15, 3);
        assert_eq!(term.full_text(), expected_full);
        assert_eq!(
            term.visible_lines(),
            vec!["1234567890ABCD", "LINE2", "LINE3"]
        );
    }

    #[test]
    fn t1_f6_06_selection_tracks_reflowed_content() {
        let mut term = Terminal::new(10, 4, CoreConfig::default());
        term.advance(b"HELLO_WORLD");
        // At 10 cols: row 0 = "HELLO_WORL" (soft-wrapped), row 1 = "D".
        term.selection_start(0, 6, SelectionKind::Stream);
        term.selection_update(1, 0);
        term.selection_finish();
        assert_eq!(term.selection_text().as_deref(), Some("WORLD"));

        term.resize(6, 4);
        assert_eq!(term.selection_text().as_deref(), Some("WORLD"));
    }
}

// ===========================================================================
// F7 — Core VT Modes & Screen Control (7 specs, ungated M2)
// ===========================================================================

mod stage_m2_f7 {
    use egui_xterm_core::{CoreConfig, CursorShape, PackedColor, Terminal};

    #[test]
    fn t1_f7_01_alt_screen_1049_preserves_primary_grid_and_cursor() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"PRIMARY");
        assert_eq!(term.cursor_pos(), (7, 0));

        term.advance(b"\x1b[?1049hALT");
        assert!(term.is_alt_screen());
        assert_eq!(term.visible_line_text(0), Some("ALT".to_owned()));

        term.advance(b"\x1b[?1049l");
        assert!(!term.is_alt_screen());
        assert_eq!(term.visible_line_text(0), Some("PRIMARY".to_owned()));
        assert_eq!(term.cursor_pos(), (7, 0));
    }

    #[test]
    fn t1_f7_02_decstbm_scrolls_only_within_margins() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"R0\r\nR1\r\nR2\r\nR3\r\nR4");
        term.advance(b"\x1b[2;4r\x1b[4;1H\n");

        assert_eq!(term.visible_lines(), vec!["R0", "R2", "R3", "", "R4"]);
    }

    #[test]
    fn t1_f7_03_decom_origin_mode_relative_addressing() {
        let mut term = Terminal::new(20, 6, CoreConfig::default());
        term.advance(b"\x1b[3;5r\x1b[?6h\x1b[1;1HX");

        let cell = term.cell_at(0, 2).expect("origin row 2");
        assert_eq!(cell.ch, 'X');
    }

    #[test]
    fn t1_f7_04_decsc_decrc_save_and_restore_cursor_and_sgr() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"\x1b[2;4H\x1b[32m\x1b7\x1b[1;1H\x1b[0m\x1b8Z");

        let cell = term.cell_at(3, 1).expect("restored cell");
        assert_eq!(cell.ch, 'Z');
        assert_eq!(cell.fg, PackedColor::indexed(2));
        assert_eq!(term.cursor_pos(), (4, 1));
    }

    #[test]
    fn t1_f7_05_decscusr_cursor_shapes() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());

        term.advance(b"\x1b[2 q");
        assert_eq!(term.cursor_shape(), CursorShape::Block);
        assert!(!term.cursor_blink());

        term.advance(b"\x1b[4 q");
        assert_eq!(term.cursor_shape(), CursorShape::Underline);
        assert!(!term.cursor_blink());

        term.advance(b"\x1b[6 q");
        assert_eq!(term.cursor_shape(), CursorShape::Bar);
        assert!(!term.cursor_blink());
    }

    #[test]
    fn t1_f7_06_tab_stops_hts_tbc_and_cbt() {
        let mut term = Terminal::new(40, 5, CoreConfig::default());
        term.advance(b"\x1b[3g\x1b[1;6H\x1bH\x1b[1;12H\x1bH\r\tA\x1b[1ZX");

        assert_eq!(term.visible_line_text(0), Some("     X".to_owned()));
        assert_eq!(term.cursor_pos(), (6, 0));
    }

    #[test]
    fn t1_f7_07_dec_special_graphics_and_device_queries() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"\x1b(0lqk\x1b(BA\x1b[5n\x1b[6n\x1b[c\x1b[>c\x1b[?7$p");

        assert_eq!(term.visible_line_text(0), Some("┌─┐A".to_owned()));
        let outgoing = String::from_utf8(term.drain_outgoing()).expect("ASCII VT replies");
        assert!(outgoing.contains("\x1b[0n"), "missing DSR 5: {outgoing:?}");
        assert!(outgoing.contains("\x1b[1;5R"), "missing CPR: {outgoing:?}");
        assert!(outgoing.contains("\x1b[?"), "missing DA1: {outgoing:?}");
        assert!(outgoing.contains("\x1b[>"), "missing DA2: {outgoing:?}");
        assert!(
            outgoing.contains("\x1b[?7;1$y"),
            "missing DECRPM 7: {outgoing:?}"
        );
    }
}

// ===========================================================================
// F8 — Colors, SGR & OSC State (7 specs, ungated M2)
// ===========================================================================

mod stage_m2_f8 {
    use egui_xterm_core::{
        CellFlags, CoreConfig, PackedColor, Terminal, TerminalEvent, UnderlineStyle,
    };

    #[test]
    fn t1_f8_01_standard_16_ansi_colors() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"\x1b[31;44mA\x1b[92;105mB\x1b[39;49mC");

        let a = term.cell_at(0, 0).expect("A");
        assert_eq!(a.fg, PackedColor::indexed(1));
        assert_eq!(a.bg, PackedColor::indexed(4));

        let b = term.cell_at(1, 0).expect("B");
        assert_eq!(b.fg, PackedColor::indexed(10));
        assert_eq!(b.bg, PackedColor::indexed(13));

        let c = term.cell_at(2, 0).expect("C");
        assert_eq!(c.fg, PackedColor::Default);
        assert_eq!(c.bg, PackedColor::Default);
    }

    #[test]
    fn t1_f8_02_256_and_truecolor_semicolon_and_colon() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"\x1b[38;5;208;48;2;12;34;56mA\x1b[38:2::99:88:77;48:5:25mB");

        let a = term.cell_at(0, 0).expect("A");
        assert_eq!(a.fg, PackedColor::indexed(208));
        assert_eq!(a.bg, PackedColor::rgb(12, 34, 56));

        let b = term.cell_at(1, 0).expect("B");
        assert_eq!(b.fg, PackedColor::rgb(99, 88, 77));
        assert_eq!(b.bg, PackedColor::indexed(25));
    }

    #[test]
    fn t1_f8_03_all_five_underline_styles_and_reset() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"\x1b[4:1m1\x1b[4:2m2\x1b[4:3m3\x1b[4:4m4\x1b[4:5m5\x1b[4:0m0");

        let expected = [
            UnderlineStyle::Single,
            UnderlineStyle::Double,
            UnderlineStyle::Curly,
            UnderlineStyle::Dotted,
            UnderlineStyle::Dashed,
            UnderlineStyle::None,
        ];
        for (col, style) in expected.into_iter().enumerate() {
            let cell = term
                .cell_at(u16::try_from(col).expect("col fits in u16"), 0)
                .expect("underline cell");
            assert_eq!(cell.underline_style, style);
        }
    }

    #[test]
    fn t1_f8_04_sgr_attributes_and_individual_resets() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"\x1b[1;2;3;5;7;8;9;53mX\x1b[22;23;25;27;28;29;55mY");

        let x = term.cell_at(0, 0).expect("X");
        assert!(x.flags.contains(
            CellFlags::BOLD
                | CellFlags::DIM
                | CellFlags::ITALIC
                | CellFlags::BLINK_SLOW
                | CellFlags::INVERSE
                | CellFlags::HIDDEN
                | CellFlags::STRIKETHROUGH
                | CellFlags::OVERLINE
        ));

        let y = term.cell_at(1, 0).expect("Y");
        assert!(!y.flags.intersects(
            CellFlags::BOLD
                | CellFlags::DIM
                | CellFlags::ITALIC
                | CellFlags::BLINK_SLOW
                | CellFlags::INVERSE
                | CellFlags::HIDDEN
                | CellFlags::STRIKETHROUGH
                | CellFlags::OVERLINE
        ));
    }

    #[test]
    fn t1_f8_05_osc_title_icon_and_cwd_events() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(
            b"\x1b]0;Both\x07\x1b]1;Icon\x07\x1b]2;Win\x1b\\\x1b]7;file://localhost/tmp/work\x1b\\",
        );

        assert_eq!(term.title(), "Win");
        assert_eq!(term.icon_name(), "Icon");
        assert_eq!(term.cwd(), Some("/tmp/work"));

        let events = term.drain_events();
        assert!(events.contains(&TerminalEvent::TitleChanged("Both".to_owned())));
        assert!(events.contains(&TerminalEvent::TitleChanged("Win".to_owned())));
        assert!(events.contains(&TerminalEvent::CwdChanged("/tmp/work".to_owned())));
    }

    #[test]
    fn t1_f8_06_osc_palette_set_and_query() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(
            b"\x1b]4;1;rgb:aa/bb/cc\x07\x1b]4;1;?\x07\x1b]10;#112233\x07\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07",
        );

        let reply = String::from_utf8(term.drain_outgoing()).expect("valid OSC color replies");
        assert!(
            reply.contains("\x1b]4;1;rgb:aaaa/bbbb/cccc\x07"),
            "reply={reply:?}"
        );
        assert!(
            reply.contains("\x1b]10;rgb:1111/2222/3333\x07"),
            "reply={reply:?}"
        );
        assert!(reply.contains("\x1b]11;rgb:"), "reply={reply:?}");
        assert!(reply.contains("\x1b]12;rgb:"), "reply={reply:?}");
    }

    #[test]
    fn t1_f8_07_osc_52_clipboard_write_emits_event() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"\x1b]52;c;aGVsbG8=\x07");

        let events = term.drain_events();
        assert_eq!(
            events,
            vec![TerminalEvent::ClipboardWrite("hello".to_owned())]
        );
    }
}

// ===========================================================================
// F9 — DEC Mode 2026 Synchronized Output (6 specs, ungated M2)
// ===========================================================================

mod stage_m2_f9 {
    use egui_xterm_core::{CoreConfig, Terminal};

    #[test]
    fn t1_f9_01_decrqm_2026_reports_reset_initially() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance_with_time(b"\x1b[?2026$p", 1.0);
        assert_eq!(term.drain_outgoing(), b"\x1b[?2026;2$y");
    }

    #[test]
    fn t1_f9_02_decrqm_2026_reports_set_when_active() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance_with_time(b"\x1b[?2026h\x1b[?2026$p", 1.0);
        assert_eq!(term.drain_outgoing(), b"\x1b[?2026;1$y");
    }

    #[test]
    fn t1_f9_03_mode_2026_latches_frozen_snapshot_during_partial_updates() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance_with_time(b"FRAME_0", 0.5);
        term.advance_with_time(b"\x1b[?2026h\x1b[2J\x1b[HPARTIAL", 1.0);

        assert!(term.is_synchronized_output());
        assert_eq!(
            term.render_snapshot_line_text(0),
            Some("FRAME_0".to_owned())
        );
    }

    #[test]
    fn t1_f9_04_mode_2026_reset_commits_updated_state_atomically() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance_with_time(b"FRAME_0", 0.5);
        term.advance_with_time(b"\x1b[?2026h\x1b[2J\x1b[HPARTIAL", 1.0);
        assert_eq!(
            term.render_snapshot_line_text(0),
            Some("FRAME_0".to_owned())
        );

        term.advance_with_time(b"_COMPLETE\x1b[?2026l", 1.02);
        assert!(!term.is_synchronized_output());
        assert_eq!(
            term.render_snapshot_line_text(0),
            Some("PARTIAL_COMPLETE".to_owned())
        );
    }

    #[test]
    fn t1_f9_05_multichunk_sync_redraw_stays_latched_under_250ms() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance_with_time(b"OLD", 9.9);
        term.advance_with_time(b"\x1b[?2026h\x1b[2J\x1b[H1", 10.0);
        assert_eq!(term.render_snapshot_line_text(0), Some("OLD".to_owned()));

        term.advance_with_time(b"2", 10.1);
        assert_eq!(term.render_snapshot_line_text(0), Some("OLD".to_owned()));

        term.advance_with_time(b"3\x1b[?2026l", 10.2);
        assert_eq!(term.render_snapshot_line_text(0), Some("123".to_owned()));
    }

    #[test]
    fn t1_f9_06_is_synchronized_output_toggles_cleanly() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        assert!(!term.is_synchronized_output());

        term.advance_with_time(b"\x1b[?2026h", 0.0);
        assert!(term.is_synchronized_output());

        term.advance_with_time(b"\x1b[?2026l", 0.05);
        assert!(!term.is_synchronized_output());

        term.advance_with_time(b"LIVE", 0.06);
        assert_eq!(term.render_snapshot_line_text(0), Some("LIVE".to_owned()));
    }
}

// ===========================================================================
// F10 — Selection, Mouse & Input Modes (7 specs: 3 ungated M2 + 4 M4)
// ===========================================================================

mod stage_m2_f10 {
    use egui_xterm_core::{CoreConfig, SelectionKind, Terminal};

    #[test]
    fn t1_f10_01_stream_selection_across_multiple_rows() {
        let mut term = Terminal::new(20, 4, CoreConfig::default());
        term.advance(b"hello world\r\nfoo bar");
        term.selection_start(0, 6, SelectionKind::Stream);
        term.selection_update(1, 3);
        term.selection_finish();

        // Trailing spaces on hard-broken lines are trimmed cleanly per line.
        assert_eq!(term.selection_text().as_deref(), Some("world\nfoo"));
    }

    #[test]
    fn t1_f10_02_word_and_line_selection() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"alpha beta_1 gamma");

        term.selection_word(0, 7);
        assert_eq!(term.selection_text().as_deref(), Some("beta_1"));

        term.selection_line(0, 2);
        assert_eq!(term.selection_text().as_deref(), Some("alpha beta_1 gamma"));
    }

    #[test]
    fn t1_f10_03_block_rectangular_selection() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"ABCDE\r\n12345\r\nVWXYZ");
        term.selection_start(0, 1, SelectionKind::Block);
        term.selection_update(2, 3);
        term.selection_finish();

        assert_eq!(term.selection_text().as_deref(), Some("BCD\n234\nWXY"));
    }
}

mod stage_m4_f10 {
    use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Vec2};
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn t1_f10_04_decckm_and_deckpam_key_encoding() {
        let mut h = HeadlessTermHarness::scripted();
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        // 1. DECCKM off -> ArrowUp emits CSI A.
        h.tap_key(Key::ArrowUp, Modifiers::NONE);
        let _ = h.run_frame();
        assert_eq!(h.runtime.input_mut().take_written(), b"\x1b[A");

        // 2. DECCKM on (\x1b[?1h) + DECKPAM (\x1b=).
        h.runtime.input_mut().push_str("\x1b[?1h\x1b=");
        let _ = h.run_frame();
        h.tap_key(Key::ArrowUp, Modifiers::NONE);
        let _ = h.run_frame();
        assert_eq!(h.runtime.input_mut().take_written(), b"\x1bOA");

        // Exercise DECKPAM with Key::Enter (egui::Key lacks a distinct NumpadEnter
        // variant, so implementations may emit either keypad-application "\x1bOM"
        // or main-keyboard "\r").
        h.tap_key(Key::Enter, Modifiers::NONE);
        let _ = h.run_frame();
        let enter_pam = h.runtime.input_mut().take_written();
        assert!(
            enter_pam == b"\x1bOM" || enter_pam == b"\r",
            "expected DECKPAM Enter to emit \\x1bOM or \\r, got {enter_pam:?}"
        );

        // 3. Reset DECCKM (\x1b[?1l) and switch back to DECKPNM (\x1b>).
        h.runtime.input_mut().push_str("\x1b[?1l\x1b>");
        let _ = h.run_frame();
        h.tap_key(Key::ArrowUp, Modifiers::NONE);
        h.tap_key(Key::Enter, Modifiers::NONE);
        let _ = h.run_frame();
        assert_eq!(h.runtime.input_mut().take_written(), b"\x1b[A\r");
    }

    #[test]
    fn t1_f10_05_bracketed_paste_and_focus_reporting() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[?2004h\x1b[?1004h");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_paste("echo hi");
        let _ = h.run_frame();
        assert_eq!(
            h.runtime.input().written_bytes(),
            b"\x1b[200~echo hi\x1b[201~"
        );
        h.runtime.input_mut().clear_written();

        // Terminal already gained focus via `focus_terminal()` (which emitted `\x1b[I`
        // before `clear_written()`), so transition focus out then back in.
        h.queue_event(Event::WindowFocused(false));
        let _ = h.run_frame();
        h.queue_event(Event::WindowFocused(true));
        let _ = h.run_frame();
        assert_eq!(h.runtime.input().written_bytes(), b"\x1b[O\x1b[I");
    }

    #[test]
    fn t1_f10_06_mouse_tracking_sgr_and_x10_encoding() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[?1000;1006h");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        let p = h.cell_center(5, 3).expect("cell (5,3) in bounds");
        h.inject_click(p, PointerButton::Primary, Modifiers::NONE);
        assert_eq!(
            h.runtime.input().written_bytes(),
            b"\x1b[<0;6;4M\x1b[<0;6;4m"
        );
        h.runtime.input_mut().clear_written();

        h.runtime.input_mut().push_str("\x1b[?1002;1006h");
        let _ = h.run_frame();
        let p2 = h.cell_center(6, 3).expect("cell (6,3) in bounds");
        h.inject_drag_steps(p, p2, PointerButton::Primary, Modifiers::NONE, 1);
        assert!(h.runtime.input().written_str().contains("\x1b[<32;"));
        h.runtime.input_mut().clear_written();

        h.runtime.input_mut().push_str("\x1b[?1003;1006h");
        let _ = h.run_frame();
        h.inject_pointer_move(p);
        let _ = h.run_frame();
        assert!(h.runtime.input().written_str().contains("\x1b[<35;"));
        h.runtime.input_mut().clear_written();

        h.runtime.input_mut().push_str("\x1b[?1006l\x1b[?1000h");
        let _ = h.run_frame();
        h.inject_click(p, PointerButton::Primary, Modifiers::NONE);
        assert!(
            h.runtime
                .input()
                .written_bytes()
                .starts_with(b"\x1b[M\x20\x26\x24")
        );
    }

    #[test]
    fn t1_f10_07_alternate_scroll_mode_1007_on_alt_screen() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[?1007h\x1b[?1049h");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_scroll(MouseWheelUnit::Line, Vec2::new(0.0, 2.0), Modifiers::NONE);
        let _ = h.run_frame();
        h.inject_scroll(MouseWheelUnit::Line, Vec2::new(0.0, -1.0), Modifiers::NONE);
        let _ = h.run_frame();

        let written = h.runtime.input().written_str();
        assert!(
            written.contains("\x1b[A"),
            "expected Up scroll: {written:?}"
        );
        assert!(
            written.contains("\x1b[B"),
            "expected Down scroll: {written:?}"
        );
    }
}

// ===========================================================================
// F11 — Optional Feature: search (7 specs: 6 M3 + 1 M4)
// ===========================================================================

#[cfg(feature = "search")]
mod stage_m3_f11 {
    use egui_xterm_core::{CoreConfig, SearchQuery, Terminal};

    #[test]
    fn f11_t1_01_literal_single_match_visible_grid() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"alpha\r\nbeta gamma\r\ndelta\r\n");

        let query = SearchQuery::literal("beta");
        let matches = term.search(&query).expect("valid search");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 1);
        assert_eq!(matches[0].start_col, 0);
        assert_eq!(matches[0].end_col, 4);
    }

    #[test]
    fn f11_t1_02_literal_multi_match_across_scrollback_and_grid() {
        let cfg = CoreConfig {
            scrollback_limit: 100,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(40, 4, cfg);
        for i in 0..10 {
            term.advance(format!("line{i:02} needle\r\n").as_bytes());
        }

        let matches = term.search(&SearchQuery::literal("needle")).expect("valid");
        assert_eq!(matches.len(), 10);
        for (idx, m) in matches.iter().enumerate() {
            assert_eq!(m.line, idx);
            assert_eq!((m.start_col, m.end_col), (7, 13));
        }
    }

    #[test]
    fn f11_t1_03_regex_search_character_classes_and_quantifiers() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"err-404: not found\r\nok-200\r\nerr-503: unavailable\r\n");

        let matches = term
            .search(&SearchQuery::regex(r"err-\d{3}"))
            .expect("valid regex");
        assert_eq!(matches.len(), 2);
        assert_eq!(
            (matches[0].line, matches[0].start_col, matches[0].end_col),
            (0, 0, 7)
        );
        assert_eq!(
            (matches[1].line, matches[1].start_col, matches[1].end_col),
            (2, 0, 7)
        );
    }

    #[test]
    fn f11_t1_04_case_sensitive_vs_insensitive_search() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"Foo foo FOO\r\n");

        let sensitive = term
            .search(&SearchQuery::literal("foo").with_case_sensitive(true))
            .expect("sensitive");
        assert_eq!(sensitive.len(), 1);
        assert_eq!((sensitive[0].start_col, sensitive[0].end_col), (4, 7));

        let insensitive = term
            .search(&SearchQuery::literal("foo").with_case_sensitive(false))
            .expect("insensitive");
        assert_eq!(insensitive.len(), 3);
    }

    #[test]
    fn f11_t1_05_next_prev_match_navigation_scrollback_jump() {
        let cfg = CoreConfig {
            scrollback_limit: 100,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(40, 4, cfg);
        for i in 0..20 {
            let line = if i == 1 || i == 10 || i == 18 {
                format!("row {i} target\r\n")
            } else {
                format!("row {i} filler\r\n")
            };
            term.advance(line.as_bytes());
        }
        assert_eq!(term.display_offset(), 0);

        let matches = term
            .search(&SearchQuery::literal("target"))
            .expect("search");
        assert_eq!(matches.len(), 3);

        let m1 = *term.prev_search_match().expect("prev 1");
        assert_eq!(m1.line, 18);
        let m2 = *term.prev_search_match().expect("prev 2");
        assert_eq!(m2.line, 10);
        assert!(term.display_offset() > 0);
        let m3 = *term.next_search_match().expect("next 1");
        assert_eq!(m3.line, 18);
    }

    #[test]
    fn f11_t1_06_soft_wrapped_logical_line_search_span() {
        let mut term = Terminal::new(10, 5, CoreConfig::default());
        term.advance(b"abcdefghij12345");

        let matches = term.search(&SearchQuery::literal("hij12")).expect("search");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 0);
        assert_eq!(matches[0].start_col, 7);
        assert_eq!(matches[0].end_line, 1);
        assert_eq!(matches[0].end_col, 2);
    }
}

#[cfg(feature = "search")]
mod stage_m4_f11 {
    use egui_xterm_core::SearchQuery;
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn f11_t1_07_search_match_highlight_shapes_rendered() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("hello world hello\r\n");
        let base_outcome = h.run_frame();
        let base_inspection = base_outcome.inspect_shapes();

        let matches = h
            .runtime
            .terminal_mut()
            .search(&SearchQuery::literal("hello"))
            .expect("search");
        assert_eq!(matches.len(), 2);

        let hl_outcome = h.run_frame();
        let hl_inspection = hl_outcome.inspect_shapes();
        assert!(
            hl_inspection.rect_fills != base_inspection.rect_fills
                || hl_inspection.mesh_vertices_total > base_inspection.mesh_vertices_total,
            "expected search highlight geometry/colors to differ from baseline"
        );
    }
}

// ===========================================================================
// F12 — Optional Feature: hyperlinks (6 specs: 4 M3 + 2 M4)
// ===========================================================================

#[cfg(feature = "hyperlinks")]
mod stage_m3_f12 {
    use egui_xterm_core::{CoreConfig, Terminal};

    #[test]
    fn f12_t1_01_explicit_osc_8_hyperlink_with_st() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"\x1b]8;;https://example.com\x1b\\Click\x1b]8;;\x1b\\ plain");

        for col in 0..5 {
            assert_eq!(
                term.hyperlink_at(col, 0).map(|h| h.uri.as_str()),
                Some("https://example.com")
            );
        }
        for col in 5..11 {
            assert!(term.hyperlink_at(col, 0).is_none());
        }
    }

    #[test]
    fn f12_t1_02_explicit_osc_8_hyperlink_with_bel() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"\x1b]8;;https://rust-lang.org\x07Rust\x1b]8;;\x07");

        for col in 0..4 {
            assert_eq!(
                term.hyperlink_at(col, 0).map(|h| h.uri.as_str()),
                Some("https://rust-lang.org")
            );
        }
        assert!(term.hyperlink_at(4, 0).is_none());
    }

    #[test]
    fn f12_t1_03_osc_8_id_parameter_groups_wrapped_link() {
        let mut term = Terminal::new(10, 5, CoreConfig::default());
        term.advance(b"\x1b]8;id=grp1;https://example.org/docs\x1b\\LongLinkText\x1b]8;;\x1b\\");

        let first = term.hyperlink_at(0, 0).expect("row 0 link");
        let second = term.hyperlink_at(1, 1).expect("row 1 link");
        assert_eq!(first.id.as_deref(), Some("grp1"));
        assert_eq!(first.uri, "https://example.org/docs");
        assert_eq!(second.id.as_deref(), Some("grp1"));
        assert_eq!(second.uri, "https://example.org/docs");
    }

    #[test]
    fn f12_t1_04_implicit_http_https_url_auto_detection() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"Visit https://docs.rs/egui/latest for docs\r\n");

        assert_eq!(
            term.hyperlink_at(10, 0).map(|h| h.uri.as_str()),
            Some("https://docs.rs/egui/latest")
        );
        assert!(term.hyperlink_at(34, 0).is_none());
    }
}

#[cfg(feature = "hyperlinks")]
mod stage_m4_f12 {
    use egui::{CursorIcon, Modifiers, PointerButton};
    use egui_xterm::TerminalEvent;
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn f12_t1_05_hovering_hyperlink_updates_cursor_icon_and_underline() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime
            .input_mut()
            .push_str("\x1b]8;;https://example.com\x1b\\ClickHere!\x1b]8;;\x1b\\");
        let _ = h.run_frame();

        let p = h.cell_center(3, 0).expect("cell (3, 0)");
        h.inject_pointer_move(p);
        let outcome = h.run_frame();
        assert_eq!(
            outcome.platform_output.cursor_icon,
            CursorIcon::PointingHand
        );
    }

    #[test]
    fn f12_t1_06_clicking_hyperlink_emits_activated_event() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime
            .input_mut()
            .push_str("\x1b]8;;https://example.com\x1b\\Open\x1b]8;;\x1b\\");
        let _ = h.run_frame();

        let p = h.cell_center(2, 0).expect("cell (2, 0)");
        h.inject_click(p, PointerButton::Primary, Modifiers::NONE);
        let outcome = h.last_outcome().expect("outcome recorded");
        let activated = outcome.terminal_response.events.iter().any(|ev| {
            matches!(
                ev,
                TerminalEvent::HyperlinkActivated { uri, .. } if uri == "https://example.com"
            )
        }) || outcome
            .opened_urls()
            .iter()
            .any(|u| u == "https://example.com");
        assert!(activated, "expected hyperlink activation");
    }
}

// ===========================================================================
// F13 — Optional Feature: shell-integration (6 specs: 5 M3 + 1 M4)
// ===========================================================================

#[cfg(feature = "shell-integration")]
mod stage_m3_f13 {
    use egui_xterm_core::{CoreConfig, Terminal, TerminalEvent, ZoneKind};

    #[test]
    fn f13_t1_01_full_osc_133_lifecycle_tracks_semantic_zones() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07file1\r\nfile2\r\n\x1b]133;D;0\x07",
        );

        let zones = term.shell_zones();
        assert!(zones.iter().any(|z| z.kind == ZoneKind::Prompt));
        assert!(zones.iter().any(|z| z.kind == ZoneKind::Input));
        assert!(zones.iter().any(|z| z.kind == ZoneKind::Output));
    }

    #[test]
    fn f13_t1_02_nonzero_exit_code_recorded_in_d_marker() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07ok\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        term.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07bad\r\n\x1b]133;C\x07\x1b]133;D;127\x07");

        let events = term.drain_events();
        assert!(events.contains(&TerminalEvent::CommandFinished { exit_code: Some(0) }));
        assert!(events.contains(&TerminalEvent::CommandFinished {
            exit_code: Some(127)
        }));
    }

    #[test]
    fn f13_t1_03_jump_to_previous_and_next_prompt() {
        let cfg = CoreConfig {
            scrollback_limit: 100,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(80, 5, cfg);
        for i in 0..5 {
            term.advance(
                format!(
                    "\x1b]133;A\x07$ \x1b]133;B\x07cmd{i}\r\n\x1b]133;C\x07o1\r\no2\r\no3\r\no4\r\n\x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }
        assert_eq!(term.display_offset(), 0);
        assert!(term.jump_to_prompt(-1));
        let off1 = term.display_offset();
        assert!(off1 > 0);
        assert!(term.jump_to_prompt(-1));
        assert!(term.display_offset() > off1);
        assert!(term.jump_to_prompt(1));
    }

    #[test]
    fn f13_t1_04_command_output_extraction() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07lineA\r\nlineB\r\n\x1b]133;D;0\x07",
        );

        let output = term.command_output_at(1).expect("command output exists");
        assert_eq!(output, "lineA\nlineB");
    }

    #[test]
    fn f13_t1_05_multiline_prompt_with_st_and_c_params() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(
            b"\x1b]133;A;cl=m\x1b\\user@host\r\n> \x1b]133;B\x1b\\pwd\r\n\x1b]133;C;cmdline_url=pwd\x1b\\/home\r\n\x1b]133;D;0\x1b\\",
        );

        assert!(!term.shell_zones().is_empty());
        assert_eq!(term.command_output_at(2).as_deref(), Some("/home"));
    }
}

#[cfg(feature = "shell-integration")]
mod stage_m4_f13 {
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn f13_t1_06_prompt_gutter_exit_code_indicator_rendered() {
        let mut h_plain = HeadlessTermHarness::scripted();
        h_plain.runtime.input_mut().push_str("$ cmd\r\n");
        let plain = h_plain.run_frame().inspect_shapes();

        let mut h_ok = HeadlessTermHarness::scripted();
        h_ok.runtime
            .input_mut()
            .push_str("\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        let ok_shapes = h_ok.run_frame().inspect_shapes();

        let mut h_fail = HeadlessTermHarness::scripted();
        h_fail
            .runtime
            .input_mut()
            .push_str("\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07\x1b]133;D;2\x07");
        let fail_shapes = h_fail.run_frame().inspect_shapes();

        assert!(
            ok_shapes.rect_fills != plain.rect_fills
                || ok_shapes.mesh_vertices_total > plain.mesh_vertices_total
                || ok_shapes.line_count > plain.line_count
                || ok_shapes.path_count > plain.path_count
                || ok_shapes.mesh_colors() != plain.mesh_colors(),
            "expected OSC 133 D;0 prompt gutter indicator beyond plain baseline"
        );
        assert!(
            ok_shapes.rect_fills != fail_shapes.rect_fills
                || ok_shapes.mesh_vertices_total != fail_shapes.mesh_vertices_total
                || ok_shapes.line_count != fail_shapes.line_count
                || ok_shapes.path_count != fail_shapes.path_count
                || ok_shapes.mesh_colors() != fail_shapes.mesh_colors(),
            "expected D;0 and D;2 exit codes to produce distinct gutter geometry/colors"
        );
    }
}

// ===========================================================================
// F14 — Optional Feature: kitty-keyboard (6 specs: 2 M3 + 4 M4)
// ===========================================================================

#[cfg(feature = "kitty-keyboard")]
mod stage_m3_f14 {
    use egui_xterm_core::{CoreConfig, KittyKeyboardFlags, Terminal};

    #[test]
    fn f14_t1_01_push_and_query_kitty_keyboard_flags() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"\x1b[?u\x1b[>1u\x1b[?u\x1b[>5u\x1b[?u");

        assert_eq!(
            term.kitty_keyboard_flags(),
            KittyKeyboardFlags::from_bits_retain(5)
        );
        assert_eq!(term.drain_outgoing(), b"\x1b[?0u\x1b[?1u\x1b[?5u");
    }

    #[test]
    fn f14_t1_02_pop_restores_previous_stack_levels() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"\x1b[>1u\x1b[>3u\x1b[>7u\x1b[<u\x1b[?u\x1b[<2u\x1b[?u");

        assert_eq!(term.kitty_keyboard_flags(), KittyKeyboardFlags::empty());
        assert_eq!(term.drain_outgoing(), b"\x1b[?3u\x1b[?0u");
    }
}

#[cfg(feature = "kitty-keyboard")]
mod stage_m4_f14 {
    use egui::{Event, Key, Modifiers};
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn f14_t1_03_flag_1_disambiguate_ctrl_i_vs_tab_and_escape() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[>1u");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.tap_key(Key::Tab, Modifiers::NONE);
        h.tap_key(Key::I, Modifiers::CTRL);
        h.tap_key(Key::Escape, Modifiers::NONE);
        let _ = h.run_frame();

        let written = h.runtime.input().written_str();
        assert!(written.contains('\t'), "{written:?}");
        assert!(written.contains("\x1b[105;5u"), "{written:?}");
        assert!(written.contains("\x1b[27u"), "{written:?}");
    }

    #[test]
    fn f14_t1_04_flag_2_report_event_types_press_repeat_release() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[>3u");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_key(Key::A, true, Modifiers::CTRL);
        h.queue_event(Event::Key {
            key: Key::A,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: Modifiers::CTRL,
        });
        h.inject_key(Key::A, false, Modifiers::CTRL);
        let _ = h.run_frame();

        let written = h.runtime.input().written_str();
        assert!(
            written.contains("\x1b[97;5u") || written.contains("\x1b[97;5:1u"),
            "{written:?}"
        );
        assert!(written.contains("\x1b[97;5:2u"), "{written:?}");
        assert!(written.contains("\x1b[97;5:3u"), "{written:?}");
    }

    #[test]
    fn f14_t1_05_flag_4_and_8_alternate_and_all_keys_as_escape() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[>13u");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_key(Key::A, true, Modifiers::SHIFT);
        let _ = h.run_frame();

        let written = h.runtime.input().written_str();
        assert!(written.contains("\x1b[97:65;2u"), "{written:?}");
    }

    #[test]
    fn f14_t1_06_flag_16_associated_text_codepoints() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[>25u");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_key(Key::A, true, Modifiers::NONE);
        h.inject_text("a");
        let _ = h.run_frame();

        let written = h.runtime.input().written_str();
        assert!(
            written.contains("\x1b[97;1;97u") || written.contains("\x1b[97;;97u"),
            "{written:?}"
        );
    }
}

// ===========================================================================
// F15 — Optional Features: serde & metrics (6 specs: 2 M1 + 2 M3 + 2 M4)
// ===========================================================================

#[cfg(feature = "serde")]
mod stage_m1_f15 {
    use egui_xterm::TerminalSize;
    use egui_xterm_core::color::{ColorScheme, Theme};

    #[test]
    fn f15_t1_01_serde_roundtrip_terminal_size() {
        let size = TerminalSize {
            cols: 132,
            rows: 43,
            pixel_width: 1056,
            pixel_height: 860,
        };
        let json = serde_json::to_string(&size).expect("serialize TerminalSize");
        let decoded: TerminalSize = serde_json::from_str(&json).expect("deserialize TerminalSize");
        assert_eq!(decoded, size);
    }

    #[test]
    fn f15_t1_02_serde_roundtrip_theme_and_color_scheme() {
        let mut scheme = ColorScheme::light();
        scheme.foreground = [11, 22, 33];
        scheme.background = [240, 241, 242];
        scheme.cursor = [200, 100, 50];
        scheme.ansi[1] = [180, 10, 20];
        let theme = Theme {
            scheme,
            bold_is_bright: false,
        };

        let json = serde_json::to_string(&theme).expect("serialize Theme");
        let decoded: Theme = serde_json::from_str(&json).expect("deserialize Theme");
        assert_eq!(decoded, theme);
    }
}

#[cfg(feature = "serde")]
#[test]
fn f15_t1_03_serde_roundtrip_terminal_config_and_scale_mode() {
    use egui_xterm::{ScaleMode, TerminalConfig};

    let cfg = TerminalConfig {
        scale_mode: ScaleMode::AutoScaleToFit { cols: 80, rows: 24 },
        zoom_factor: 1.25,
        copy_on_ctrl_c_when_selected: false,
        read_budget_bytes_per_frame: 32_768,
        ..TerminalConfig::default()
    };

    let json = serde_json::to_string(&cfg).expect("serialize TerminalConfig");
    let decoded: TerminalConfig = serde_json::from_str(&json).expect("deserialize TerminalConfig");
    assert_eq!(decoded.scale_mode, cfg.scale_mode);
    assert!((decoded.zoom_factor - 1.25).abs() < 1e-6);
    assert!(!decoded.copy_on_ctrl_c_when_selected);
    assert_eq!(decoded.read_budget_bytes_per_frame, 32_768);
}

#[cfg(feature = "serde")]
#[test]
fn f15_t1_04_serde_roundtrip_terminal_snapshot() {
    use egui_xterm_core::{CoreConfig, PackedColor, Terminal, TerminalSnapshot};

    let mut term = Terminal::new(40, 10, CoreConfig::default());
    term.advance(b"\x1b[32mLine1\r\nLine2\x1b[2;3H");
    let snap = term.export_snapshot();

    let json = serde_json::to_string(&snap).expect("serialize TerminalSnapshot");
    let decoded: TerminalSnapshot =
        serde_json::from_str(&json).expect("deserialize TerminalSnapshot");
    let restored = Terminal::from_snapshot(decoded, CoreConfig::default());

    assert_eq!(restored.size(), (40, 10));
    assert_eq!(restored.visible_line_text(0), Some("Line1".to_owned()));
    assert_eq!(restored.visible_line_text(1), Some("Line2".to_owned()));
    assert_eq!(restored.cursor_pos(), (2, 1));
    let cell0 = restored.cell_at(0, 0).expect("cell (0, 0) exists");
    assert_eq!(cell0.ch, 'L');
    assert_eq!(cell0.fg, PackedColor::Indexed(2));
}

#[cfg(feature = "metrics")]
#[test]
fn f15_t1_05_metrics_byte_counter_increments_on_vt_ingestion() {
    use egui_xterm_core::{CoreConfig, Terminal};

    let mut term = Terminal::new(80, 24, CoreConfig::default());
    let payload = vec![b'A'; 4096];
    term.advance(&payload);
    assert!(term.metrics().bytes_processed >= 4096);
}

#[cfg(feature = "metrics")]
#[test]
fn f15_t1_06_metrics_frame_timing_during_runtime_show() {
    use egui_xterm_e2e::HeadlessTermHarness;

    let mut h = HeadlessTermHarness::scripted();
    for i in 0..5 {
        h.runtime.input_mut().push_str(format!("Frame {i}\r\n"));
        let _ = h.run_frame();
    }
    let metrics = h.runtime.terminal().metrics();
    assert_eq!(metrics.frames_rendered, 5);
    assert!(metrics.dirty_rows_rendered > 0);
}

// ===========================================================================
// F16 — egui Frame Auto-Layout & Auto-Scaling (6 specs, M4)
// ===========================================================================

mod stage_m4_f16 {
    use egui_xterm::{ScaleMode, TerminalConfig};
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn f16_t1_01_central_panel_auto_layout_computes_cols_rows() {
        let cfg = TerminalConfig {
            scale_mode: ScaleMode::ReflowGrid,
            font_size: 14.0,
            zoom_factor: 1.0,
            ..TerminalConfig::default()
        };

        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.set_viewport_size(800.0, 600.0);
        let outcome = h.run_frame();
        let resp = &outcome.terminal_response;

        assert!(resp.cols > 1);
        assert!(resp.rows > 1);
        assert!(f32::from(resp.cols) * resp.cell_size.x <= 800.0);
        assert!(f32::from(resp.rows) * resp.cell_size.y <= 600.0);
    }

    #[test]
    fn f16_t1_02_nested_frame_and_panel_padding_subtracted() {
        let mut h = HeadlessTermHarness::scripted();
        h.set_viewport_size(800.0, 600.0);
        let full = h.run_frame().terminal_response;

        let nested = h
            .run_frame_in_ui(|ui, runtime, config| {
                egui::Panel::left("nav")
                    .exact_size(200.0)
                    .show(ui, |_ui| {});
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.inner_margin(20.0))
                    .show(ui, |ui| runtime.show_with_config(ui, config))
                    .inner
            })
            .terminal_response;

        assert!(nested.cols < full.cols);
        assert!(nested.rows < full.rows);
        assert!(f32::from(nested.cols) * nested.cell_size.x <= 560.0);
        assert!(f32::from(nested.rows) * nested.cell_size.y <= 560.0);
    }

    #[test]
    fn f16_t1_03_dynamic_shrink_and_expand_triggers_reflow_and_resize() {
        let cfg = TerminalConfig {
            resize_debounce_secs: 0.0,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);

        h.set_viewport_size(800.0, 600.0);
        let r1 = h.run_frame().terminal_response;

        h.set_viewport_size(400.0, 300.0);
        let r2 = h.run_frame().terminal_response;
        assert!(r2.resized_this_frame);
        assert!(r2.cols < r1.cols);
        assert!(r2.rows < r1.rows);

        h.set_viewport_size(1000.0, 700.0);
        let r3 = h.run_frame().terminal_response;
        assert!(r3.resized_this_frame);
        assert!(r3.cols > r2.cols);
        assert!(r3.rows > r2.rows);
        assert!(h.runtime.input().recorded_resizes().len() >= 2);
    }

    #[test]
    fn f16_t1_04_auto_scale_to_fit_scales_font_size() {
        let cfg = TerminalConfig {
            scale_mode: ScaleMode::AutoScaleToFit { cols: 80, rows: 24 },
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);

        h.set_viewport_size(1280.0, 720.0);
        let big = h.run_frame().terminal_response;
        assert_eq!((big.cols, big.rows), (80, 24));

        h.set_viewport_size(640.0, 360.0);
        let small = h.run_frame().terminal_response;
        assert_eq!((small.cols, small.rows), (80, 24));
        assert!(small.effective_font_size < big.effective_font_size * 0.65);
    }

    #[test]
    fn f16_t1_05_resize_debounce_coalesces_rapid_frames() {
        let cfg = TerminalConfig {
            resize_debounce_secs: 0.1,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);

        h.set_viewport_size(800.0, 600.0);
        let _ = h.run_frame();
        h.runtime.input_mut().clear_resizes();

        h.advance_time(0.02);
        h.set_viewport_size(700.0, 600.0);
        let _ = h.run_frame();

        h.advance_time(0.03);
        h.set_viewport_size(600.0, 600.0);
        let target = h.run_frame().terminal_response;
        assert!(h.runtime.input().recorded_resizes().is_empty());

        h.advance_time(0.15);
        let _ = h.run_frame();
        let resizes = h.runtime.input().recorded_resizes();
        assert_eq!(resizes.len(), 1);
        assert_eq!(
            (resizes[0].cols, resizes[0].rows),
            (target.cols, target.rows)
        );
    }

    #[test]
    fn f16_t1_06_dpi_scaling_snaps_geometry_and_reports_role_terminal() {
        let cfg = TerminalConfig {
            resize_debounce_secs: 0.0,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.set_pixels_per_point(2.0);
        let outcome = h.run_frame();

        let cw_px = outcome.terminal_response.cell_size.x * 2.0;
        let ch_px = outcome.terminal_response.cell_size.y * 2.0;
        assert!((cw_px - cw_px.round()).abs() <= 1e-3);
        assert!((ch_px - ch_px.round()).abs() <= 1e-3);
        assert!(outcome.accesskit_roles().contains(&egui::Role::Terminal));
    }
}

// ===========================================================================
// F17 — 4-Stage <16ms egui Renderer & vector-glyphs (6 specs, M4)
// ===========================================================================

mod stage_m4_f17 {
    use egui::Color32;
    use egui_xterm_core::{CursorShape, Theme};
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn f17_t1_01_valid_clipped_shape_and_tessellator_output() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime
            .input_mut()
            .push_str("\x1b[31;44mHello\x1b[0m World\r\n");
        let outcome = h.run_frame();
        let inspection = outcome.inspect_shapes();
        assert!(inspection.contains_text("Hello"));

        let stats = h.render_to_mesh_stats();
        assert!(stats.primitive_count > 0);
        assert!(stats.total_vertices > 0);
        assert!(stats.total_indices > 0);
    }

    #[test]
    fn f17_t1_02_coalesced_adjacent_background_cells_reduce_geometry() {
        let mut h_uniform = HeadlessTermHarness::scripted();
        let spaces = " ".repeat(80);
        h_uniform
            .runtime
            .input_mut()
            .push_str(format!("\x1b[42m{spaces}"));
        let _ = h_uniform.run_frame();
        let uniform_stats = h_uniform.render_to_mesh_stats();

        let mut h_alt = HeadlessTermHarness::scripted();
        let mut alt_payload = String::new();
        for i in 0..80 {
            let bg = if i % 2 == 0 { 42 } else { 44 };
            alt_payload.push_str(&format!("\x1b[{bg}m "));
        }
        h_alt.runtime.input_mut().push_str(&alt_payload);
        let _ = h_alt.run_frame();
        let alt_stats = h_alt.render_to_mesh_stats();

        assert!(uniform_stats.total_vertices < alt_stats.total_vertices);
    }

    #[test]
    fn f17_t1_03_dirty_row_cache_stability_and_invalidation() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("line 0\r\nline 1\r\n");
        let f1 = h.run_frame().inspect_shapes();
        let f2 = h.run_frame().inspect_shapes();
        assert_eq!(f1.rendered_lines(), f2.rendered_lines());

        h.runtime.input_mut().push_str("\x1b[6;1Hline 5");
        let f3 = h.run_frame().inspect_shapes();
        assert!(f3.contains_text("line 5"));
    }

    #[test]
    fn f17_t1_04_cache_invalidation_on_theme_and_dpi_change() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("theme_check");
        let _ = h.run_frame();

        h.config.theme = Theme::light();
        let f2 = h.run_frame().inspect_shapes();
        let light_bg = Color32::from_rgb(
            h.config.theme.scheme.background[0],
            h.config.theme.scheme.background[1],
            h.config.theme.scheme.background[2],
        );
        assert!(f2.has_rect_or_mesh_color(light_bg));

        h.set_pixels_per_point(2.0);
        let f3 = h.run_frame().inspect_shapes();
        assert!(
            f3.text_entries
                .iter()
                .all(|e| (e.pixels_per_point - 2.0).abs() < 1e-6)
        );
    }

    #[cfg(feature = "vector-glyphs")]
    #[test]
    fn f17_t1_05_vector_glyphs_render_box_block_braille() {
        let mut h = HeadlessTermHarness::scripted();
        let base = h.run_frame().inspect_shapes();

        h.runtime.input_mut().push_str("┌──┐\r\n│██│\r\n└──┘\r\n⣿⡇");
        let outcome = h.run_frame();
        let inspection = outcome.inspect_shapes();
        assert!(
            inspection.line_count > base.line_count
                || inspection.path_count > base.path_count
                || inspection.rect_fills.len() > base.rect_fills.len()
                || inspection.mesh_vertices_total > base.mesh_vertices_total,
            "expected vector-glyph primitives beyond blank baseline"
        );
        let prims = h.tessellate_last_frame();
        assert!(!prims.is_empty());
    }

    #[test]
    fn f17_t1_06_underline_styles_and_cursor_shapes_render() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime
            .input_mut()
            .push_str("\x1b[4:1mS\x1b[4:2mD\x1b[4:3mC\x1b[4:4mT\x1b[4:5mL\x1b[0m");
        h.focus_terminal();

        h.config.cursor_shape = CursorShape::Block;
        let block = h.run_frame().inspect_shapes();
        h.config.cursor_shape = CursorShape::Underline;
        let underline = h.run_frame().inspect_shapes();
        h.config.cursor_shape = CursorShape::Bar;
        let beam = h.run_frame().inspect_shapes();

        assert!(
            block.line_count > 0
                || block.path_count > 0
                || block.rect_count > 0
                || block.mesh_vertices_total > 0
        );
        let block_verts = block.mesh_vertex_positions().to_vec();
        let underline_verts = underline.mesh_vertex_positions().to_vec();
        let beam_verts = beam.mesh_vertex_positions().to_vec();
        let block_sig = (
            block.rect_fills,
            block.mesh_vertices_total,
            block.line_count,
            block_verts,
        );
        let underline_sig = (
            underline.rect_fills,
            underline.mesh_vertices_total,
            underline.line_count,
            underline_verts,
        );
        let beam_sig = (
            beam.rect_fills,
            beam.mesh_vertices_total,
            beam.line_count,
            beam_verts,
        );
        assert_ne!(block_sig, underline_sig);
        assert_ne!(underline_sig, beam_sig);
        assert_ne!(block_sig, beam_sig);
    }
}

// ===========================================================================
// F18 — egui Focus, Input, IME & Clipboard (7 specs, M4)
// ===========================================================================

mod stage_m4_f18 {
    use egui::{Key, Modifiers, MouseWheelUnit, PointerButton, Vec2};
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn f18_t1_01_focus_and_event_filter_capture_tab_and_arrows() {
        let mut h = HeadlessTermHarness::scripted();
        let _ = h.run_frame_in_ui(|ui, runtime, config| {
            let _ = ui.button("other_focusable");
            runtime.show_with_config(ui, config)
        });
        let center = h
            .last_outcome()
            .expect("warm-up")
            .terminal_response
            .response
            .rect
            .center();
        h.inject_pointer_move(center);
        h.inject_pointer_button(center, PointerButton::Primary, true, Modifiers::NONE);
        h.inject_pointer_button(center, PointerButton::Primary, false, Modifiers::NONE);
        let _ = h.run_frame_in_ui(|ui, runtime, config| {
            let _ = ui.button("other_focusable");
            runtime.show_with_config(ui, config)
        });

        h.tap_key(Key::Tab, Modifiers::NONE);
        h.tap_key(Key::ArrowUp, Modifiers::NONE);
        let outcome = h.run_frame_in_ui(|ui, runtime, config| {
            let _ = ui.button("other_focusable");
            runtime.show_with_config(ui, config)
        });
        assert!(outcome.terminal_response.response.has_focus());
        assert_eq!(h.runtime.input().written_bytes(), b"\t\x1b[A");
    }

    #[test]
    fn f18_t1_02_event_text_and_key_translate_to_vt_sequences() {
        let mut h = HeadlessTermHarness::scripted();
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_text("ls -la");
        h.tap_key(Key::Enter, Modifiers::NONE);
        h.tap_key(Key::Backspace, Modifiers::NONE);
        h.tap_key(Key::Home, Modifiers::NONE);
        h.tap_key(Key::F5, Modifiers::NONE);
        let _ = h.run_frame();

        assert_eq!(
            h.runtime.input().written_bytes(),
            b"ls -la\r\x7f\x1b[H\x1b[15~"
        );
    }

    #[test]
    fn f18_t1_03_event_copy_copies_selection_without_etx() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("copy_me please\r\n");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        let from = h.cell_center(0, 0).expect("cell (0, 0)");
        let to = h.cell_center(6, 0).expect("cell (6, 0)");
        h.inject_drag_steps(from, to, PointerButton::Primary, Modifiers::NONE, 4);

        h.inject_copy();
        let outcome = h.run_frame();
        assert_eq!(outcome.copied_texts(), vec!["copy_me".to_owned()]);
        assert!(!h.runtime.input().written_bytes().contains(&0x03));
    }

    #[test]
    fn f18_t1_04_event_copy_without_selection_sends_etx() {
        let mut h = HeadlessTermHarness::scripted();
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_copy();
        let outcome = h.run_frame();
        assert_eq!(h.runtime.input().written_bytes(), b"\x03");
        assert!(outcome.copied_texts().is_empty());
    }

    #[test]
    fn f18_t1_05_event_paste_respects_bracketed_paste_mode() {
        let mut h = HeadlessTermHarness::scripted();
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_paste("hello\nworld");
        let _ = h.run_frame();
        let plain = h.runtime.input_mut().take_written();
        assert!(plain == b"hello\nworld" || plain == b"hello\rworld");

        h.runtime.input_mut().push_str("\x1b[?2004h");
        let _ = h.run_frame();
        h.inject_paste("hello\nworld");
        let _ = h.run_frame();
        assert_eq!(
            h.runtime.input().written_bytes(),
            b"\x1b[200~hello\nworld\x1b[201~"
        );
    }

    #[test]
    fn f18_t1_06_ime_composition_and_platform_output_ime_rect() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[3;6H");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        let f1 = h.run_frame();
        assert!(f1.platform_output.ime.is_some());

        h.inject_ime_preedit("nihao", Some(0..5));
        let _ = h.run_frame();
        assert!(h.runtime.input().written_bytes().is_empty());

        h.inject_ime_commit("你好");
        let _ = h.run_frame();
        assert_eq!(h.runtime.input().written_bytes(), "你好".as_bytes());
    }

    #[test]
    fn f18_t1_07_pointer_drag_settled_and_sgr_mouse_reporting() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("select here\r\n");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        let from = h.cell_center(0, 0).expect("cell (0, 0)");
        let to = h.cell_center(5, 0).expect("cell (5, 0)");
        h.inject_drag_steps(from, to, PointerButton::Primary, Modifiers::NONE, 3);
        let settled = h.last_outcome().expect("drag release").settled_selections();
        assert!(settled.iter().any(|s| s == "select"));

        h.runtime.input_mut().push_str("\x1b[?1000;1006h");
        let _ = h.run_frame();
        let cell = h.cell_center(4, 3).expect("cell (4, 3)");
        h.inject_click(cell, PointerButton::Primary, Modifiers::NONE);
        h.inject_scroll(MouseWheelUnit::Line, Vec2::new(0.0, 1.0), Modifiers::NONE);
        let _ = h.run_frame();

        let written = h.runtime.input().written_str();
        assert!(written.contains("\x1b[<0;5;4M\x1b[<0;5;4m"), "{written:?}");
        assert!(
            written.contains("\x1b[<64;5;4M") || written.contains("\x1b[<65;5;4M"),
            "{written:?}"
        );
    }
}

// ===========================================================================
// F19 — Performance & Frame Latency Contracts (6 specs: 2 ungated M2 + 4 M4)
// ===========================================================================

mod stage_m2_f19 {
    use std::time::Instant;

    use egui_xterm_core::{CoreConfig, Terminal};

    #[test]
    fn f19_t1_01_high_throughput_ascii_vt_stream_ingestion() {
        let mut term = Terminal::new(120, 40, CoreConfig::default());
        let line =
            b"\x1b[32m0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ\x1b[0m\r\n";
        let mut buf = Vec::with_capacity(10 * 1024 * 1024 + line.len());
        while buf.len() < 10 * 1024 * 1024 {
            buf.extend_from_slice(line);
        }

        let start = Instant::now();
        for chunk in buf.chunks(65_536) {
            term.advance(chunk);
        }
        let elapsed = start.elapsed();
        assert!(term.scrollback_len() > 0);
        assert!(elapsed.as_secs_f64() < 30.0, "took {elapsed:?}");
    }

    #[test]
    fn f19_t1_04_scrollback_growth_o1_append() {
        let cfg = CoreConfig {
            scrollback_limit: 10_000,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(80, 24, cfg);
        for i in 0..500 {
            term.advance(format!("warmup line {i:04}\r\n").as_bytes());
        }

        let t1_start = Instant::now();
        for i in 0..5_000 {
            term.advance(format!("batch1 line {i:04}\r\n").as_bytes());
        }
        let t1 = t1_start.elapsed();

        let t2_start = Instant::now();
        for i in 0..5_000 {
            term.advance(format!("batch2 line {i:04}\r\n").as_bytes());
        }
        let t2 = t2_start.elapsed();

        assert_eq!(term.scrollback_len(), 10_000);
        let slack = std::time::Duration::from_millis(5);
        assert!(t2 <= t1 * 3 + slack, "t1={t1:?}, t2={t2:?}");
    }
}

mod stage_m4_f19 {
    use std::fmt::Write as _;
    use std::time::Instant;

    use egui_xterm::{TerminalConfig, TerminalInput, TerminalIoError, TerminalSize};
    use egui_xterm_e2e::{HeadlessTermHarness, ScriptedInput};

    struct ByteCountingInput {
        inner: ScriptedInput,
        total_read: usize,
    }

    impl TerminalInput for ByteCountingInput {
        fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
            let n = self.inner.poll_read(buf)?;
            self.total_read += n;
            Ok(n)
        }

        fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
            self.inner.write_bytes(bytes)
        }

        fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalIoError> {
            self.inner.resize(size)
        }

        fn is_closed(&self) -> bool {
            self.inner.is_closed()
        }
    }

    #[test]
    fn f19_t1_02_bounded_read_budget_prevents_single_frame_stall() {
        let cfg = TerminalConfig {
            read_budget_bytes_per_frame: 65_536,
            ..TerminalConfig::default()
        };
        let mut inner = ScriptedInput::new();
        let payload = "A".repeat(1_048_576);
        inner.push_chunked(payload.as_bytes(), 16_384);

        let mut h = HeadlessTermHarness::with_config(
            ByteCountingInput {
                inner,
                total_read: 0,
            },
            cfg,
        );

        let _ = h.run_frame();
        let after_frame1 = h.runtime.input().total_read;
        assert!(
            after_frame1 > 0 && after_frame1 <= 65_536,
            "after_frame1={after_frame1}"
        );

        let mut frames = 1usize;
        while h.runtime.input().total_read < 1_048_576 && frames < 32 {
            let _ = h.run_frame();
            frames += 1;
        }
        assert_eq!(h.runtime.input().total_read, 1_048_576);
        assert!(frames >= 16, "drained too fast in {frames} frames");
    }

    #[test]
    fn f19_t1_03_single_frame_dense_sgr_screen_latency() {
        let mut h = HeadlessTermHarness::scripted();
        h.set_viewport_size(1200.0, 800.0);
        let init_resp = h.run_frame().terminal_response;
        let total_rows = usize::from(init_resp.rows).min(40);
        let total_cols = usize::from(init_resp.cols).min(120);

        let mut dense = String::new();
        for row in 0..total_rows {
            for col in 0..total_cols {
                let _ = write!(
                    dense,
                    "\x1b[38;2;{};{};{}mX",
                    (col * 2) % 255,
                    (row * 6) % 255,
                    128
                );
            }
            if row + 1 < total_rows {
                dense.push_str("\r\n");
            }
        }
        h.runtime.input_mut().push_str(&dense);
        let drained = h.run_until(16, 1.0 / 60.0, |harness, _| {
            harness.runtime.input().is_drained()
        });
        assert!(
            drained,
            "dense SGR payload did not drain within 16 warm-up frames"
        );

        // Update 20% of the screen (8 rows on a 40-row screen).
        let update_rows = (total_rows / 5).max(8).min(total_rows);
        let mut patch = String::new();
        for row in 1..=update_rows {
            let _ = write!(patch, "\x1b[{row};1HUPDATED_ROW_{row:02}");
        }
        h.runtime.input_mut().push_str(&patch);

        let start = Instant::now();
        let _ = h.run_frame();
        let prims = h.tessellate_last_frame();
        let elapsed = start.elapsed();

        assert!(!prims.is_empty());
        #[cfg(not(debug_assertions))]
        assert!(
            elapsed.as_millis() < 16,
            "release frame latency exceeded 16ms: {elapsed:?}"
        );
        #[cfg(debug_assertions)]
        assert!(
            elapsed.as_millis() < 2000,
            "debug frame latency exploded: {elapsed:?}"
        );
    }

    #[test]
    fn f19_t1_05_multiframe_steady_state_scroll_churn() {
        let cfg = TerminalConfig {
            scrollback_limit: 500,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);

        for frame in 0..200 {
            let mut batch = String::new();
            for line in 0..100 {
                let _ = writeln!(batch, "churn {frame}:{line}\r");
            }
            h.runtime.input_mut().push_str(&batch);
            let _ = h.run_frame();
            assert!(h.runtime.terminal().scrollback_len() <= 500);
        }
    }

    #[test]
    fn f19_t1_06_render_and_tessellation_scaling_smoke() {
        let mut h = HeadlessTermHarness::scripted();
        h.set_viewport_size(800.0, 600.0);
        for row in 0..24 {
            h.runtime
                .input_mut()
                .push_str(format!("\x1b[34mRow {row:02} content\x1b[0m\r\n"));
        }
        let _ = h.run_frame();
        let stats_small = h.render_to_mesh_stats();
        assert!(stats_small.primitive_count > 0);

        h.runtime.input_mut().push_str("\x1b[1;1HONE_ROW_UPDATE");
        let _ = h.run_frame();
        let stats_one = h.render_to_mesh_stats();
        assert!(stats_one.primitive_count > 0);

        h.set_viewport_size(1200.0, 900.0);
        let _ = h.run_frame();
        let stats_large = h.render_to_mesh_stats();
        assert!(stats_large.total_vertices >= stats_small.total_vertices);
    }
}

// ===========================================================================
// F20 — Native PTY Backend & pty Feature (6 specs, M5 desktop-only)
// ===========================================================================

#[cfg(all(not(target_arch = "wasm32"), feature = "pty"))]
mod stage_m5_f20 {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use egui_xterm::pty::{PtyBackend, PtyConfig};
    use egui_xterm::{TerminalInput, TerminalSize};

    fn poll_until(
        backend: &mut PtyBackend,
        timeout: Duration,
        mut pred: impl FnMut(&str, &PtyBackend) -> bool,
    ) -> (String, bool) {
        let deadline = Instant::now() + timeout;
        let mut captured = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match backend.poll_read(&mut buf) {
                Ok(0) => {}
                Ok(n) => captured.extend_from_slice(&buf[..n]),
                Err(_) => {}
            }
            let text = String::from_utf8_lossy(&captured).into_owned();
            if pred(&text, backend) {
                return (text, true);
            }
            if Instant::now() >= deadline {
                return (text, false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn f20_t1_01_spawn_short_command_and_read_stdout() {
        #[cfg(windows)]
        let (shell, args) = (
            "cmd".to_owned(),
            vec!["/C".to_owned(), "echo EGUI_XTERM_OK".to_owned()],
        );
        #[cfg(not(windows))]
        let (shell, args) = (
            "sh".to_owned(),
            vec!["-c".to_owned(), "echo EGUI_XTERM_OK".to_owned()],
        );

        let mut backend = PtyBackend::spawn(PtyConfig {
            shell: Some(shell),
            args,
            cwd: None,
            env: Vec::new(),
            initial_size: TerminalSize::new(80, 24),
        })
        .expect("spawn PTY");

        let (text, ok) = poll_until(&mut backend, Duration::from_secs(5), |s, _| {
            s.contains("EGUI_XTERM_OK")
        });
        assert!(ok, "missing marker in PTY output: {text:?}");
    }

    #[test]
    fn f20_t1_02_bidirectional_io_with_interactive_shell() {
        let mut backend = PtyBackend::spawn(PtyConfig {
            shell: None,
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            initial_size: TerminalSize::new(80, 24),
        })
        .expect("spawn interactive shell");

        backend
            .write_bytes(b"echo ROUNDTRIP_123\r")
            .expect("write command");
        let (text, ok) = poll_until(&mut backend, Duration::from_secs(5), |s, _| {
            s.contains("ROUNDTRIP_123")
        });
        let _ = backend.write_bytes(b"exit\r");
        assert!(ok, "missing ROUNDTRIP_123: {text:?}");
    }

    #[test]
    fn f20_t1_03_pty_config_cwd_sets_working_directory() {
        let tmp_dir = std::env::temp_dir().canonicalize().expect("canonical temp");
        #[cfg(windows)]
        let (shell, args) = ("cmd".to_owned(), vec!["/C".to_owned(), "cd".to_owned()]);
        #[cfg(not(windows))]
        let (shell, args) = ("sh".to_owned(), vec!["-c".to_owned(), "pwd".to_owned()]);

        let mut backend = PtyBackend::spawn(PtyConfig {
            shell: Some(shell),
            args,
            cwd: Some(tmp_dir.clone()),
            env: Vec::new(),
            initial_size: TerminalSize::new(120, 24),
        })
        .expect("spawn PTY with cwd");

        let folder_name = tmp_dir
            .file_name()
            .and_then(|s| s.to_str())
            .expect("temp dir name");
        let (text, ok) = poll_until(&mut backend, Duration::from_secs(5), |s, _| {
            s.to_ascii_lowercase()
                .contains(&folder_name.to_ascii_lowercase())
        });
        assert!(ok, "cwd {folder_name:?} missing in {text:?}");
    }

    #[test]
    fn f20_t1_04_pty_config_env_injects_variables() {
        #[cfg(windows)]
        let (shell, args) = (
            "cmd".to_owned(),
            vec!["/C".to_owned(), "echo %EGUI_XTERM_VAR%".to_owned()],
        );
        #[cfg(not(windows))]
        let (shell, args) = (
            "sh".to_owned(),
            vec!["-c".to_owned(), "echo $EGUI_XTERM_VAR".to_owned()],
        );

        let mut backend = PtyBackend::spawn(PtyConfig {
            shell: Some(shell),
            args,
            cwd: None,
            env: vec![("EGUI_XTERM_VAR".to_owned(), "VAL_42".to_owned())],
            initial_size: TerminalSize::new(80, 24),
        })
        .expect("spawn PTY with env");

        let (text, ok) = poll_until(&mut backend, Duration::from_secs(5), |s, _| {
            s.contains("VAL_42")
        });
        assert!(ok, "missing VAL_42 in {text:?}");
    }

    #[test]
    fn f20_t1_05_pty_backend_resize_updates_child_dimensions() {
        #[cfg(windows)]
        let (shell, args) = (
            "powershell.exe".to_owned(),
            vec!["-NoLogo".to_owned(), "-NoProfile".to_owned()],
        );
        #[cfg(not(windows))]
        let (shell, args) = ("sh".to_owned(), Vec::new());

        let mut backend = PtyBackend::spawn(PtyConfig {
            shell: Some(shell),
            args,
            cwd: None,
            env: Vec::new(),
            initial_size: TerminalSize::new(80, 24),
        })
        .expect("spawn shell for resize");

        #[cfg(windows)]
        {
            let (boot_text, ready) = poll_until(&mut backend, Duration::from_secs(5), |s, _| {
                s.contains("PS ") || s.contains('>')
            });
            assert!(
                ready,
                "PowerShell prompt did not become ready before resize: {boot_text:?}"
            );
        }

        backend
            .resize(TerminalSize::with_pixels(100, 35, 800, 700))
            .expect("resize succeeds");

        #[cfg(windows)]
        let probe = b"([string][Console]::WindowWidth + 'x' + [Console]::WindowHeight)\r";
        #[cfg(not(windows))]
        let probe = b"stty size\r";

        backend.write_bytes(probe).expect("write size probe");
        #[cfg(windows)]
        let expected = "100x35";
        #[cfg(not(windows))]
        let expected = "35 100";

        let (text, ok) = poll_until(&mut backend, Duration::from_secs(8), |s, _| {
            s.contains(expected)
        });
        let _ = backend.write_bytes(b"exit\r");
        assert!(ok, "expected {expected:?} in {text:?}");
    }

    #[test]
    fn f20_t1_06_child_exit_transitions_is_closed_and_triggers_repaint() {
        let repaints = Arc::new(AtomicUsize::new(0));
        let repaints_cb = Arc::clone(&repaints);

        #[cfg(windows)]
        let (shell, args) = (
            "cmd".to_owned(),
            vec!["/C".to_owned(), "echo DONE && exit 0".to_owned()],
        );
        #[cfg(not(windows))]
        let (shell, args) = (
            "sh".to_owned(),
            vec!["-c".to_owned(), "echo DONE; exit 0".to_owned()],
        );

        let mut backend = PtyBackend::spawn_with_repaint(
            PtyConfig {
                shell: Some(shell),
                args,
                cwd: None,
                env: Vec::new(),
                initial_size: TerminalSize::new(80, 24),
            },
            Box::new(move || {
                repaints_cb.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .expect("spawn PTY with repaint");

        let (_, ok) = poll_until(&mut backend, Duration::from_secs(5), |_, b| b.is_closed());
        assert!(ok, "PTY did not transition to is_closed()");
        assert!(repaints.load(Ordering::Relaxed) >= 1);
    }
}

// ===========================================================================
// F21 — Multi-Tab / Multi-Pane Integration (6 specs: 5 M4 + 1 M5)
// ===========================================================================

mod stage_m4_f21 {
    use egui::Color32;
    use egui_xterm::{ChannelInput, RingBufferInput, ScaleMode, TerminalRuntime};
    use egui_xterm_core::Theme;
    use egui_xterm_e2e::{HeadlessTermHarness, MultiPaneHarness, ScriptedInput};

    #[test]
    fn f21_t1_01_two_runtimes_distinct_input_types_zero_crosstalk() {
        let (tx, ch_input) = ChannelInput::pair();
        let ring = RingBufferInput::from_bytes(b"PANE_B");
        tx.send(b"PANE_A".to_vec()).expect("send pane A");

        let mut panes = MultiPaneHarness::new(ch_input, ring);
        let _ = panes.run_split_frame();

        assert_eq!(
            panes.left.terminal().visible_line_text(0).as_deref(),
            Some("PANE_A")
        );
        assert_eq!(
            panes.right.terminal().visible_line_text(0).as_deref(),
            Some("PANE_B")
        );
    }

    #[test]
    fn f21_t1_02_independent_focus_switching_between_panes() {
        let mut panes = MultiPaneHarness::new(ScriptedInput::new(), ScriptedInput::new());
        let _ = panes.run_split_frame();

        // Click left pane (100, 100) then type "aaa".
        panes.queue_event(egui::Event::PointerMoved(egui::pos2(100.0, 100.0)));
        panes.queue_event(egui::Event::PointerButton {
            pos: egui::pos2(100.0, 100.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        panes.queue_event(egui::Event::PointerButton {
            pos: egui::pos2(100.0, 100.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = panes.run_split_frame();
        panes.queue_event(egui::Event::Text("aaa".to_owned()));
        let _ = panes.run_split_frame();

        // Click right pane (600, 100) then type "bbb".
        panes.queue_event(egui::Event::PointerMoved(egui::pos2(600.0, 100.0)));
        panes.queue_event(egui::Event::PointerButton {
            pos: egui::pos2(600.0, 100.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        panes.queue_event(egui::Event::PointerButton {
            pos: egui::pos2(600.0, 100.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = panes.run_split_frame();
        panes.queue_event(egui::Event::Text("bbb".to_owned()));
        let _ = panes.run_split_frame();

        assert_eq!(panes.left.input().written_bytes(), b"aaa");
        assert_eq!(panes.right.input().written_bytes(), b"bbb");
    }

    #[test]
    fn f21_t1_03_independent_auto_layout_sizes_asymmetric_split() {
        let mut panes = MultiPaneHarness::new(ScriptedInput::new(), ScriptedInput::new());
        panes.set_viewport_size(800.0, 600.0);
        panes.set_split_fraction(0.30);
        let (resp_a, resp_b, _) = panes.run_split_frame();

        assert!(resp_b.cols > resp_a.cols);
        assert_eq!(resp_a.rows, resp_b.rows);
    }

    #[test]
    fn f21_t1_04_independent_theme_and_scale_mode_per_pane() {
        let mut panes = MultiPaneHarness::new(ScriptedInput::new(), ScriptedInput::new());
        panes.left_config.theme = Theme::dark();
        panes.left_config.scale_mode = ScaleMode::ReflowGrid;
        panes.right_config.theme = Theme::light();
        panes.right_config.scale_mode = ScaleMode::AutoScaleToFit { cols: 40, rows: 12 };

        let (resp_a, resp_b, meta) = panes.run_split_frame();
        assert_eq!((resp_b.cols, resp_b.rows), (40, 12));
        assert_ne!((resp_a.cols, resp_a.rows), (40, 12));

        let inspection = meta.inspect_shapes();
        let dark_bg = Theme::dark().scheme.background;
        let light_bg = Theme::light().scheme.background;
        assert!(
            inspection
                .has_rect_or_mesh_color(Color32::from_rgb(dark_bg[0], dark_bg[1], dark_bg[2]))
        );
        assert!(inspection.has_rect_or_mesh_color(Color32::from_rgb(
            light_bg[0],
            light_bg[1],
            light_bg[2]
        )));
    }

    #[test]
    fn f21_t1_05_multitab_switching_preserves_hidden_tab_state() {
        let mut tab0 = HeadlessTermHarness::scripted();
        let mut tab1 = TerminalRuntime::new(ScriptedInput::from_bytes(b"TAB1"));
        tab0.runtime.input_mut().push_str("TAB0");
        let _ = tab0.run_frame();

        let _ = egui_xterm_e2e::run_headless_ui(
            &tab0.ctx,
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)),
            1.0,
            0.1,
            true,
            Vec::new(),
            |ui| {
                let _ = tab1.show(ui);
            },
        );
        assert_eq!(
            tab1.terminal().visible_line_text(0).as_deref(),
            Some("TAB1")
        );

        let _ = tab0.run_frame();
        assert_eq!(
            tab0.runtime.terminal().visible_line_text(0).as_deref(),
            Some("TAB0")
        );
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "pty"))]
#[test]
fn f21_t1_06_mixed_custom_trait_and_pty_pane_in_single_context() {
    use std::time::{Duration, Instant};

    use egui_xterm::pty::{PtyBackend, PtyConfig};
    use egui_xterm::{ChannelInput, TerminalSize};
    use egui_xterm_e2e::MultiPaneHarness;

    let (tx, ch_input) = ChannelInput::pair();
    tx.send(b"TRAIT_TAB_OK".to_vec()).expect("send trait");

    #[cfg(windows)]
    let (shell, args) = (
        "cmd".to_owned(),
        vec!["/C".to_owned(), "echo PTY_TAB_OK".to_owned()],
    );
    #[cfg(not(windows))]
    let (shell, args) = (
        "sh".to_owned(),
        vec!["-c".to_owned(), "echo PTY_TAB_OK".to_owned()],
    );

    let pty_backend = PtyBackend::spawn(PtyConfig {
        shell: Some(shell),
        args,
        cwd: None,
        env: Vec::new(),
        initial_size: TerminalSize::new(80, 24),
    })
    .expect("spawn PTY pane");

    let mut panes = MultiPaneHarness::new(ch_input, pty_backend);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut found = false;
    while Instant::now() < deadline {
        panes.advance_time(1.0 / 60.0);
        let _ = panes.run_split_frame();
        if panes
            .right
            .terminal()
            .visible_lines()
            .iter()
            .any(|l| l.contains("PTY_TAB_OK"))
        {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(
        panes.left.terminal().visible_line_text(0).as_deref(),
        Some("TRAIT_TAB_OK")
    );
    assert!(found, "PTY pane did not render PTY_TAB_OK before timeout");
}
