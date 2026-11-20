//! Tier 2 — Boundary, Corner, and Negative E2E Tests (`F2`–`F21`, 123 specs / 125 tests).
//!
//! Exercises core APIs (`egui_xterm_core::{input, parser, color, grid, term}`),
//! optional feature gates (`search`, `hyperlinks`, `shell-integration`, `kitty-keyboard`,
//! `serde`, `metrics`, `vector-glyphs`, `native-clipboard`, `pty`), and `egui_xterm`
//! headless runtime/rendering boundary conditions.

// ===========================================================================
// F2 — TerminalInput Trait & Adapters Boundaries (6 specs, ungated M1)
// ===========================================================================

mod stage_m1_f2 {
    use egui_xterm::{
        ChannelInput, FnInput, RingBufferInput, TerminalInput, TerminalIoError, TerminalSize,
    };

    struct CustomErrorInput {
        read_err: TerminalIoError,
        write_err: TerminalIoError,
    }

    impl TerminalInput for CustomErrorInput {
        fn poll_read(&mut self, _buf: &mut [u8]) -> Result<usize, TerminalIoError> {
            Err(self.read_err.clone())
        }

        fn write_bytes(&mut self, _bytes: &[u8]) -> Result<(), TerminalIoError> {
            Err(self.write_err.clone())
        }
    }

    struct ClosingAfterPayload {
        payload: &'static [u8],
        pos: usize,
        closed: bool,
    }

    impl ClosingAfterPayload {
        const fn new(payload: &'static [u8]) -> Self {
            Self {
                payload,
                pos: 0,
                closed: false,
            }
        }
    }

    impl TerminalInput for ClosingAfterPayload {
        fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
            if buf.is_empty() {
                return if self.closed {
                    Err(TerminalIoError::Closed)
                } else {
                    Ok(0)
                };
            }
            if self.pos >= self.payload.len() {
                self.closed = true;
                return Err(TerminalIoError::Closed);
            }
            let remaining = &self.payload[self.pos..];
            let n = remaining.len().min(buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            self.pos += n;
            if self.pos >= self.payload.len() {
                self.closed = true;
            }
            Ok(n)
        }

        fn is_closed(&self) -> bool {
            self.closed
        }
    }

    #[test]
    fn t2_f2_01_zero_length_read_buffer() {
        // ChannelInput
        let (tx, mut ch) = ChannelInput::pair();
        tx.send(b"0123456789".to_vec()).expect("send 10 bytes");
        assert_eq!(ch.poll_read(&mut []), Ok(0));
        let mut buf = [0u8; 16];
        assert_eq!(ch.poll_read(&mut buf), Ok(10));
        assert_eq!(&buf[..10], b"0123456789");

        // RingBufferInput
        let mut ring = RingBufferInput::from_bytes(b"0123456789");
        assert_eq!(ring.poll_read(&mut []), Ok(0));
        assert_eq!(ring.pending_len(), 10);
        let mut buf = [0u8; 16];
        assert_eq!(ring.poll_read(&mut buf), Ok(10));
        assert_eq!(&buf[..10], b"0123456789");

        // FnInput
        let data = b"0123456789";
        let mut pos = 0usize;
        let mut fn_in = FnInput::new(|out: &mut [u8]| {
            if out.is_empty() || pos >= data.len() {
                return Ok(0);
            }
            let n = (data.len() - pos).min(out.len());
            out[..n].copy_from_slice(&data[pos..pos + n]);
            pos += n;
            Ok(n)
        });
        assert_eq!(fn_in.poll_read(&mut []), Ok(0));
        let mut buf = [0u8; 16];
        assert_eq!(fn_in.poll_read(&mut buf), Ok(10));
        assert_eq!(&buf[..10], b"0123456789");
    }

    #[test]
    fn t2_f2_02_partial_read_smaller_buffer() {
        let (tx, mut ch) = ChannelInput::pair();
        tx.send(b"0123456789AB".to_vec()).expect("send 12 bytes");
        let mut ring = RingBufferInput::from_bytes(b"0123456789AB");

        for source in [&mut ch as &mut dyn TerminalInput, &mut ring] {
            let mut buf4 = [0u8; 4];
            assert_eq!(source.poll_read(&mut buf4), Ok(4));
            assert_eq!(&buf4, b"0123");
            assert_eq!(source.poll_read(&mut buf4), Ok(4));
            assert_eq!(&buf4, b"4567");
            assert_eq!(source.poll_read(&mut buf4), Ok(4));
            assert_eq!(&buf4, b"89AB");
            assert_eq!(source.poll_read(&mut buf4), Ok(0));
        }
    }

    #[test]
    fn t2_f2_03_closed_stream_terminal_io_error_closed() {
        let mut ring = RingBufferInput::from_bytes(b"ok");
        ring.close();
        assert!(!ring.is_closed());

        let mut buf = [0u8; 8];
        assert_eq!(ring.poll_read(&mut buf), Ok(2));
        assert_eq!(&buf[..2], b"ok");
        assert!(ring.is_closed());
        assert_eq!(ring.poll_read(&mut buf), Err(TerminalIoError::Closed));
        assert_eq!(ring.poll_read(&mut buf), Err(TerminalIoError::Closed));

        let mut custom = ClosingAfterPayload::new(b"ok");
        assert!(!custom.is_closed());
        assert_eq!(custom.poll_read(&mut buf), Ok(2));
        assert_eq!(&buf[..2], b"ok");
        assert!(custom.is_closed());
        let err = custom.poll_read(&mut buf).expect_err("must be closed");
        assert!(err.is_closed());
        assert_eq!(custom.poll_read(&mut buf), Err(TerminalIoError::Closed));
    }

    #[test]
    fn t2_f2_04_descriptive_io_error_message() {
        let mut src = CustomErrorInput {
            read_err: TerminalIoError::Message("EPIPE".to_owned()),
            write_err: TerminalIoError::Message("EPIPE".to_owned()),
        };
        let mut buf = [0u8; 8];
        let read_err = src.poll_read(&mut buf).expect_err("poll_read error");
        let write_err = src.write_bytes(b"x").expect_err("write_bytes error");

        assert_eq!(read_err, TerminalIoError::Message("EPIPE".to_owned()));
        assert_eq!(write_err, TerminalIoError::Message("EPIPE".to_owned()));
        assert!(!read_err.is_closed());
        assert_eq!(read_err.to_string(), "EPIPE");
        assert!(format!("{read_err:?}").contains("EPIPE"));
    }

    #[test]
    fn t2_f2_05_ring_buffer_empty_push_growth_post_close_and_hooks() {
        let mut ring = RingBufferInput::with_capacity(8);
        ring.push(b"");
        assert_eq!(ring.pending_len(), 0);

        ring.push(b"123456");
        let mut buf4 = [0u8; 4];
        assert_eq!(ring.poll_read(&mut buf4), Ok(4));
        assert_eq!(&buf4, b"1234");

        ring.push(b"abcdefghij");
        assert_eq!(ring.pending_len(), 12);

        let mut buf32 = [0u8; 32];
        assert_eq!(ring.poll_read(&mut buf32), Ok(12));
        assert_eq!(&buf32[..12], b"56abcdefghij");
        assert_eq!(ring.pending_len(), 0);

        ring.close();
        assert_eq!(ring.write_bytes(b"x"), Err(TerminalIoError::Closed));

        let mut hooked = RingBufferInput::new()
            .with_write_hook(|bytes| Err(TerminalIoError::message(format!("w:{}", bytes.len()))))
            .with_resize_hook(|sz| {
                Err(TerminalIoError::message(format!(
                    "r:{}x{}",
                    sz.cols, sz.rows
                )))
            });
        assert_eq!(
            hooked.write_bytes(b"abc"),
            Err(TerminalIoError::Message("w:3".to_owned()))
        );
        let target_sz = TerminalSize::new(100, 30);
        assert_eq!(
            hooked.resize(target_sz),
            Err(TerminalIoError::Message("r:100x30".to_owned()))
        );
        assert_eq!(hooked.last_size(), Some(target_sz));
    }

    #[test]
    fn t2_f2_06_channel_input_sender_dropped() {
        let (tx, mut input) = ChannelInput::pair();
        tx.send(b"last".to_vec()).expect("send payload");
        drop(tx);

        let mut buf = [0u8; 16];
        assert_eq!(input.poll_read(&mut buf), Ok(4));
        assert_eq!(&buf[..4], b"last");
        assert!(input.is_closed());
        assert_eq!(input.poll_read(&mut buf), Err(TerminalIoError::Closed));
    }
}

// ===========================================================================
// F3 — VT500 Parser & memchr Fast-Path Boundaries (7 specs, ungated M1+M2)
// ===========================================================================

mod stage_m1_f3 {
    use std::fmt::Write as _;

    use egui_xterm_core::parser::{
        C1Policy, CsiParams, DEFAULT_MAX_OSC_BYTES, MAX_CSI_SLOTS, MAX_INTERMEDIATES,
        OscTerminator, Parser, State, VtHandler,
    };
    use egui_xterm_core::{CoreConfig, PackedColor, Terminal, TerminalEvent};

    #[derive(Default)]
    struct SpyHandler {
        csi_count: usize,
        last_truncated: bool,
        last_param_len: usize,
    }

    impl VtHandler for SpyHandler {
        fn print(&mut self, _ch: char) {}

        fn execute(&mut self, _byte: u8) {}

        fn hook(&mut self, _params: &CsiParams, _intermediates: &[u8], _action: u8) {}

        fn put(&mut self, _byte: u8) {}

        fn unhook(&mut self) {}

        fn osc_dispatch(&mut self, _params: &[&[u8]], _terminator: OscTerminator) {}

        fn csi_dispatch(&mut self, params: &CsiParams, _intermediates: &[u8], _action: u8) {
            self.csi_count += 1;
            self.last_truncated = params.is_truncated();
            self.last_param_len = params.len();
        }

        fn esc_dispatch(&mut self, _intermediates: &[u8], _action: u8) {}
    }

    #[test]
    fn t2_f3_01_split_utf8_byte_by_byte() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        for &byte in "é中🚀".as_bytes() {
            term.advance(&[byte]);
        }
        let line = term.visible_line_text(0).expect("row 0 exists");
        assert_eq!(line, "é中🚀");
        assert!(!line.contains('\u{FFFD}'));
    }

    #[test]
    fn t2_f3_02_split_csi_and_osc_byte_by_byte() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        for &byte in b"\x1b[2;4HX\x1b]2;Split\x1b\\Y" {
            term.advance(&[byte]);
        }
        assert_eq!(term.cell_at(3, 1).expect("cell (3,1)").ch, 'X');
        assert_eq!(term.cell_at(4, 1).expect("cell (4,1)").ch, 'Y');
        assert_eq!(
            term.drain_events(),
            vec![TerminalEvent::TitleChanged("Split".to_owned())]
        );
    }

    #[test]
    fn t2_f3_03_malformed_utf8_recovery() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(&[0xFF, 0xFE, 0xC0, 0xAF, 0xE4, 0xB8, b'A']);
        assert_eq!(
            term.visible_line_text(0),
            Some("\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}A".to_owned())
        );
    }

    #[test]
    fn t2_f3_04_can_sub_cancel_sequences() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"\x1b[31\x18OK\x1b]2;Partial\x1a!");

        assert_eq!(term.visible_line_text(0), Some("OK!".to_owned()));
        assert_eq!(
            term.cell_at(0, 0).expect("cell (0,0)").fg,
            PackedColor::Default
        );
        assert!(term.drain_events().is_empty());
        assert!(term.title().is_empty());
    }

    #[test]
    fn t2_f3_05_bounded_csi_params_and_huge_integer_clamping() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(
            b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;999999999m\x1b[999999;999999HX",
        );

        assert_eq!(term.cursor_pos(), (79, 23));
        assert_eq!(term.cell_at(79, 23).expect("bottom-right cell").ch, 'X');

        let mut parser = Parser::new();
        let mut spy = SpyHandler::default();
        let mut many_params = String::from("\x1b[");
        for i in 1..=70 {
            if i > 1 {
                many_params.push(';');
            }
            let _ = write!(many_params, "{i}");
        }
        many_params.push('m');
        parser.advance(many_params.as_bytes(), &mut spy);
        assert_eq!(spy.csi_count, 1);
        assert!(spy.last_truncated);
        assert!(spy.last_param_len <= MAX_CSI_SLOTS);
        assert_eq!(parser.state(), State::Ground);

        // Overflow intermediates (> MAX_INTERMEDIATES) enters CsiIgnore and suppresses dispatch.
        let mut too_many_intermediates = vec![0x1B, b'['];
        too_many_intermediates.extend(std::iter::repeat_n(b' ', MAX_INTERMEDIATES + 2));
        parser.advance(&too_many_intermediates, &mut spy);
        assert_eq!(parser.state(), State::CsiIgnore);
        parser.advance(b"m", &mut spy);
        assert_eq!(spy.csi_count, 1);
        assert_eq!(parser.state(), State::Ground);
    }

    #[test]
    fn t2_f3_06_overlong_osc_payload_and_esc_interruption() {
        // Subcase A: custom max_osc_bytes = 4096
        let cfg_small = CoreConfig {
            max_osc_bytes: 4096,
            ..CoreConfig::default()
        };
        let mut term_small = Terminal::new(80, 24, cfg_small);
        term_small.advance(b"\x1b]2;");
        let chunk_64k = vec![b'a'; 64 * 1024];
        term_small.advance(&chunk_64k);
        term_small.advance(b"\x1b\\OK");
        term_small.advance(b"\x1b[12\x1b[5CZ");

        assert!(term_small.title().is_empty());
        assert!(term_small.drain_events().is_empty());
        assert_eq!(
            term_small.visible_line_text(0),
            Some("OK     Z".to_owned()),
            "\"OK\" moves cursor to col 2, so CSI 5 C lands 'Z' at col 7"
        );
        assert_eq!(term_small.cell_at(7, 0).expect("col 7").ch, 'Z');
        assert_eq!(term_small.cursor_pos(), (8, 0));

        // Subcase B: default DEFAULT_MAX_OSC_BYTES (4 MiB) exceeded with 5 MiB payload
        let mut term_default = Terminal::new(80, 24, CoreConfig::default());
        term_default.advance(b"\x1b]2;");
        let chunk_5m = vec![b'a'; DEFAULT_MAX_OSC_BYTES + 1024 * 1024];
        term_default.advance(&chunk_5m);
        term_default.advance(b"\x1b\\OK");

        assert!(term_default.title().is_empty());
        assert!(term_default.drain_events().is_empty());
        assert_eq!(term_default.visible_line_text(0), Some("OK".to_owned()));
    }

    #[test]
    fn t2_f3_07_c1_policy_utf8_only_vs_vt500_eight_bit() {
        // Subcase A: Utf8Only (default) protects UTF-8 continuation bytes 0x80..=0x9F.
        let mut utf8_term = Terminal::new(80, 24, CoreConfig::default());
        assert_eq!(CoreConfig::default().c1_policy, C1Policy::Utf8Only);
        utf8_term.advance(&[0x9B, b'2', b';', b'3', b'H', b'Z']);
        utf8_term.advance(b"\r\n");
        utf8_term.advance("Ü😀".as_bytes());
        assert_eq!(utf8_term.cell_at(2, 1).expect("cell (2,1)").ch, ' ');
        assert_eq!(utf8_term.visible_line_text(1), Some("Ü😀".to_owned()));

        // Subcase B: Vt500EightBit treats 0x9B/0x9D/0x9C as CSI/OSC/ST.
        let cfg_c1 = CoreConfig {
            c1_policy: C1Policy::Vt500EightBit,
            ..CoreConfig::default()
        };
        let mut c1_term = Terminal::new(80, 24, cfg_c1);
        c1_term.advance(&[
            0x9B, b'2', b';', b'3', b'H', b'Z', 0x9D, b'2', b';', b'C', b'1', 0x9C,
        ]);
        assert_eq!(c1_term.cell_at(2, 1).expect("cell (2,1)").ch, 'Z');
        assert_eq!(c1_term.title(), "C1");
        assert_eq!(
            c1_term.drain_events(),
            vec![TerminalEvent::TitleChanged("C1".to_owned())]
        );
    }
}

// ===========================================================================
// F4 — Compact Cell & Sparse CellExtra Boundaries (6 specs, ungated M2)
// ===========================================================================

mod stage_m2_f4 {
    use egui_xterm_core::{CoreConfig, PackedColor, Terminal, UnderlineStyle};

    #[test]
    fn t2_f4_01_wide_char_at_last_column() {
        let mut term = Terminal::new(10, 5, CoreConfig::default());
        term.advance(b"\x1b[1;10H");
        term.advance("中".as_bytes());

        let pad = term.cell_at(9, 0).expect("last col pad");
        assert_eq!(pad.ch, ' ');
        assert_eq!(pad.width, 0);
        assert!(!pad.is_wide);
        assert!(!pad.is_wide_spacer);

        let lead = term.cell_at(0, 1).expect("wrapped lead");
        assert_eq!(lead.ch, '中');
        assert!(lead.is_wide);
        assert_eq!(lead.width, 2);

        let spacer = term.cell_at(1, 1).expect("wrapped spacer");
        assert!(spacer.is_wide_spacer);
        assert_eq!(spacer.width, 0);

        assert_eq!(term.cursor_pos(), (2, 1));
    }

    #[test]
    fn t2_f4_02_overwrite_left_or_right_half_of_wide_char() {
        let mut term = Terminal::new(10, 5, CoreConfig::default());
        term.advance("中文".as_bytes());

        // Case A: overwrite left half of '中' at col 0.
        term.advance(b"\x1b[1;1HA");
        let c0 = term.cell_at(0, 0).expect("col 0");
        assert_eq!(c0.ch, 'A');
        assert!(!c0.is_wide);
        let c1 = term.cell_at(1, 0).expect("col 1");
        assert_eq!(c1.ch, ' ');
        assert!(!c1.is_wide_spacer);

        // Case B: overwrite right spacer of '文' at col 3.
        term.advance(b"\x1b[1;4HB");
        let c2 = term.cell_at(2, 0).expect("col 2");
        assert_eq!(c2.ch, ' ');
        assert!(!c2.is_wide);
        let c3 = term.cell_at(3, 0).expect("col 3");
        assert_eq!(c3.ch, 'B');
        assert!(!c3.is_wide_spacer);
    }

    #[test]
    fn t2_f4_03_combining_mark_at_origin_no_base_char() {
        let mut term = Terminal::new(10, 5, CoreConfig::default());
        term.advance("\u{0301}A".as_bytes());

        assert_eq!(term.visible_line_text(0), Some("A".to_owned()));
        assert_eq!(term.cursor_pos(), (1, 0));
        assert_eq!(term.cell_at(0, 0).expect("col 0").ch, 'A');
    }

    #[test]
    fn t2_f4_04_one_column_terminal_with_wide_char() {
        let mut term = Terminal::new(1, 5, CoreConfig::default());
        term.advance("中A".as_bytes());

        assert_eq!(term.size(), (1, 5));
        assert_eq!(term.visible_line_text(0), Some("中".to_owned()));
        assert_eq!(term.visible_line_text(1), Some("A".to_owned()));
    }

    #[test]
    fn t2_f4_05_malformed_sgr_58_and_sgr_0_reset() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"\x1b[4;58;2;1;2;3mA\x1b[58;2;255mB\x1b[0mC");

        let a = term.cell_at(0, 0).expect("A");
        assert_eq!(a.underline_style, UnderlineStyle::Single);
        assert_eq!(a.underline_color, Some(PackedColor::rgb(1, 2, 3)));

        let b = term.cell_at(1, 0).expect("B");
        assert_eq!(b.ch, 'B');

        let c = term.cell_at(2, 0).expect("C");
        assert_eq!(c.ch, 'C');
        assert_eq!(c.underline_style, UnderlineStyle::None);
        assert_eq!(c.underline_color, None);
    }

    #[test]
    fn t2_f4_06_clearing_or_overwriting_cell_releases_extra_data() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance("\x1b[4;58;2;1;2;3me\u{0301}\r\n".as_bytes());
        term.advance("\x1b[4;58;2;1;2;3me\u{0301}\r\n".as_bytes());
        term.advance("\x1b[4;58;2;1;2;3me\u{0301}".as_bytes());

        // Overwrite row 0 col 0
        term.advance(b"\x1b[1;1H\x1b[0mZ");
        // ECH row 1 col 0
        term.advance(b"\x1b[2;1H\x1b[0m\x1b[1X");
        // EL row 2
        term.advance(b"\x1b[3;1H\x1b[0m\x1b[2K");

        let r0 = term.cell_at(0, 0).expect("row 0");
        assert_eq!(r0.ch, 'Z');
        assert_eq!(r0.grapheme, "Z");
        assert_eq!(r0.underline_color, None);
        assert_eq!(r0.underline_style, UnderlineStyle::None);

        for row in [1u16, 2u16] {
            let cell = term.cell_at(0, row).expect("erased cell");
            assert_eq!(cell.ch, ' ');
            assert_eq!(cell.grapheme, " ");
            assert_eq!(cell.underline_color, None);
            assert_eq!(cell.underline_style, UnderlineStyle::None);
        }
    }
}

// ===========================================================================
// F5 — Ring Scrollback & Row Pooling Boundaries (6 specs, ungated M2)
// ===========================================================================

mod stage_m2_f5 {
    use egui_xterm_core::{CoreConfig, Terminal};

    #[test]
    fn t2_f5_01_zero_scrollback_limit_discards_offscreen_lines() {
        let cfg = CoreConfig {
            scrollback_limit: 0,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(20, 3, cfg);
        term.advance(b"L0\r\nL1\r\nL2\r\nL3\r\nL4");
        term.scroll_display(10);

        assert_eq!(term.scrollback_len(), 0);
        assert_eq!(term.display_offset(), 0);
        assert_eq!(term.scrollback_line_text(0), None);
        assert_eq!(term.visible_lines(), vec!["L2", "L3", "L4"]);
    }

    #[test]
    fn t2_f5_02_scroll_display_clamping_upper_and_lower() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"L0\r\nL1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6");
        assert_eq!(term.scrollback_len(), 4);

        term.scroll_display(i32::MAX);
        assert_eq!(term.display_offset(), 4);

        term.scroll_display(-1000);
        assert_eq!(term.display_offset(), 0);
    }

    #[test]
    fn t2_f5_03_scroll_display_zero_is_noop() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"L0\r\nL1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6");
        term.scroll_display(2);
        let before = term.visible_lines();

        term.scroll_display(0);
        assert_eq!(term.display_offset(), 2);
        assert_eq!(term.visible_lines(), before);
    }

    #[test]
    fn t2_f5_04_new_output_keeps_scrolled_viewport_anchored() {
        let cfg = CoreConfig {
            scrollback_limit: 100,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(20, 3, cfg);
        term.advance(b"L0\r\nL1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6\r\nL7\r\nL8\r\nL9");
        term.scroll_display(3);
        assert_eq!(term.visible_lines(), vec!["L4", "L5", "L6"]);

        term.advance(b"\r\nL10\r\nL11");
        assert_eq!(term.display_offset(), 5);
        assert_eq!(term.visible_lines(), vec!["L4", "L5", "L6"]);
    }

    #[test]
    fn t2_f5_05_new_output_evicting_viewed_line_clamps_display_offset() {
        let cfg = CoreConfig {
            scrollback_limit: 4,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(20, 3, cfg);
        term.advance(b"L0\r\nL1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6");
        term.scroll_display(4);
        assert_eq!(term.visible_lines(), vec!["L0", "L1", "L2"]);

        term.advance(b"\r\nL7\r\nL8");
        assert_eq!(term.scrollback_len(), 4);
        assert!(term.display_offset() <= term.scrollback_len());
        assert_eq!(term.display_offset(), 4);
        assert_eq!(term.visible_lines(), vec!["L2", "L3", "L4"]);
    }

    #[test]
    fn t2_f5_06_su_sd_full_screen_vs_decstbm_scroll_region() {
        let mut term = Terminal::new(20, 4, CoreConfig::default());
        term.advance(b"L0\r\nL1\r\nL2\r\nL3");

        // Full-screen SU 1 pushes L0 to scrollback.
        term.advance(b"\x1b[1S");
        assert_eq!(term.scrollback_len(), 1);
        assert_eq!(term.scrollback_line_text(0), Some("L0".to_owned()));
        assert_eq!(term.visible_lines(), vec!["L1", "L2", "L3", ""]);

        // DECSTBM 2..3 SU 1 scrolls only rows 1..=2 (0-based) without pushing to scrollback.
        term.advance(b"\x1b[2;3r\x1b[1S");
        assert_eq!(term.scrollback_len(), 1);
        assert_eq!(term.visible_lines(), vec!["L1", "L3", "", ""]);

        // DECSTBM 2..3 SD 1 scrolls down within rows 1..=2.
        term.advance(b"\x1b[1T");
        assert_eq!(term.scrollback_len(), 1);
        assert_eq!(term.visible_lines(), vec!["L1", "", "L3", ""]);
    }
}

// ===========================================================================
// F6 — Lossless Resize Reflow Boundaries (6 specs, ungated M2)
// ===========================================================================

mod stage_m2_f6 {
    use egui_xterm_core::{CoreConfig, SelectionKind, Terminal};

    #[test]
    fn t2_f6_01_atomic_wide_char_wrap_on_narrow() {
        let mut term = Terminal::new(6, 4, CoreConfig::default());
        term.advance("ABCD中".as_bytes());
        term.resize(5, 4);

        let pad = term.cell_at(4, 0).expect("pad cell");
        assert_eq!(pad.ch, ' ');
        assert_eq!(pad.width, 0);

        let lead = term.cell_at(0, 1).expect("wrapped lead");
        assert_eq!(lead.ch, '中');
        assert!(lead.is_wide);

        let spacer = term.cell_at(1, 1).expect("wrapped spacer");
        assert!(spacer.is_wide_spacer);

        assert_eq!(term.visible_line_text(0), Some("ABCD".to_owned()));
        assert_eq!(term.visible_line_text(1), Some("中".to_owned()));
    }

    #[test]
    fn t2_f6_02_alt_screen_active_resize_no_reflow() {
        let mut term = Terminal::new(10, 4, CoreConfig::default());
        term.advance(b"1234567890");
        term.advance(b"\x1b[?1049hALT_LINE_0");
        term.resize(5, 4);

        assert!(term.is_alt_screen());
        assert_eq!(term.visible_line_text(0), Some("ALT_L".to_owned()));
        assert_eq!(term.visible_line_text(1), Some(String::new()));
    }

    #[test]
    fn t2_f6_03_exiting_alt_screen_after_resize_reflows_primary() {
        let mut term = Terminal::new(10, 4, CoreConfig::default());
        term.advance(b"1234567890");
        term.advance(b"\x1b[?1049hALT_LINE_0");
        term.resize(5, 4);

        term.advance(b"\x1b[?1049l");
        assert!(!term.is_alt_screen());
        assert_eq!(term.size(), (5, 4));
        assert_eq!(term.visible_line_text(0), Some("12345".to_owned()));
        assert_eq!(term.visible_line_text(1), Some("67890".to_owned()));
    }

    #[test]
    fn t2_f6_04_degenerate_zero_dimension_resize() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"hello");

        term.resize(0, 0);
        assert_eq!(term.size(), (1, 1));
        term.resize(0, 10);
        assert_eq!(term.size(), (1, 10));
        term.resize(10, 0);
        assert_eq!(term.size(), (10, 1));
    }

    #[test]
    fn t2_f6_05_minimum_1x1_resize_with_wide_chars() {
        let mut term = Terminal::new(10, 4, CoreConfig::default());
        term.advance("A中B".as_bytes());

        term.resize(1, 1);
        assert_eq!(term.size(), (1, 1));

        term.resize(10, 4);
        assert_eq!(term.size(), (10, 4));
        assert_eq!(term.full_text(), "A中B");
        assert_eq!(term.visible_line_text(0), Some("A中B".to_owned()));
    }

    #[test]
    fn t2_f6_06_same_size_resize_is_noop() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"L0\r\nL1\r\nL2\r\nL3\r\nL4\r\nstable");
        term.scroll_display(1);
        term.selection_start(3, 0, SelectionKind::Stream);
        term.selection_update(3, 1);
        term.selection_finish();

        let lines_before = term.visible_lines();
        let cursor_before = term.cursor_pos();
        let sel_before = term.selection_text();
        let offset_before = term.display_offset();
        let epoch_before = term.damage_epoch();

        term.resize(20, 5);

        assert_eq!(term.visible_lines(), lines_before);
        assert_eq!(term.cursor_pos(), cursor_before);
        assert_eq!(term.selection_text(), sel_before);
        assert_eq!(term.display_offset(), offset_before);
        assert_eq!(term.damage_epoch(), epoch_before);
    }
}

// ===========================================================================
// F7 — Core VT Modes & Screen Control Boundaries (6 specs, ungated M2)
// ===========================================================================

mod stage_m2_f7 {
    use egui_xterm_core::{CoreConfig, CursorShape, Terminal};

    #[test]
    fn t2_f7_01_decawm_disabled_overwrites_last_column() {
        let mut term = Terminal::new(5, 3, CoreConfig::default());
        term.advance(b"\x1b[?7lABCDEFG");

        assert_eq!(term.visible_lines(), vec!["ABCDG", "", ""]);
        assert_eq!(term.cursor_pos(), (4, 0));
    }

    #[test]
    fn t2_f7_02_invalid_decstbm_margins() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"R0\r\nR1\r\nR2\r\nR3\r\nR4");
        term.advance(b"\x1b[3;3H");
        assert_eq!(term.cursor_pos(), (2, 2));

        // Invalid top >= bottom margins must be ignored without moving cursor.
        term.advance(b"\x1b[4;2r\x1b[3;3r");
        assert_eq!(term.cursor_pos(), (2, 2));

        // bottom > rows clamps to rows and homes cursor.
        term.advance(b"\x1b[1;99r");
        assert_eq!(term.cursor_pos(), (0, 0));
        assert_eq!(term.visible_lines(), vec!["R0", "R1", "R2", "R3", "R4"]);
    }

    #[test]
    fn t2_f7_03_decom_cursor_clamping_against_margins() {
        let mut term = Terminal::new(20, 6, CoreConfig::default());
        term.advance(b"\x1b[2;4r\x1b[?6h\x1b[99;1H*");

        assert_eq!(term.cell_at(0, 3).expect("bottom margin row").ch, '*');
        assert_eq!(term.cell_at(0, 4).expect("row 4").ch, ' ');
        assert_eq!(term.cell_at(0, 5).expect("row 5").ch, ' ');
        assert_eq!(term.cursor_pos(), (1, 3));
    }

    #[test]
    fn t2_f7_04_alt_screen_47_1047_1049_and_double_1049h_guard() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"PRI\x1b[?1049h\x1b[4;10H\x1b[?1049h\x1b[?1049l");
        assert!(!term.is_alt_screen());
        assert_eq!(term.visible_line_text(0), Some("PRI".to_owned()));
        assert_eq!(term.cursor_pos(), (3, 0));

        term.advance(b"\x1b[?47hALT47");
        assert!(term.is_alt_screen());
        term.advance(b"\x1b[?47l");
        assert!(!term.is_alt_screen());
        assert_eq!(term.visible_line_text(0), Some("PRI".to_owned()));

        term.advance(b"\x1b[?1047hALT1047");
        assert!(term.is_alt_screen());
        term.advance(b"\x1b[?1047l");
        assert!(!term.is_alt_screen());
        assert_eq!(term.visible_line_text(0), Some("PRI".to_owned()));
    }

    #[test]
    fn t2_f7_05_decrqm_unknown_dec_and_ansi_modes() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"\x1b[?9999$p\x1b[9999$p");
        assert_eq!(term.drain_outgoing(), b"\x1b[?9999;0$y\x1b[9999;0$y");
    }

    #[test]
    fn t2_f7_06_decscusr_out_of_range_and_cht_cbt_clamping() {
        let mut term = Terminal::new(10, 3, CoreConfig::default());
        term.advance(b"\x1b[2 q\x1b[99 q\x1b[1;9H\x1b[5IA\x1b[1;2H\x1b[5ZB");

        assert_eq!(term.cursor_shape(), CursorShape::Block);
        assert!(!term.cursor_blink());
        assert_eq!(term.cell_at(9, 0).expect("CHT clamped col 9").ch, 'A');
        assert_eq!(term.cell_at(0, 0).expect("CBT clamped col 0").ch, 'B');
    }
}

// ===========================================================================
// F8 — Colors, SGR & OSC State Boundaries (6 specs, ungated M2)
// ===========================================================================

mod stage_m2_f8 {
    use egui_xterm_core::{CoreConfig, PackedColor, Terminal, TerminalEvent, UnderlineStyle};

    #[test]
    fn t2_f8_01_malformed_truncated_sgr_colors() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"\x1b[38;2;255mA\x1b[38;5mB\x1b[4:99mC");

        assert_eq!(term.visible_line_text(0), Some("ABC".to_owned()));
        for col in 0..3u16 {
            let cell = term.cell_at(col, 0).expect("cell exists");
            assert_eq!(cell.fg, PackedColor::Default);
            assert_eq!(cell.underline_style, UnderlineStyle::None);
        }
    }

    #[test]
    fn t2_f8_02_out_of_range_256_and_rgb_sgr_values() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"\x1b[38;5;300mA\x1b[38;2;300;0;0mB");

        assert_eq!(term.visible_line_text(0), Some("AB".to_owned()));
        for col in 0..2u16 {
            let cell = term.cell_at(col, 0).expect("cell exists");
            assert_eq!(cell.fg, PackedColor::Default);
        }
    }

    #[test]
    fn t2_f8_03_osc_104_resets_palette_entry_to_theme_default() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"\x1b]4;1;?\x07");
        let orig = term.drain_outgoing();
        assert!(!orig.is_empty());

        term.advance(b"\x1b]4;1;#ff0000\x07\x1b]104;1\x07\x1b]4;1;?\x07");
        let after_reset = term.drain_outgoing();
        assert_eq!(after_reset, orig);
        assert_ne!(after_reset, b"\x1b]4;1;rgb:ffff/0000/0000\x07");
    }

    #[test]
    fn t2_f8_04_invalid_color_spec_in_osc_4_10_11_12() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(
            b"\x1b]10;#112233\x07\x1b]10;not-a-color\x07\x1b]4;999;#112233\x07\x1b]11;#zzzzzz\x07\x1b]12;rgb:1/2\x07\x1b]10;?\x07",
        );

        assert_eq!(term.drain_outgoing(), b"\x1b]10;rgb:1111/2222/3333\x07");
    }

    #[test]
    fn t2_f8_05_osc_52_invalid_base64_clear_and_read_safety() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"\x1b]52;c;!!!invalid!!!\x07\x1b]52;c;?\x07");

        assert!(term.drain_events().is_empty());
        assert!(term.drain_outgoing().is_empty());

        term.advance(b"\x1b]52;c;\x07");
        assert_eq!(
            term.drain_events(),
            vec![TerminalEvent::ClipboardWrite(String::new())]
        );
    }

    #[test]
    fn t2_f8_06_osc_empty_title_and_sgr_0_full_reset() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance(b"\x1b]2;Old\x07");
        let _ = term.drain_events();

        term.advance(b"\x1b]2;\x07\x1b[1;3;4:3;38;2;1;2;3;58;2;4;5;6mX\x1b[0mY");
        assert_eq!(term.title(), "");
        assert!(
            term.drain_events()
                .contains(&TerminalEvent::TitleChanged(String::new()))
        );

        let x = term.cell_at(0, 0).expect("cell X");
        assert_eq!(x.underline_style, UnderlineStyle::Curly);
        assert_eq!(x.fg, PackedColor::rgb(1, 2, 3));
        assert_eq!(x.underline_color, Some(PackedColor::rgb(4, 5, 6)));

        let y = term.cell_at(1, 0).expect("cell Y");
        assert!(y.flags.is_empty());
        assert_eq!(y.underline_style, UnderlineStyle::None);
        assert_eq!(y.fg, PackedColor::Default);
        assert_eq!(y.bg, PackedColor::Default);
        assert_eq!(y.underline_color, None);
    }
}

// ===========================================================================
// F9 — DEC Mode 2026 Synchronized Output Boundaries (6 specs, ungated M2)
// ===========================================================================

mod stage_m2_f9 {
    use egui_xterm_core::{CoreConfig, Terminal};

    #[test]
    fn t2_f9_01_watchdog_250ms_auto_flushes_frozen_snapshot() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance_with_time(b"BEFORE", 4.9);
        term.advance_with_time(b"\x1b[?2026h\x1b[2J\x1b[HAFTER", 5.000);

        term.advance_with_time(b"", 5.240);
        assert!(term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0), Some("BEFORE".to_owned()));

        term.advance_with_time(b"", 5.260);
        assert!(!term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0), Some("AFTER".to_owned()));
    }

    #[test]
    fn t2_f9_02_watchdog_expiry_resets_cleanly_for_next_2026h() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance_with_time(b"BEFORE", 4.9);
        term.advance_with_time(b"\x1b[?2026h\x1b[2J\x1b[HAFTER", 5.000);
        term.advance_with_time(b"", 5.260);
        assert!(!term.is_synchronized_output());

        term.advance_with_time(b"\x1b[?2026h\x1b[2J\x1b[HNEW", 6.000);
        assert!(term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0), Some("AFTER".to_owned()));

        term.advance_with_time(b"!\x1b[?2026l", 6.050);
        assert!(!term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0), Some("NEW!".to_owned()));
    }

    #[test]
    fn t2_f9_03_normal_2026l_cancels_timer_without_spurious_later_reset() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance_with_time(b"\x1b[?2026hA", 1.00);
        term.advance_with_time(b"\x1b[?2026l", 1.05);
        assert_eq!(term.render_snapshot_line_text(0), Some("A".to_owned()));

        term.advance_with_time(b"\x1b[?2026h\x1b[2J\x1b[HB", 1.20);
        term.advance_with_time(b"", 1.30);
        assert!(term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0), Some("A".to_owned()));

        term.advance_with_time(b"\x1b[?2026l", 1.32);
        assert!(!term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0), Some("B".to_owned()));
    }

    #[test]
    fn t2_f9_04_redundant_repeated_2026h_and_inactive_2026l() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance_with_time(b"BASE", 0.0);
        term.advance_with_time(b"\x1b[?2026l", 0.1);
        assert!(!term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0), Some("BASE".to_owned()));

        term.advance_with_time(b"\x1b[?2026h\x1b[2J\x1b[HX", 1.0);
        term.advance_with_time(b"\x1b[?2026hY", 1.1);
        assert!(term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0), Some("BASE".to_owned()));

        term.advance_with_time(b"\x1b[?2026l", 1.2);
        assert!(!term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0), Some("XY".to_owned()));
    }

    #[test]
    fn t2_f9_05_resize_while_mode_2026_frozen() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance_with_time(b"HELLO_WORLD", 0.5);
        term.advance_with_time(b"\x1b[?2026h\x1b[2J\x1b[HUPDATED", 1.0);

        term.resize(6, 5);
        assert_eq!(term.size(), (6, 5));
        assert!(!term.is_synchronized_output());

        term.advance_with_time(b"\x1b[?2026l", 1.05);
        assert_eq!(term.render_snapshot_line_text(0), Some("UPDATE".to_owned()));
        assert_eq!(term.render_snapshot_line_text(1), Some("D".to_owned()));
    }

    #[test]
    fn t2_f9_06_advance_without_explicit_time_while_synchronized() {
        let mut term = Terminal::new(20, 5, CoreConfig::default());
        term.advance(b"INIT");
        term.advance(b"\x1b[?2026h\x1b[2J\x1b[HNEW");

        assert!(term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0), Some("INIT".to_owned()));

        term.advance(b"\x1b[?2026l");
        assert!(!term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0), Some("NEW".to_owned()));
    }
}

// ===========================================================================
// F10 — Selection, Mouse & Input Modes Boundaries (7 specs: 4 ungated + 3 M4)
// ===========================================================================

mod stage_m2_f10 {
    use egui_xterm_core::{CoreConfig, SelectionKind, Terminal};

    #[test]
    fn t2_f10_01_reverse_backward_drag_selection() {
        let mut term = Terminal::new(20, 4, CoreConfig::default());
        term.advance(b"first line\r\nsecond line");
        term.selection_start(1, 5, SelectionKind::Stream);
        term.selection_update(0, 2);
        term.selection_finish();

        assert_eq!(term.selection_text().as_deref(), Some("rst line\nsecond"));
    }

    #[test]
    fn t2_f10_02_selection_spanning_wide_chars_and_clear() {
        let mut term = Terminal::new(20, 3, CoreConfig::default());
        term.advance("A中文B".as_bytes());

        // Col 2 is the spacer cell of '中', col 3 is the lead cell of '文'.
        term.selection_start(0, 2, SelectionKind::Stream);
        term.selection_update(0, 3);
        term.selection_finish();
        assert_eq!(term.selection_text().as_deref(), Some("中文"));

        term.selection_clear();
        assert_eq!(term.selection_text(), None);
    }

    #[test]
    fn t2_f10_03_selection_invalidated_on_scrollback_eviction() {
        let cfg = CoreConfig {
            scrollback_limit: 2,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(20, 3, cfg);
        term.advance(b"ROW0\r\nROW1\r\nROW2");
        term.selection_start(0, 0, SelectionKind::Stream);
        term.selection_update(0, 3);
        term.selection_finish();

        for i in 3..23 {
            term.advance(format!("\r\nROW{i}").as_bytes());
        }
        assert_eq!(term.selection_text(), None);
    }

    #[test]
    fn t2_f10_07_empty_and_out_of_bounds_selection_clamped() {
        let mut term = Terminal::new(10, 3, CoreConfig::default());
        assert_eq!(term.selection_text(), None);

        term.selection_start(999, 999, SelectionKind::Stream);
        term.selection_update(999, 999);
        term.selection_finish();

        let sel = term.selection().expect("selection exists");
        assert_eq!(sel.anchor.col, 9);
        assert_eq!(sel.head.col, 9);
        assert!(
            term.selection_text().as_deref().is_none_or(str::is_empty),
            "out-of-bounds blank cell selection must be None or empty"
        );
    }
}

mod stage_m4_f10 {
    use egui::{Modifiers, MouseWheelUnit, PointerButton, Vec2};
    use egui_xterm::{ScaleMode, TerminalConfig};
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn t2_f10_04_bracketed_paste_envelope_sanitization() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[?2004h");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_paste("evil\x1b[201~rm -rf\x1b[200~");
        let _ = h.run_frame();

        assert_eq!(
            h.runtime.input().written_bytes(),
            b"\x1b[200~evilrm -rf\x1b[201~"
        );

        // Nested payload that would reconstruct `\x1b[201~` under naive single-pass stripping.
        h.runtime.input_mut().clear_written();
        h.inject_paste("\x1b[20\x1b[201~1~whoami\r");
        let _ = h.run_frame();
        let nested_written = h.runtime.input_mut().take_written();
        assert!(
            nested_written.starts_with(b"\x1b[200~") && nested_written.ends_with(b"\x1b[201~"),
            "missing bracketed paste envelope: {nested_written:?}"
        );
        let inner = &nested_written[b"\x1b[200~".len()..nested_written.len() - b"\x1b[201~".len()];
        assert!(
            !inner.windows(b"\x1b[201~".len()).any(|w| w == b"\x1b[201~"),
            "nested paste payload broke out of closing envelope: {inner:?}"
        );
        assert!(
            !inner.windows(b"\x1b[200~".len()).any(|w| w == b"\x1b[200~"),
            "nested paste payload injected opening envelope: {inner:?}"
        );
    }

    #[test]
    fn t2_f10_05_large_grid_mouse_coords_x10_utf8_sgr() {
        let cfg = TerminalConfig {
            scale_mode: ScaleMode::AutoScaleToFit {
                cols: 260,
                rows: 250,
            },
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.set_viewport_size(2600.0, 2500.0);
        h.focus_terminal();
        let p = h.cell_center(250, 240).expect("cell (250, 240) in bounds");

        // (a) X10 only (?1000h): drops or clamps safely without wrapping u8 (press+release = 12B).
        h.runtime.input_mut().push_str("\x1b[?1000h");
        let _ = h.run_frame();
        h.runtime.input_mut().clear_written();
        h.inject_click(p, PointerButton::Primary, Modifiers::NONE);
        let x10_bytes = h.runtime.input_mut().take_written();
        assert!(
            x10_bytes.is_empty()
                || (x10_bytes.len().is_multiple_of(6)
                    && x10_bytes
                        .as_chunks::<6>()
                        .0
                        .iter()
                        .all(|pkt| pkt.starts_with(b"\x1b[M") && pkt[4] == 255 && pkt[5] == 255)),
            "unexpected X10 large coord encoding: {x10_bytes:?}"
        );

        // (b) UTF-8 mode (?1000;1005h): encodes 251+32=283 (\u{011b}) and 241+32=273 (\u{0111}).
        h.runtime.input_mut().push_str("\x1b[?1005h");
        let _ = h.run_frame();
        h.runtime.input_mut().clear_written();
        h.inject_click(p, PointerButton::Primary, Modifiers::NONE);
        let utf8_bytes = h.runtime.input_mut().take_written();
        let utf8_str = String::from_utf8(utf8_bytes).expect("valid UTF-8 1005 mouse report");
        assert!(
            utf8_str.starts_with("\x1b[M \u{011b}\u{0111}"),
            "unexpected UTF-8 1005 report: {utf8_str:?}"
        );

        // (c) SGR mode (?1000;1006h): uncapped decimal coords 251;241.
        h.runtime.input_mut().push_str("\x1b[?1005l\x1b[?1006h");
        let _ = h.run_frame();
        h.runtime.input_mut().clear_written();
        h.inject_click(p, PointerButton::Primary, Modifiers::NONE);
        assert_eq!(
            h.runtime.input().written_bytes(),
            b"\x1b[<0;251;241M\x1b[<0;251;241m"
        );
    }

    #[test]
    fn t2_f10_06_alt_scroll_1007_suppressed_by_mouse_tracking_1000() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime
            .input_mut()
            .push_str("\x1b[?1049h\x1b[?1007h\x1b[?1000;1006h");
        h.focus_terminal();
        let p = h.cell_center(5, 5).expect("cell (5, 5)");
        h.inject_pointer_move(p);
        let _ = h.run_frame();
        h.runtime.input_mut().clear_written();

        h.inject_scroll(MouseWheelUnit::Line, Vec2::new(0.0, 1.0), Modifiers::NONE);
        let _ = h.run_frame();

        assert_eq!(h.runtime.input().written_bytes(), b"\x1b[<64;6;6M");
    }
}

// ===========================================================================
// F11 — Optional Feature: search Boundaries (6 specs, M3)
// ===========================================================================

#[cfg(feature = "search")]
mod stage_m3_f11 {
    use std::time::Instant;

    use egui_xterm_core::{CoreConfig, SearchQuery, Terminal};

    #[test]
    fn f11_t2_01_invalid_regex_returns_error_without_corrupting_state() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"sample text\r\n");

        let err = term.search(&SearchQuery::regex(r"[unclosed("));
        assert!(err.is_err());
        assert_eq!(term.visible_line_text(0), Some("sample text".to_owned()));

        let ok = term
            .search(&SearchQuery::literal("sample"))
            .expect("subsequent literal search works");
        assert_eq!(ok.len(), 1);
        assert_eq!((ok[0].start_col, ok[0].end_col), (0, 6));
    }

    #[test]
    fn f11_t2_02_empty_search_query_yields_empty_matches() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"non-empty content\r\n");

        let lit = term.search(&SearchQuery::literal("")).expect("empty lit");
        assert!(lit.is_empty());
        assert!(term.next_search_match().is_none());
        assert!(term.prev_search_match().is_none());
        assert!(term.active_search_match().is_none());

        let re = term.search(&SearchQuery::regex("")).expect("empty regex");
        assert!(re.is_empty());
        assert!(term.next_search_match().is_none());
        assert!(term.prev_search_match().is_none());
    }

    #[test]
    fn f11_t2_03_zero_width_regex_terminates_bounded() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"abc\r\n");

        for pattern in [r"a*", r"\b", r"^"] {
            let start = Instant::now();
            let matches = term
                .search(&SearchQuery::regex(pattern))
                .expect("valid zero-width regex");
            assert!(
                start.elapsed().as_secs_f64() < 1.0,
                "zero-width pattern {pattern:?} took too long"
            );
            assert!(
                matches.len() <= 8,
                "pattern {pattern:?} produced excessive matches: {}",
                matches.len()
            );
            assert!(matches.iter().all(|m| m.end_col <= 80));
        }
    }

    #[test]
    fn f11_t2_04_search_wide_cjk_column_indices_and_wrap() {
        // Subcase A: single row (10 cols) -> 中[0..2] 文[2..4] 测[4..6] 试[6..8] a[8..9] b[9..10]
        let mut term_a = Terminal::new(10, 5, CoreConfig::default());
        term_a.advance("中文测试ab\r\n".as_bytes());
        let matches_a = term_a
            .search(&SearchQuery::literal("测试a"))
            .expect("CJK search A");
        assert_eq!(matches_a.len(), 1);
        assert_eq!(matches_a[0].line, 0);
        assert_eq!(matches_a[0].start_col, 4);
        assert_eq!(matches_a[0].end_line, 0);
        assert_eq!(matches_a[0].end_col, 9);

        // Subcase B: cross-row wrap (8 cols) -> row 0 "中文测试" [0..8], row 1 "a" [0..1]
        let mut term_b = Terminal::new(8, 5, CoreConfig::default());
        term_b.advance("中文测试a".as_bytes());
        let matches_b = term_b
            .search(&SearchQuery::literal("测试a"))
            .expect("CJK search B");
        assert_eq!(matches_b.len(), 1);
        assert_eq!(matches_b[0].line, 0);
        assert_eq!(matches_b[0].start_col, 4);
        assert_eq!(matches_b[0].end_line, 1);
        assert_eq!(matches_b[0].end_col, 1);
    }

    #[test]
    fn f11_t2_05_search_navigation_wraps_at_boundaries() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"hit\r\nmiss\r\nhit\r\n");

        let matches = term.search(&SearchQuery::literal("hit")).expect("search");
        assert_eq!(matches.len(), 2);

        let n0 = term.next_search_match().expect("next 0").line;
        let n1 = term.next_search_match().expect("next 1").line;
        let n2 = term.next_search_match().expect("next 2").line;
        assert_ne!(n0, n1);
        assert_eq!(n0, n2);

        let p1 = term.prev_search_match().expect("prev 1").line;
        let p2 = term.prev_search_match().expect("prev 2").line;
        assert_eq!(p1, n1);
        assert_eq!(p2, n0);
    }

    #[test]
    fn f11_t2_06_search_state_invalidated_after_eviction_and_clear() {
        let cfg = CoreConfig {
            scrollback_limit: 5,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(40, 4, cfg);
        for _ in 0..3 {
            term.advance(b"old\r\n");
        }
        let initial = term.search(&SearchQuery::literal("old")).expect("search");
        assert_eq!(initial.len(), 3);

        for i in 0..20 {
            term.advance(format!("new_{i}\r\n").as_bytes());
        }
        let _ = term.next_search_match();
        let _ = term.prev_search_match();
        assert!(term.scrollback_len() <= 5);
        assert!(
            term.search(&SearchQuery::literal("old"))
                .expect("re-search after eviction")
                .is_empty()
        );

        term.advance(b"old\r\n\x1b[2J\x1b[3J");
        let _ = term.next_search_match();
        assert!(
            term.search(&SearchQuery::literal("old"))
                .expect("re-search after ED 2 + ED 3")
                .is_empty()
        );
    }
}

// ===========================================================================
// F12 — Optional Feature: hyperlinks Boundaries (6 specs: 5 M3 + 1 M4)
// ===========================================================================

#[cfg(feature = "hyperlinks")]
mod stage_m3_f12 {
    use egui_xterm_core::parser::DEFAULT_MAX_OSC_BYTES;
    use egui_xterm_core::{CoreConfig, Terminal};

    #[test]
    fn f12_t2_01_unterminated_osc8_aborted_by_can_and_sub() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(
            b"\x1b]8;;https://unterminated.test\x18\r\nnext line\r\n\x1b]8;;https://sub.test\x1aafter_sub\r\n",
        );

        assert_eq!(term.visible_line_text(1), Some("next line".to_owned()));
        assert_eq!(term.visible_line_text(2), Some("after_sub".to_owned()));
        for col in 0..10u16 {
            assert!(term.hyperlink_at(col, 1).is_none());
            assert!(term.hyperlink_at(col, 2).is_none());
        }
    }

    #[test]
    fn f12_t2_02_overlong_osc8_uri_rejected_by_caps() {
        // Case A: small max_osc_bytes = 4096
        let cfg = CoreConfig {
            max_osc_bytes: 4096,
            ..CoreConfig::default()
        };
        let mut term_a = Terminal::new(80, 24, cfg);
        let mut payload_a = b"\x1b]8;;https://example.com/".to_vec();
        payload_a.extend(std::iter::repeat_n(b'a', 16_384));
        payload_a.extend_from_slice(b"\x1b\\text\x1b]8;;\x1b\\");
        term_a.advance(&payload_a);
        assert_eq!(term_a.visible_line_text(0), Some("text".to_owned()));
        assert!(term_a.hyperlink_at(0, 0).is_none());

        // Case B: default 4 MiB limit exceeded with 5 MiB URI
        let mut term_b = Terminal::new(80, 24, CoreConfig::default());
        let mut payload_b = b"\x1b]8;;https://example.com/".to_vec();
        payload_b.extend(std::iter::repeat_n(
            b'b',
            DEFAULT_MAX_OSC_BYTES + 1024 * 1024,
        ));
        payload_b.extend_from_slice(b"\x1b\\ok\x1b]8;;\x1b\\");
        term_b.advance(&payload_b);
        assert_eq!(term_b.visible_line_text(0), Some("ok".to_owned()));
        assert!(term_b.hyperlink_at(0, 0).is_none());
    }

    #[test]
    fn f12_t2_03_implicit_url_strips_trailing_punctuation() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"See (https://example.com/path?a=1&b=2). Done!\r\n");

        assert_eq!(
            term.hyperlink_at(10, 0).map(|h| h.uri.as_str()),
            Some("https://example.com/path?a=1&b=2")
        );
        assert_eq!(
            term.hyperlink_at(36, 0).map(|h| h.uri.as_str()),
            Some("https://example.com/path?a=1&b=2")
        );
        assert!(term.hyperlink_at(4, 0).is_none());
        assert!(term.hyperlink_at(37, 0).is_none());
        assert!(term.hyperlink_at(38, 0).is_none());
    }

    #[test]
    fn f12_t2_04_unclosed_osc8_cleared_on_ris() {
        let mut term = Terminal::new(40, 5, CoreConfig::default());
        term.advance(b"\x1b]8;;https://open.example\x1b\\line1\r\nline2\r\n");
        assert_eq!(
            term.hyperlink_at(0, 0).map(|h| h.uri.as_str()),
            Some("https://open.example")
        );
        assert_eq!(
            term.hyperlink_at(0, 1).map(|h| h.uri.as_str()),
            Some("https://open.example")
        );

        term.advance(b"\x1bcplain");
        assert_eq!(term.visible_line_text(0), Some("plain".to_owned()));
        assert!(term.hyperlink_at(0, 0).is_none());
    }

    #[test]
    fn f12_t2_05_dedup_pool_survives_1000_repeats_and_eviction() {
        let cfg = CoreConfig {
            scrollback_limit: 20,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(40, 4, cfg);
        for _ in 0..1_000 {
            term.advance(b"\x1b]8;;https://same.example\x1b\\x\x1b]8;;\x1b\\\r\n");
        }
        assert!(term.hyperlink_pool().len() <= 2);

        for i in 0..100 {
            term.advance(format!("plain {i}\r\n").as_bytes());
        }
        term.advance(b"\x1b]8;;https://live.example\x1b\\live\x1b]8;;\x1b\\");

        term.scroll_display(20);
        assert!(term.hyperlink_at(0, 0).is_none());

        term.scroll_to_bottom();
        assert_eq!(
            term.hyperlink_at(0, 3).map(|h| h.uri.as_str()),
            Some("https://live.example")
        );
    }
}

#[cfg(feature = "hyperlinks")]
mod stage_m4_f12 {
    use egui::{CursorIcon, Modifiers, PointerButton, Vec2};
    use egui_xterm::TerminalEvent;
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn f12_t2_06_clicking_non_link_or_margin_emits_no_event() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime
            .input_mut()
            .push_str("\x1b]8;;https://example.com\x1b\\Link\x1b]8;;\x1b\\");
        let warm = h.run_frame();
        let rect = warm.terminal_response.response.rect;

        let non_link = h.cell_center(20, 10).expect("cell (20, 10)");
        h.inject_click(non_link, PointerButton::Primary, Modifiers::NONE);
        let out_non_link = h.last_outcome().expect("outcome 1");
        assert_ne!(
            out_non_link.platform_output.cursor_icon,
            CursorIcon::PointingHand
        );
        assert!(
            !out_non_link
                .terminal_response
                .events
                .iter()
                .any(|e| matches!(e, TerminalEvent::HyperlinkActivated { .. }))
        );
        assert!(out_non_link.opened_urls().is_empty());

        let margin = rect.max - Vec2::new(2.0, 2.0);
        h.inject_click(margin, PointerButton::Primary, Modifiers::NONE);
        let out_margin = h.last_outcome().expect("outcome 2");
        assert_ne!(
            out_margin.platform_output.cursor_icon,
            CursorIcon::PointingHand
        );
        assert!(
            !out_margin
                .terminal_response
                .events
                .iter()
                .any(|e| matches!(e, TerminalEvent::HyperlinkActivated { .. }))
        );
        assert!(out_margin.opened_urls().is_empty());
    }
}

// ===========================================================================
// F13 — Optional Feature: shell-integration Boundaries (6 specs, M3)
// ===========================================================================

#[cfg(feature = "shell-integration")]
mod stage_m3_f13 {
    use egui_xterm_core::{CoreConfig, Terminal, TerminalEvent, ZoneKind};

    #[test]
    fn f13_t2_01_osc133_d_without_exit_code() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07aborted\r\n\x1b]133;D\x07",
        );

        let events = term.drain_events();
        assert!(events.contains(&TerminalEvent::CommandFinished { exit_code: None }));
        assert!(!events.contains(&TerminalEvent::CommandFinished { exit_code: Some(0) }));
        assert!(
            term.shell_zones()
                .iter()
                .any(|z| z.kind == ZoneKind::Output)
        );
    }

    #[test]
    fn f13_t2_02_osc133_d_malformed_exit_code() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out\r\n\x1b]133;D;notanint\x07",
        );

        let events = term.drain_events();
        assert!(events.contains(&TerminalEvent::CommandFinished { exit_code: None }));
        assert_eq!(term.command_output_at(1).as_deref(), Some("out"));
    }

    #[test]
    fn f13_t2_03_osc133_out_of_order_markers_stay_consistent() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(
            b"\x1b]133;D;1\x07\x1b]133;C\x07orphan\r\n\x1b]133;A\x07$ \x1b]133;A\x07> cmd\r\n\x1b]133;B\x07ok\r\n\x1b]133;C\x07res\r\n\x1b]133;D;0\x07",
        );

        let zones = term.shell_zones();
        assert!(zones.iter().any(|z| z.kind == ZoneKind::Prompt));
        assert!(zones.iter().any(|z| z.kind == ZoneKind::Output));
        assert_eq!(term.visible_line_text(0).as_deref(), Some("orphan"));
        assert!(
            term.drain_events()
                .contains(&TerminalEvent::CommandFinished { exit_code: Some(0) })
        );
    }

    #[test]
    fn f13_t2_04_jump_to_prompt_with_zero_markers() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        for i in 0..50 {
            term.advance(format!("plain {i}\r\n").as_bytes());
        }

        assert!(term.shell_zones().is_empty());
        assert!(!term.jump_to_prompt(-1));
        assert_eq!(term.display_offset(), 0);
        assert!(!term.jump_to_prompt(1));
        assert_eq!(term.display_offset(), 0);
    }

    #[test]
    fn f13_t2_05_scrollback_eviction_prunes_old_prompt_markers() {
        let cfg = CoreConfig {
            scrollback_limit: 10,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(40, 4, cfg);
        for i in 0..30 {
            term.advance(
                format!(
                    "\x1b]133;A\x07P{i:02}\x1b]133;B\x07c\r\n\x1b]133;C\x07o{i:02}\r\n\x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }

        assert!(term.shell_zones().len() < 90);
        for _ in 0..20 {
            let _ = term.jump_to_prompt(-1);
            assert!(term.display_offset() <= term.scrollback_len());
            assert!(
                !term.visible_lines().iter().any(|line| line.contains("P00")),
                "evicted prompt P00 must never reappear"
            );
        }
    }

    #[test]
    fn f13_t2_06_osc133_on_alt_screen_does_not_corrupt_primary() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07pri_out\r\n\x1b]133;D;0\x07",
        );
        let pri_zones = term.shell_zones().len();

        term.advance(
            b"\x1b[?1049h\x1b]133;A\x07alt\x1b]133;B\x07x\r\n\x1b]133;C\x07alt_out\r\n\x1b]133;D;9\x07\x1b[?1049l",
        );

        assert_eq!(term.shell_zones().len(), pri_zones);
        assert_eq!(term.command_output_at(1).as_deref(), Some("pri_out"));
    }
}

// ===========================================================================
// F14 — Optional Feature: kitty-keyboard Boundaries (6 specs: 5 M3 + 1 M4)
// ===========================================================================

#[cfg(feature = "kitty-keyboard")]
mod stage_m3_f14 {
    use egui_xterm_core::{CoreConfig, Terminal};

    #[test]
    fn f14_t2_01_separate_stacks_primary_vs_alt_screen() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"\x1b[>1u\x1b[?1049h\x1b[?u\x1b[>15u\x1b[?u\x1b[?1049l\x1b[?u");

        assert_eq!(term.drain_outgoing(), b"\x1b[?0u\x1b[?15u\x1b[?1u");
        assert_eq!(term.kitty_keyboard_flags().bits(), 1);
    }

    #[test]
    fn f14_t2_02_stack_underflow_saturates_at_zero() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"\x1b[<99u\x1b[?u\x1b[>1u\x1b[<5u\x1b[?u");

        assert_eq!(term.drain_outgoing(), b"\x1b[?0u\x1b[?0u");
        assert!(term.kitty_keyboard_flags().is_empty());
    }

    #[test]
    fn f14_t2_03_stack_overflow_bounded_cap() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        for _ in 0..300 {
            term.advance(b"\x1b[>1u");
        }
        term.advance(b"\x1b[?u");
        assert_eq!(term.drain_outgoing(), b"\x1b[?1u");

        for _ in 0..300 {
            term.advance(b"\x1b[<u");
        }
        term.advance(b"\x1b[?u");
        assert_eq!(term.drain_outgoing(), b"\x1b[?0u");
        assert!(term.kitty_keyboard_flags().is_empty());
    }

    #[test]
    fn f14_t2_04_omitted_flags_default_zero_and_invalid_bits_masked() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"\x1b[>1u\x1b[>u\x1b[?u\x1b[>999999u\x1b[?u");

        let out = term.drain_outgoing();
        assert!(out.starts_with(b"\x1b[?0u"), "out={out:?}");
        let bits = term.kitty_keyboard_flags().bits();
        assert!(bits <= 31 && (bits & !0x1F) == 0, "bits={bits:#x}");
        let expected_suffix = format!("\x1b[?{bits}u");
        assert!(out.ends_with(expected_suffix.as_bytes()), "out={out:?}");
    }

    #[test]
    fn f14_t2_06_set_flags_equals_and_ris_stack_reset() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"\x1b[>1u\x1b[=4;2u");
        assert_ne!(term.kitty_keyboard_flags().bits() & 4, 0);

        term.advance(b"\x1bc\x1b[?u");
        assert!(term.kitty_keyboard_flags().is_empty());
        assert!(term.drain_outgoing().ends_with(b"\x1b[?0u"));
    }
}

#[cfg(feature = "kitty-keyboard")]
mod stage_m4_f14 {
    use egui::{Key, Modifiers};
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn f14_t2_05_key_release_ignored_when_flag2_disabled() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[>1u");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_key(Key::Enter, true, Modifiers::NONE);
        let _ = h.run_frame();
        let press = h.runtime.input_mut().take_written();
        assert!(
            press == b"\r" || press == b"\x1b[13u",
            "unexpected press sequence: {press:?}"
        );

        h.inject_key(Key::Enter, false, Modifiers::NONE);
        let _ = h.run_frame();
        assert!(h.runtime.input().written_bytes().is_empty());
    }
}

// ===========================================================================
// F15 — Optional Features: serde & metrics Boundaries (6 specs: 2 ungated + staged)
// ===========================================================================

#[cfg(feature = "serde")]
mod stage_m2_f15 {
    use egui_xterm::TerminalSize;
    use egui_xterm_core::{CoreConfig, Theme};

    #[test]
    fn f15_t2_01_malformed_json_fails_gracefully_core() {
        for bad in ["{invalid_json", r#"{"cols": "wrong_type"}"#] {
            let err_sz = serde_json::from_str::<TerminalSize>(bad)
                .expect_err("malformed TerminalSize JSON must fail");
            assert!(!err_sz.to_string().is_empty());

            let err_theme =
                serde_json::from_str::<Theme>(bad).expect_err("malformed Theme JSON must fail");
            assert!(!err_theme.to_string().is_empty());

            let err_cfg = serde_json::from_str::<CoreConfig>(bad)
                .expect_err("malformed CoreConfig JSON must fail");
            assert!(!err_cfg.to_string().is_empty());
        }
    }

    #[test]
    fn f15_t2_03_terminal_size_zero_and_u16_max_roundtrip() {
        let cases = [
            TerminalSize {
                cols: 0,
                rows: 0,
                pixel_width: u16::MAX,
                pixel_height: u16::MAX,
            },
            TerminalSize {
                cols: u16::MAX,
                rows: u16::MAX,
                pixel_width: 0,
                pixel_height: 0,
            },
        ];
        for sz in cases {
            let json = serde_json::to_string(&sz).expect("serialize TerminalSize");
            let decoded: TerminalSize =
                serde_json::from_str(&json).expect("deserialize TerminalSize");
            assert_eq!(decoded, sz);
        }
    }
}

#[cfg(feature = "serde")]
mod stage_m4_f15 {
    use egui_xterm::TerminalConfig;

    #[test]
    fn f15_t2_01b_malformed_json_terminal_config() {
        for bad in ["{invalid_json", r#"{"font_size": "wrong_type"}"#] {
            let err = serde_json::from_str::<TerminalConfig>(bad)
                .expect_err("malformed TerminalConfig JSON must fail");
            assert!(!err.to_string().is_empty());
        }
    }

    #[test]
    fn f15_t2_02_partial_and_empty_terminal_config_json() {
        let def = TerminalConfig::default();

        match serde_json::from_str::<TerminalConfig>("{}") {
            Ok(cfg) => {
                assert!((cfg.font_size - def.font_size).abs() < 1e-6);
                assert!((cfg.zoom_factor - def.zoom_factor).abs() < 1e-6);
                assert_eq!(cfg.scale_mode, def.scale_mode);
                assert_eq!(
                    cfg.copy_on_ctrl_c_when_selected,
                    def.copy_on_ctrl_c_when_selected
                );
                assert_eq!(
                    cfg.read_budget_bytes_per_frame,
                    def.read_budget_bytes_per_frame
                );
            }
            Err(err) => assert!(err.is_data(), "expected data error, got {err}"),
        }

        match serde_json::from_str::<TerminalConfig>(r#"{"font_size": 16.0}"#) {
            Ok(cfg) => {
                assert!((cfg.font_size - 16.0).abs() < 1e-6);
                assert!((cfg.zoom_factor - def.zoom_factor).abs() < 1e-6);
                assert_eq!(cfg.scale_mode, def.scale_mode);
                assert_eq!(
                    cfg.copy_on_ctrl_c_when_selected,
                    def.copy_on_ctrl_c_when_selected
                );
                assert_eq!(
                    cfg.read_budget_bytes_per_frame,
                    def.read_budget_bytes_per_frame
                );
            }
            Err(err) => assert!(err.is_data(), "expected data error, got {err}"),
        }
    }
}

#[cfg(feature = "serde")]
mod stage_m3_f15_serde {
    use egui_xterm_core::{CoreConfig, PackedColor, Terminal, TerminalSnapshot, UnderlineStyle};

    #[test]
    fn f15_t2_05_snapshot_roundtrip_wide_grapheme_and_cell_extra() {
        let mut term = Terminal::new(40, 10, CoreConfig::default());
        term.advance("👨‍👩‍👧‍👦 cafe\u{0301} \x1b[4:3;58:2::255:0:0merr\x1b[0m".as_bytes());
        let snap = term.export_snapshot();

        let json = serde_json::to_string(&snap).expect("serialize snapshot");
        let decoded: TerminalSnapshot = serde_json::from_str(&json).expect("deserialize snapshot");
        let restored = Terminal::from_snapshot(decoded, CoreConfig::default());

        assert_eq!(restored.cell_at(0, 0).expect("emoji").grapheme, "👨‍👩‍👧‍👦");
        assert_eq!(
            restored.cell_at(6, 0).expect("combining").grapheme,
            "e\u{0301}"
        );
        let err_cell = restored.cell_at(8, 0).expect("styled err");
        assert_eq!(err_cell.underline_style, UnderlineStyle::Curly);
        assert_eq!(err_cell.underline_color, Some(PackedColor::rgb(255, 0, 0)));
    }
}

#[cfg(feature = "metrics")]
mod stage_m3_f15_metrics {
    use egui_xterm_core::{CoreConfig, Terminal};

    #[test]
    fn f15_t2_04_metrics_initial_zero_and_empty_advance() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        assert_eq!(term.metrics().bytes_processed, 0);

        term.advance(b"");
        assert_eq!(term.metrics().bytes_processed, 0);
    }

    #[test]
    fn f15_t2_06_metrics_monotonic_across_resize_and_clear() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(&vec![b'A'; 1000]);
        let c1 = term.metrics().bytes_processed;
        assert!(c1 >= 1000);

        term.resize(40, 12);
        term.advance(b"\x1b[2J");
        term.advance(&vec![b'B'; 500]);
        let c2 = term.metrics().bytes_processed;
        assert!(c2 >= c1 + 500);
    }
}

#[cfg(feature = "metrics")]
mod stage_m4_f15_metrics {
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn f15_t2_04b_metrics_idle_frames_invariance() {
        let mut h = HeadlessTermHarness::scripted();
        let _ = h.run_frame();
        let dirty_1 = h.runtime.terminal().metrics().dirty_rows_rendered;
        for _ in 0..3 {
            let _ = h.run_frame();
        }
        let metrics = h.runtime.terminal().metrics();
        assert_eq!(metrics.bytes_processed, 0);
        assert_eq!(metrics.frames_rendered, 4);
        assert_eq!(metrics.dirty_rows_rendered, dirty_1);
    }
}

// ===========================================================================
// F16 — egui Frame Auto-Layout & Auto-Scaling Boundaries (6 specs, M4)
// ===========================================================================

mod stage_m4_f16 {
    use egui::{Pos2, Rect, Vec2};
    use egui_xterm::{ScaleMode, TerminalConfig, TerminalIoError};
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn f16_t2_01_zero_sized_container_rect_clamps_safely() {
        let mut h = HeadlessTermHarness::scripted();
        let in_ui = h
            .run_frame_in_ui(|ui, runtime, config| {
                ui.scope_builder(egui::UiBuilder::new().max_rect(Rect::ZERO), |ui| {
                    runtime.show_with_config(ui, config)
                })
                .inner
            })
            .terminal_response;
        assert!(in_ui.cols >= 1 && in_ui.rows >= 1);
        assert!(in_ui.cell_size.x.is_finite() && in_ui.cell_size.x > 0.0);
        assert!(in_ui.cell_size.y.is_finite() && in_ui.cell_size.y > 0.0);
        assert!(in_ui.effective_font_size.is_finite() && in_ui.effective_font_size > 0.0);

        h.set_viewport_size(0.0, 0.0);
        let zero_vp = h.run_frame().terminal_response;
        assert!(zero_vp.cols >= 1 && zero_vp.rows >= 1);
        assert!(zero_vp.cell_size.x.is_finite() && zero_vp.cell_size.x > 0.0);
        assert!(zero_vp.cell_size.y.is_finite() && zero_vp.cell_size.y > 0.0);
        assert!(zero_vp.effective_font_size.is_finite() && zero_vp.effective_font_size > 0.0);
    }

    #[test]
    fn f16_t2_02_tiny_1x1_container_clamps_both_scale_modes() {
        let mut h = HeadlessTermHarness::scripted();
        let tiny_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1.0, 1.0));

        h.config.scale_mode = ScaleMode::ReflowGrid;
        let reflow = h
            .run_frame_in_ui(|ui, runtime, config| {
                ui.scope_builder(egui::UiBuilder::new().max_rect(tiny_rect), |ui| {
                    runtime.show_with_config(ui, config)
                })
                .inner
            })
            .terminal_response;
        assert!(reflow.cols >= 1 && reflow.rows >= 1);
        assert!(reflow.cell_size.x.is_finite() && reflow.cell_size.x > 0.0);
        assert!(reflow.cell_size.y.is_finite() && reflow.cell_size.y > 0.0);

        h.config.scale_mode = ScaleMode::AutoScaleToFit { cols: 80, rows: 24 };
        let scaled = h
            .run_frame_in_ui(|ui, runtime, config| {
                ui.scope_builder(egui::UiBuilder::new().max_rect(tiny_rect), |ui| {
                    runtime.show_with_config(ui, config)
                })
                .inner
            })
            .terminal_response;
        assert_eq!((scaled.cols, scaled.rows), (80, 24));
        assert!(scaled.effective_font_size.is_finite() && scaled.effective_font_size > 0.0);
    }

    #[test]
    fn f16_t2_03_extreme_dpi_scale_factors() {
        let mut h = HeadlessTermHarness::scripted();
        h.set_viewport_size(800.0, 600.0);

        h.set_pixels_per_point(0.5);
        let r1 = h.run_frame().terminal_response;
        assert!(r1.cols >= 1 && r1.rows >= 1);
        assert!(r1.cell_size.x.is_finite() && r1.cell_size.x > 0.0);
        assert!(r1.cell_size.y.is_finite() && r1.cell_size.y > 0.0);

        h.set_pixels_per_point(4.0);
        let r2 = h.run_frame().terminal_response;
        assert!(r2.cols >= 1 && r2.rows >= 1);
        assert!(r2.cell_size.x.is_finite() && r2.cell_size.x > 0.0);
        assert!(r2.cell_size.y.is_finite() && r2.cell_size.y > 0.0);

        let snapped_x = r2.cell_size.x * 4.0;
        assert!((snapped_x - snapped_x.round()).abs() <= 1e-3);
        assert!(r2.cols.abs_diff(r1.cols) <= 20);
    }

    #[test]
    fn f16_t2_04_extreme_and_non_positive_zoom_factors() {
        let mut h = HeadlessTermHarness::scripted();
        h.set_zoom_factor(1.0);
        let base = h.run_frame().terminal_response;

        h.set_zoom_factor(0.1);
        let zoomed_out = h.run_frame().terminal_response;
        assert!(zoomed_out.cols > base.cols);

        h.set_zoom_factor(10.0);
        let zoomed_in = h.run_frame().terminal_response;
        assert!(zoomed_in.cols >= 1 && zoomed_in.cols < base.cols);

        for bad_zoom in [0.0, -1.0] {
            h.set_zoom_factor(bad_zoom);
            let resp = h.run_frame().terminal_response;
            assert!(resp.effective_font_size.is_finite() && resp.effective_font_size > 0.0);
            assert!(resp.cols >= 1 && resp.rows >= 1);
        }
    }

    #[test]
    fn f16_t2_05_backend_resize_error_is_non_fatal() {
        let cfg = TerminalConfig {
            resize_debounce_secs: 0.0,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.set_viewport_size(800.0, 600.0);
        let r1 = h.run_frame().terminal_response;

        h.runtime
            .input_mut()
            .fail_next_resize(TerminalIoError::message("boom"));
        h.set_viewport_size(400.0, 300.0);
        h.runtime.input_mut().push_str("ALIVE");
        let outcome = h.run_frame();
        let r2 = &outcome.terminal_response;

        assert!(r2.cols < r1.cols && r2.rows < r1.rows);
        assert_eq!(h.runtime.terminal().size(), (r2.cols, r2.rows));
        assert!(outcome.inspect_shapes().contains_text("ALIVE"));
    }

    #[test]
    fn f16_t2_06_degenerate_auto_scale_zero_cols_rows_clamps() {
        let cfg = TerminalConfig {
            scale_mode: ScaleMode::AutoScaleToFit { cols: 0, rows: 0 },
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        let resp = h.run_frame().terminal_response;

        assert!(resp.cols >= 1 && resp.rows >= 1);
        assert!(resp.effective_font_size.is_finite() && resp.effective_font_size > 0.0);
        assert!(resp.cell_size.x.is_finite() && resp.cell_size.x > 0.0);
        assert!(resp.cell_size.y.is_finite() && resp.cell_size.y > 0.0);
    }
}

// ===========================================================================
// F17 — 4-Stage <16ms egui Renderer & vector-glyphs Boundaries (6 specs, M4)
// ===========================================================================

mod stage_m4_f17 {
    use egui::{Color32, PointerButton};
    use egui_xterm::{ScaleMode, TerminalConfig};
    use egui_xterm_core::Theme;
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn f17_t2_01_empty_terminal_produces_minimal_valid_output() {
        let mut h = HeadlessTermHarness::scripted();
        h.set_window_focused(false);
        let outcome = h.run_frame();
        let insp = outcome.inspect_shapes();

        assert!(insp.text_entries.iter().all(|e| e.text.trim().is_empty()));
        assert!(insp.rect_count >= 1 || insp.mesh_shape_count >= 1);
        let stats = h.render_to_mesh_stats();
        assert!(stats.primitive_count > 0 && stats.total_vertices > 0);
    }

    #[test]
    fn f17_t2_02_sgr_8_hidden_text_suppresses_visible_fg() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime
            .input_mut()
            .push_str("\x1b[8mSECRET\x1b[0mVISIBLE");
        let insp = h.run_frame().inspect_shapes();

        assert!(insp.contains_text("VISIBLE"));
        let theme = Theme::dark();
        let default_fg = Color32::from_rgb(
            theme.scheme.foreground[0],
            theme.scheme.foreground[1],
            theme.scheme.foreground[2],
        );
        let default_bg = Color32::from_rgb(
            theme.scheme.background[0],
            theme.scheme.background[1],
            theme.scheme.background[2],
        );

        for entry in &insp.text_entries {
            if let Some(byte_idx) = entry.text.find("SECRET") {
                let end_idx = byte_idx + "SECRET".len();
                for section in &entry.galley.job.sections {
                    let overlaps =
                        section.byte_range.start.0 < end_idx && section.byte_range.end.0 > byte_idx;
                    if overlaps {
                        let c = section.format.color;
                        assert_ne!(c, default_fg, "SGR 8 SECRET must not use visible fg");
                        assert!(
                            c.a() == 0 || c == default_bg,
                            "SGR 8 SECRET section color must be transparent or bg, got {c:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn f17_t2_04_cursor_blink_phase_and_unfocused_cursor() {
        let cfg = TerminalConfig {
            cursor_blink: true,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);

        // Warm-up frame to capture dynamic rects for both external button and terminal.
        let mut ext_rect = egui::Rect::NOTHING;
        let mut term_rect = egui::Rect::NOTHING;
        let _ = h.run_frame_in_ui(|ui, runtime, config| {
            ext_rect = ui.button("ext").rect;
            let resp = runtime.show_with_config(ui, config);
            term_rect = resp.response.rect;
            resp
        });
        assert!(ext_rect.is_positive() && term_rect.is_positive());

        // Focus the terminal first before stepping the blink loop.
        let term_center = term_rect.center();
        h.inject_pointer_move(term_center);
        h.inject_pointer_button(
            term_center,
            PointerButton::Primary,
            true,
            egui::Modifiers::NONE,
        );
        h.inject_pointer_button(
            term_center,
            PointerButton::Primary,
            false,
            egui::Modifiers::NONE,
        );
        let focused_out = h.run_frame_in_ui(|ui, runtime, config| {
            ext_rect = ui.button("ext").rect;
            let resp = runtime.show_with_config(ui, config);
            term_rect = resp.response.rect;
            resp
        });
        assert!(
            focused_out.terminal_response.response.has_focus(),
            "terminal must be focused before stepping blink cycle"
        );

        let mut sigs = Vec::new();
        for t in [0.0_f64, 0.25, 0.55, 0.85, 1.15] {
            let delta = t - h.sim_time();
            if delta > 0.0 {
                h.advance_time(delta);
            }
            let out = h.run_frame_in_ui(|ui, runtime, config| {
                ext_rect = ui.button("ext").rect;
                let resp = runtime.show_with_config(ui, config);
                term_rect = resp.response.rect;
                resp
            });
            let insp = egui_xterm_e2e::ShapeInspection::from_terminal_shapes(
                &out.shapes,
                term_rect,
                out.pixels_per_point,
            );
            sigs.push((
                insp.rect_count,
                insp.line_count,
                insp.path_count,
                insp.rect_fills.clone(),
                insp.mesh_vertices_total,
                insp.mesh_vertex_positions().to_vec(),
            ));
        }

        let first = &sigs[0];
        assert!(
            sigs.iter().any(|s| s != first),
            "expected at least 2 distinct cursor signatures across focused blink cycle"
        );

        // Click external button to lose terminal focus.
        let ext_pos = ext_rect.center();
        h.inject_pointer_move(ext_pos);
        h.inject_pointer_button(ext_pos, PointerButton::Primary, true, egui::Modifiers::NONE);
        h.inject_pointer_button(
            ext_pos,
            PointerButton::Primary,
            false,
            egui::Modifiers::NONE,
        );
        let unfocused_out = h.run_frame_in_ui(|ui, runtime, config| {
            let _ = ui.button("ext");
            let resp = runtime.show_with_config(ui, config);
            term_rect = resp.response.rect;
            resp
        });
        assert!(
            !unfocused_out.terminal_response.response.has_focus(),
            "terminal must lose focus after clicking external button"
        );
        let unfocused = egui_xterm_e2e::ShapeInspection::from_terminal_shapes(
            &unfocused_out.shapes,
            term_rect,
            unfocused_out.pixels_per_point,
        );
        let unfocused_sig = (
            unfocused.rect_count,
            unfocused.line_count,
            unfocused.path_count,
            unfocused.rect_fills.clone(),
            unfocused.mesh_vertices_total,
            unfocused.mesh_vertex_positions().to_vec(),
        );
        assert_ne!(&unfocused_sig, first);
    }

    #[test]
    fn f17_t2_05_wide_emoji_does_not_shift_adjacent_cells() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("👨‍👩‍👧‍👦AB\r\n");
        let out = h.run_frame();
        let resp = &out.terminal_response;
        let insp = out.inspect_shapes();

        assert!(insp.contains_text("A") && insp.contains_text("B"));
        let origin_x = resp.response.rect.min.x;
        let a_entry = insp
            .text_entries
            .iter()
            .find(|e| e.text.contains('A'))
            .expect("galley for A");
        let b_entry = insp
            .text_entries
            .iter()
            .find(|e| e.text.contains('B'))
            .expect("galley for B");

        let a_byte = a_entry.text.find('A').expect("'A' inside galley");
        let a_char_idx = a_entry.text[..a_byte].chars().count();
        let a_x = a_entry.pos.x
            + a_entry
                .galley
                .pos_from_cursor(egui::text::CCursor::new(a_char_idx))
                .min
                .x;

        let b_byte = b_entry.text.find('B').expect("'B' inside galley");
        let b_char_idx = b_entry.text[..b_byte].chars().count();
        let b_x = b_entry.pos.x
            + b_entry
                .galley
                .pos_from_cursor(egui::text::CCursor::new(b_char_idx))
                .min
                .x;

        assert!(
            a_x >= origin_x + 1.5 * resp.cell_size.x,
            "expected 'A' at x={a_x} to start after 2-cell emoji (origin_x={origin_x}, cell_w={})",
            resp.cell_size.x
        );
        assert!(
            b_x > a_x,
            "expected 'B' at x={b_x} strictly after 'A' at x={a_x}"
        );
        assert!(!h.tessellate_last_frame().is_empty());
    }

    #[test]
    fn f17_t2_06_font_size_change_invalidates_row_galley_cache() {
        let cfg = TerminalConfig {
            scale_mode: ScaleMode::AutoScaleToFit { cols: 40, rows: 10 },
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.runtime.input_mut().push_str("ROW_0\r\nROW_1\r\nROW_2");

        h.set_viewport_size(800.0, 400.0);
        let f1 = h.run_frame();
        let font1 = f1.terminal_response.effective_font_size;

        h.set_viewport_size(400.0, 200.0);
        let f2 = h.run_frame();
        let font2 = f2.terminal_response.effective_font_size;
        assert!(font2 < font1 * 0.7);
        let insp2 = f2.inspect_shapes();
        assert!(insp2.contains_text("ROW_0"));
        for entry in &insp2.text_entries {
            for sec in &entry.galley.job.sections {
                assert!((sec.format.font_id.size - font2).abs() <= 0.5);
            }
        }

        h.set_viewport_size(800.0, 400.0);
        let f3 = h.run_frame();
        let font3 = f3.terminal_response.effective_font_size;
        assert!((font3 - font1).abs() <= 0.5);
        let insp3 = f3.inspect_shapes();
        assert!(insp3.contains_text("ROW_0"));
        assert!(insp3.contains_text("ROW_1"));
        assert!(insp3.contains_text("ROW_2"));
    }
}

#[cfg(feature = "vector-glyphs")]
#[test]
fn f17_t2_03_vector_glyphs_respect_parent_clip_rect() {
    use egui::{Pos2, Rect, Vec2};
    use egui_xterm_e2e::HeadlessTermHarness;

    let mut h = HeadlessTermHarness::scripted();
    h.runtime.input_mut().push_str("┌──┐\r\n│██│\r\n└──┘\r\n⣿⡇");
    let sub_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(150.0, 95.0));

    let out = h.run_frame_in_ui(|ui, runtime, config| {
        ui.scope_builder(egui::UiBuilder::new().max_rect(sub_rect), |ui| {
            ui.set_clip_rect(sub_rect);
            runtime.show_with_config(ui, config)
        })
        .inner
    });

    assert!(!out.shapes.is_empty());
    let insp = egui_xterm_e2e::ShapeInspection::from_terminal_shapes(
        &out.shapes,
        sub_rect,
        out.pixels_per_point,
    );
    assert!(
        insp.rect_count + insp.line_count + insp.path_count + insp.mesh_shape_count > 0,
        "expected non-zero terminal visual geometry inside sub_rect"
    );
    let bound = sub_rect.expand(1e-2);
    for clipped in &out.shapes {
        if clipped.clip_rect.intersects(sub_rect) {
            assert!(
                bound.contains_rect(clipped.clip_rect),
                "clip_rect {:?} escaped sub_rect {:?}",
                clipped.clip_rect,
                sub_rect
            );
        }
    }
}

// ===========================================================================
// F18 — egui Focus, Input, IME & Clipboard Boundaries (7 specs, M4)
// ===========================================================================

mod stage_m4_f18 {
    use egui::{Key, Modifiers, PointerButton, pos2};
    use egui_xterm::TerminalConfig;
    use egui_xterm_e2e::HeadlessTermHarness;

    #[test]
    fn f18_t2_01_unfocused_terminal_ignores_keys_text_ime_paste() {
        let mut h = HeadlessTermHarness::scripted();
        let _ = h.run_frame_in_ui(|ui, runtime, config| {
            let btn = ui.button("other");
            btn.request_focus();
            runtime.show_with_config(ui, config)
        });

        h.inject_text("abc");
        h.tap_key(Key::Enter, Modifiers::NONE);
        h.inject_ime_commit("中");
        h.inject_paste("x");

        let out = h.run_frame_in_ui(|ui, runtime, config| {
            let btn = ui.button("other");
            btn.request_focus();
            runtime.show_with_config(ui, config)
        });

        assert!(!out.terminal_response.response.has_focus());
        assert!(h.runtime.input().written_bytes().is_empty());
        assert!(out.platform_output.ime.is_none());
    }

    #[test]
    fn f18_t2_02_ctrl_c_sends_etx_when_copy_on_ctrl_c_disabled() {
        let cfg = TerminalConfig {
            copy_on_ctrl_c_when_selected: false,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.runtime.input_mut().push_str("abc def\r\n");
        h.focus_terminal();

        let from = h.cell_center(0, 0).expect("cell (0,0)");
        let to = h.cell_center(2, 0).expect("cell (2,0)");
        h.inject_drag_steps(from, to, PointerButton::Primary, Modifiers::NONE, 2);
        h.runtime.input_mut().clear_written();

        h.tap_key(Key::C, Modifiers::CTRL);
        let out = h.run_frame();
        assert_eq!(h.runtime.input().written_bytes(), b"\x03");
        assert!(out.copied_texts().is_empty());
    }

    #[test]
    fn f18_t2_03_paste_sanitizes_raw_esc_and_c0_under_dec_2004() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[?2004h");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_paste("safe\x1b[31m\x07line");
        let _ = h.run_frame();
        let written = h.runtime.input().written_bytes();

        assert!(written.starts_with(b"\x1b[200~"), "written={written:?}");
        assert!(written.ends_with(b"\x1b[201~"), "written={written:?}");
        let inner = &written[b"\x1b[200~".len()..written.len() - b"\x1b[201~".len()];
        assert!(
            !inner.contains(&0x1B),
            "raw ESC must be stripped: {inner:?}"
        );
        assert!(
            !inner.contains(&0x07),
            "raw BEL C0 must be stripped: {inner:?}"
        );
        assert!(inner.windows(4).any(|w| w == b"safe"));
        assert!(inner.windows(4).any(|w| w == b"line"));
    }

    #[test]
    fn f18_t2_04_empty_preedit_and_empty_commit_clear_preedit() {
        let mut h = HeadlessTermHarness::scripted();
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_ime_preedit("abc", Some(0..3));
        let _ = h.run_frame();
        h.inject_ime_preedit("", None);
        let after_empty_preedit = h.run_frame().inspect_shapes();
        assert!(!after_empty_preedit.contains_text("abc"));

        h.inject_ime_preedit("xyz", Some(0..3));
        let _ = h.run_frame();
        h.inject_ime_commit("");
        let after_empty_commit = h.run_frame().inspect_shapes();
        assert!(!after_empty_commit.contains_text("xyz"));
        assert!(h.runtime.input().written_bytes().is_empty());
    }

    #[test]
    fn f18_t2_05_focus_mode_1004_emits_once_on_gain_and_loss() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[?1004h");
        let mut ext_rect = egui::Rect::NOTHING;
        let warm = h.run_frame_in_ui(|ui, runtime, config| {
            ext_rect = ui.button("ext").rect;
            runtime.show_with_config(ui, config)
        });
        let term_center = warm.terminal_response.response.rect.center();

        // Click external first so terminal starts unfocused.
        h.inject_pointer_move(ext_rect.center());
        h.inject_pointer_button(
            ext_rect.center(),
            PointerButton::Primary,
            true,
            Modifiers::NONE,
        );
        h.inject_pointer_button(
            ext_rect.center(),
            PointerButton::Primary,
            false,
            Modifiers::NONE,
        );
        let _ = h.run_frame_in_ui(|ui, runtime, config| {
            ext_rect = ui.button("ext").rect;
            runtime.show_with_config(ui, config)
        });
        h.runtime.input_mut().clear_written();

        // Gain focus -> \x1b[I once.
        h.inject_pointer_move(term_center);
        h.inject_pointer_button(term_center, PointerButton::Primary, true, Modifiers::NONE);
        h.inject_pointer_button(term_center, PointerButton::Primary, false, Modifiers::NONE);
        let _ = h.run_frame_in_ui(|ui, runtime, config| {
            ext_rect = ui.button("ext").rect;
            runtime.show_with_config(ui, config)
        });
        assert_eq!(h.runtime.input_mut().take_written(), b"\x1b[I");

        // Steady focused frame -> nothing.
        let _ = h.run_frame_in_ui(|ui, runtime, config| {
            ext_rect = ui.button("ext").rect;
            runtime.show_with_config(ui, config)
        });
        assert!(h.runtime.input().written_bytes().is_empty());

        // Lose focus -> \x1b[O once.
        h.inject_pointer_move(ext_rect.center());
        h.inject_pointer_button(
            ext_rect.center(),
            PointerButton::Primary,
            true,
            Modifiers::NONE,
        );
        h.inject_pointer_button(
            ext_rect.center(),
            PointerButton::Primary,
            false,
            Modifiers::NONE,
        );
        let _ = h.run_frame_in_ui(|ui, runtime, config| {
            ext_rect = ui.button("ext").rect;
            runtime.show_with_config(ui, config)
        });
        assert_eq!(h.runtime.input_mut().take_written(), b"\x1b[O");
    }

    #[test]
    fn f18_t2_06_out_of_bounds_mouse_clicks_ignored() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("\x1b[?1000;1006h");
        h.focus_terminal();
        h.runtime.input_mut().clear_written();

        h.inject_click(pos2(-10.0, -10.0), PointerButton::Primary, Modifiers::NONE);
        h.inject_click(
            pos2(9999.0, 9999.0),
            PointerButton::Primary,
            Modifiers::NONE,
        );
        assert!(h.runtime.input().written_bytes().is_empty());
    }
}

#[cfg(all(feature = "native-clipboard", not(target_arch = "wasm32")))]
#[test]
fn f18_t2_07_native_clipboard_copy_paste_safe_in_headless_ci() {
    use egui::{Modifiers, PointerButton};
    use egui_xterm_e2e::HeadlessTermHarness;

    let mut h = HeadlessTermHarness::scripted();
    h.runtime.input_mut().push_str("clip_test\r\n");
    h.focus_terminal();

    let from = h.cell_center(0, 0).expect("cell (0,0)");
    let to = h.cell_center(8, 0).expect("cell (8,0)");
    h.inject_drag_steps(from, to, PointerButton::Primary, Modifiers::NONE, 3);
    h.runtime.input_mut().clear_written();

    h.inject_copy();
    let copy_out = h.run_frame();
    assert_eq!(copy_out.copied_texts(), vec!["clip_test".to_owned()]);

    h.inject_paste("pasted");
    let _ = h.run_frame();
    assert_eq!(h.runtime.input().written_bytes(), b"pasted");
}

// ===========================================================================
// F19 — Performance & Frame Latency Boundaries (6 specs: 1 ungated + 5 M4)
// ===========================================================================

mod stage_m2_f19 {
    use std::fmt::Write as _;
    use std::time::Instant;

    use egui_xterm_core::{CoreConfig, Terminal};

    #[test]
    fn f19_t2_05_pathological_huge_resize_reflow_full_scrollback() {
        let cfg = CoreConfig {
            scrollback_limit: 2_000,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(200, 60, cfg);
        let filler = "x".repeat(174);
        let mut batch = String::with_capacity(2_000 * 182);
        for i in 0..2_000 {
            let _ = write!(batch, "R{i:04}_{filler}\r\n");
        }
        term.advance(batch.as_bytes());

        let start = Instant::now();
        term.resize(10, 24);
        term.resize(200, 60);
        let elapsed = start.elapsed();

        assert!(elapsed.as_secs_f64() < 15.0, "reflow took {elapsed:?}");
        assert_eq!(term.size(), (200, 60));
        assert!(term.scrollback_len() <= 2_000);
        assert!(
            term.visible_lines()
                .iter()
                .any(|line| line.contains("R1999_"))
        );
    }
}

mod stage_m4_f19 {
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
    fn f19_t2_01_zero_read_budget_does_not_deadlock() {
        let cfg = TerminalConfig {
            read_budget_bytes_per_frame: 0,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.runtime.input_mut().push_str("hello");

        let start = Instant::now();
        let resp = h.run_frame().terminal_response;
        assert!(start.elapsed().as_secs_f64() < 1.0);
        assert!(resp.cols >= 1 && resp.rows >= 1);
    }

    #[test]
    fn f19_t2_02_usize_max_read_budget_drains_finite_input() {
        let cfg = TerminalConfig {
            read_budget_bytes_per_frame: usize::MAX,
            ..TerminalConfig::default()
        };
        let mut h = HeadlessTermHarness::scripted_with_config(cfg);
        h.runtime.input_mut().push_bytes(vec![b'A'; 1_024]);

        let out = h.run_frame();
        assert!(h.runtime.input().is_drained());
        assert!(out.inspect_shapes().contains_text("A"));
    }

    #[test]
    fn f19_t2_03_one_byte_trickle_split_utf8_and_csi_under_budget() {
        let cfg = TerminalConfig {
            read_budget_bytes_per_frame: 4096,
            ..TerminalConfig::default()
        };
        let unit = "\x1b[32m你好\x1b[0m ";
        let mut payload = String::with_capacity(10_500);
        while payload.len() < 10_000 {
            payload.push_str(unit);
        }
        let mut inner = ScriptedInput::new();
        inner.push_chunked(payload.as_bytes(), 1);

        let mut h = HeadlessTermHarness::with_config(
            ByteCountingInput {
                inner,
                total_read: 0,
            },
            cfg,
        );
        let _ = h.run_frame();
        assert!(h.runtime.input().total_read <= 4096);

        let drained = h.run_until(16, 1.0 / 60.0, |harness, _| {
            harness.runtime.input().inner.is_drained()
        });
        assert!(drained, "trickle input did not drain in 16 frames");

        let lines = h.runtime.terminal().visible_lines();
        assert!(lines.iter().any(|l| l.contains("你好")));
        assert!(lines.iter().all(|l| !l.contains('\u{FFFD}')));
    }

    #[test]
    fn f19_t2_04_closed_input_stops_polling_cleanly() {
        let mut input = ScriptedInput::from_bytes(b"hello");
        input.close();
        let mut h = HeadlessTermHarness::new(input);

        for _ in 0..5 {
            let out = h.run_frame();
            assert!(h.runtime.input().is_closed());
            assert!(out.inspect_shapes().contains_text("hello"));
        }
    }

    #[test]
    fn f19_t2_06_dec_2026_watchdog_unlatches_render_after_250ms() {
        let mut h = HeadlessTermHarness::scripted();
        h.runtime.input_mut().push_str("STALE");
        let _ = h.run_frame();

        // Latch ?2026h at t = 0.0 before advancing time.
        h.runtime
            .input_mut()
            .push_str("\x1b[?2026h\x1b[2J\x1b[HLIVE");
        let latched = h.run_frame().inspect_shapes();
        assert!(latched.contains_text("STALE"));
        assert!(!latched.contains_text("LIVE"));

        // At t = 0.10 (100ms < 250ms), snapshot remains latched.
        h.advance_time(0.10);
        let mid = h.run_frame().inspect_shapes();
        assert!(mid.contains_text("STALE"));
        assert!(!mid.contains_text("LIVE"));

        // At t = 0.30 (300ms > 250ms), watchdog unlatches and renders LIVE.
        h.advance_time(0.20);
        let after = h.run_frame().inspect_shapes();
        assert!(after.contains_text("LIVE"));
    }
}

// ===========================================================================
// F20 — Native PTY Backend Boundaries (6 specs, M5 desktop-only)
// ===========================================================================

#[cfg(all(not(target_arch = "wasm32"), feature = "pty"))]
mod stage_m5_f20 {
    use std::time::{Duration, Instant};

    use egui_xterm::pty::{PtyBackend, PtyConfig};
    use egui_xterm::{TerminalInput, TerminalSize};

    #[test]
    fn f20_t2_01_nonexistent_executable_returns_err() {
        let res = PtyBackend::spawn(PtyConfig {
            shell: Some("nonexistent_egui_xterm_binary_xyz_999".to_owned()),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            initial_size: TerminalSize::new(80, 24),
        });
        assert!(res.is_err());
    }

    #[test]
    fn f20_t2_02_nonexistent_cwd_returns_err() {
        let missing = std::env::temp_dir().join("egui_xterm_missing_cwd_12345");
        let res = PtyBackend::spawn(PtyConfig {
            shell: None,
            args: Vec::new(),
            cwd: Some(missing),
            env: Vec::new(),
            initial_size: TerminalSize::new(80, 24),
        });
        assert!(res.is_err());
    }

    #[test]
    fn f20_t2_03_write_and_resize_after_exit_fail_gracefully() {
        #[cfg(windows)]
        let (shell, args) = ("cmd".to_owned(), vec!["/C".to_owned(), "exit 0".to_owned()]);
        #[cfg(not(windows))]
        let (shell, args) = ("sh".to_owned(), vec!["-c".to_owned(), "exit 0".to_owned()]);

        let mut backend = PtyBackend::spawn(PtyConfig {
            shell: Some(shell),
            args,
            cwd: None,
            env: Vec::new(),
            initial_size: TerminalSize::new(80, 24),
        })
        .expect("spawn short command");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buf = [0u8; 256];
        while !backend.is_closed() && Instant::now() < deadline {
            let _ = backend.poll_read(&mut buf);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(backend.is_closed(), "child did not exit within 5s");
        assert!(backend.write_bytes(b"hello").is_err());
        match backend.resize(TerminalSize::new(40, 10)) {
            Ok(()) => {}
            Err(err) => assert!(!err.to_string().is_empty()),
        }
    }

    #[test]
    fn f20_t2_04_poll_read_zero_length_buffer_returns_ok_zero() {
        let mut backend = PtyBackend::spawn(PtyConfig {
            shell: None,
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            initial_size: TerminalSize::new(80, 24),
        })
        .expect("spawn shell");

        let start = Instant::now();
        let res = backend.poll_read(&mut []);
        assert!(start.elapsed() < Duration::from_millis(100));
        assert_eq!(res, Ok(0));
    }

    #[test]
    fn f20_t2_05_degenerate_zero_terminal_size_clamps_or_errors() {
        match PtyBackend::spawn(PtyConfig {
            shell: None,
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            initial_size: TerminalSize::new(0, 0),
        }) {
            Ok(backend) => drop(backend),
            Err(err) => assert!(!err.to_string().is_empty()),
        }

        let mut live = PtyBackend::spawn(PtyConfig {
            shell: None,
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            initial_size: TerminalSize::new(80, 24),
        })
        .expect("spawn 80x24 shell");
        match live.resize(TerminalSize::new(0, 0)) {
            Ok(()) => {}
            Err(err) => assert!(!err.to_string().is_empty()),
        }
    }

    #[test]
    fn f20_t2_06_drop_pty_backend_does_not_deadlock() {
        #[cfg(windows)]
        let (shell, args) = (
            "cmd".to_owned(),
            vec!["/C".to_owned(), "ping -n 60 127.0.0.1 > nul".to_owned()],
        );
        #[cfg(not(windows))]
        let (shell, args) = (
            "sh".to_owned(),
            vec!["-c".to_owned(), "sleep 60".to_owned()],
        );

        let backend = PtyBackend::spawn(PtyConfig {
            shell: Some(shell),
            args,
            cwd: None,
            env: Vec::new(),
            initial_size: TerminalSize::new(80, 24),
        })
        .expect("spawn sleeping command");

        let start = Instant::now();
        drop(backend);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

// ===========================================================================
// F21 — Multi-Tab / Multi-Pane Boundaries (6 specs: 5 M4 + 1 M4+M5+pty)
// ===========================================================================

mod stage_m4_f21 {
    use egui::{Event, Modifiers, PointerButton, Pos2, Rect, Vec2, pos2};
    use egui_xterm::{
        TerminalConfig, TerminalEvent, TerminalInput, TerminalIoError, TerminalRuntime,
        TerminalSize,
    };
    use egui_xterm_e2e::{MultiPaneHarness, ScriptedInput, run_headless_ui};

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
    fn f21_t2_01_reordered_and_removed_panes_keep_id_isolation() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 300.0));

        let mut rt_a = TerminalRuntime::new(ScriptedInput::from_bytes(b"PANE_A"));
        let mut rt_b = TerminalRuntime::new(ScriptedInput::from_bytes(b"PANE_B"));
        let mut rt_c = TerminalRuntime::new(ScriptedInput::from_bytes(b"PANE_C"));
        let mut c_rect = Rect::NOTHING;

        // Warm-up [A, B, C]
        let _ = run_headless_ui(&ctx, screen, 1.0, 0.0, true, Vec::new(), |ui| {
            ui.columns(3, |cols| {
                cols[0].push_id("pane_a", |ui| {
                    let _ = rt_a.show(ui);
                });
                cols[1].push_id("pane_b", |ui| {
                    let _ = rt_b.show(ui);
                });
                cols[2].push_id("pane_c", |ui| {
                    c_rect = rt_c.show(ui).response.rect;
                });
            });
        });

        // Click C to focus
        let c_center = c_rect.center();
        assert!(
            screen.contains(c_center),
            "pane C center {c_center:?} must lie within viewport {screen:?}"
        );
        let click_events = vec![
            Event::PointerMoved(c_center),
            Event::PointerButton {
                pos: c_center,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Event::PointerButton {
                pos: c_center,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ];
        let _ = run_headless_ui(&ctx, screen, 1.0, 0.1, true, click_events, |ui| {
            ui.columns(3, |cols| {
                cols[0].push_id("pane_a", |ui| {
                    let _ = rt_a.show(ui);
                });
                cols[1].push_id("pane_b", |ui| {
                    let _ = rt_b.show(ui);
                });
                cols[2].push_id("pane_c", |ui| {
                    let _ = rt_c.show(ui);
                });
            });
        });

        // Remove B -> [A, C], then swap -> [C, A] and type "to_c"
        let _ = run_headless_ui(&ctx, screen, 1.0, 0.2, true, Vec::new(), |ui| {
            ui.columns(2, |cols| {
                cols[0].push_id("pane_a", |ui| {
                    let _ = rt_a.show(ui);
                });
                cols[1].push_id("pane_c", |ui| {
                    let _ = rt_c.show(ui);
                });
            });
        });

        let mut c_focused = false;
        let _ = run_headless_ui(
            &ctx,
            screen,
            1.0,
            0.3,
            true,
            vec![Event::Text("to_c".to_owned())],
            |ui| {
                ui.columns(2, |cols| {
                    cols[0].push_id("pane_c", |ui| {
                        c_focused = rt_c.show(ui).response.has_focus();
                    });
                    cols[1].push_id("pane_a", |ui| {
                        let _ = rt_a.show(ui);
                    });
                });
            },
        );

        assert!(c_focused);
        assert_eq!(rt_c.input().written_bytes(), b"to_c");
        assert!(rt_a.input().written_bytes().is_empty());
        assert_eq!(
            rt_a.terminal().visible_line_text(0).as_deref(),
            Some("PANE_A")
        );
        assert_eq!(
            rt_c.terminal().visible_line_text(0).as_deref(),
            Some("PANE_C")
        );
    }

    #[test]
    fn f21_t2_03_selection_isolated_across_panes_on_copy() {
        let mut panes = MultiPaneHarness::new(
            ScriptedInput::from_bytes(b"alpha_line"),
            ScriptedInput::from_bytes(b"beta_line"),
        );
        let (ra, rb, _) = panes.run_split_frame();

        // Drag-select "alpha" (cols 0..5) on left pane.
        let la_from = Pos2::new(
            ra.response.rect.min.x + 0.5 * ra.cell_size.x,
            ra.response.rect.min.y + 0.5 * ra.cell_size.y,
        );
        let la_to = Pos2::new(
            ra.response.rect.min.x + 4.5 * ra.cell_size.x,
            ra.response.rect.min.y + 0.5 * ra.cell_size.y,
        );
        panes.queue_event(Event::PointerMoved(la_from));
        panes.queue_event(Event::PointerButton {
            pos: la_from,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        let _ = panes.run_split_frame();
        panes.queue_event(Event::PointerMoved(la_to));
        panes.queue_event(Event::PointerButton {
            pos: la_to,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        let _ = panes.run_split_frame();

        // Drag-select "beta" (cols 0..4) on right pane (focuses right).
        let rb_from = Pos2::new(
            rb.response.rect.min.x + 0.5 * rb.cell_size.x,
            rb.response.rect.min.y + 0.5 * rb.cell_size.y,
        );
        let rb_to = Pos2::new(
            rb.response.rect.min.x + 3.5 * rb.cell_size.x,
            rb.response.rect.min.y + 0.5 * rb.cell_size.y,
        );
        panes.queue_event(Event::PointerMoved(rb_from));
        panes.queue_event(Event::PointerButton {
            pos: rb_from,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        let _ = panes.run_split_frame();
        panes.queue_event(Event::PointerMoved(rb_to));
        panes.queue_event(Event::PointerButton {
            pos: rb_to,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        let _ = panes.run_split_frame();

        panes.queue_event(Event::Copy);
        let (_, _, meta) = panes.run_split_frame();
        assert_eq!(meta.copied_texts(), vec!["beta".to_owned()]);
        assert_eq!(
            panes.left.terminal().selection_text().as_deref(),
            Some("alpha")
        );
    }

    #[test]
    fn f21_t2_04_split_divider_zero_width_clamps_left_pane() {
        let mut panes = MultiPaneHarness::new(ScriptedInput::new(), ScriptedInput::new());
        panes.set_viewport_size(800.0, 600.0);
        panes.left_width_fraction = 0.0;
        let (ra, rb, _) = panes.run_split_frame();

        assert!(ra.cols >= 1 && ra.rows >= 1);
        assert!(ra.cell_size.x.is_finite() && ra.cell_size.x > 0.0);
        assert!(ra.cell_size.y.is_finite() && ra.cell_size.y > 0.0);
        assert!(rb.cols > ra.cols && rb.cols >= 80);
    }

    #[test]
    fn f21_t2_05_four_pane_burst_bounded_by_per_pane_budget() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0));
        let budget = 32 * 1024;
        let cfg = TerminalConfig {
            read_budget_bytes_per_frame: budget,
            ..TerminalConfig::default()
        };
        let payload = vec![b'X'; 1024 * 1024];

        let mut panes: Vec<TerminalRuntime<ByteCountingInput>> = (0..4)
            .map(|_| {
                let mut inner = ScriptedInput::new();
                inner.push_chunked(&payload, 8192);
                TerminalRuntime::with_config(
                    ByteCountingInput {
                        inner,
                        total_read: 0,
                    },
                    cfg.clone(),
                )
            })
            .collect();

        let _ = run_headless_ui(&ctx, screen, 1.0, 0.0, true, Vec::new(), |ui| {
            ui.columns(4, |cols| {
                for (i, pane) in panes.iter_mut().enumerate() {
                    let _ = cols[i].push_id(i, |ui| pane.show(ui));
                }
            });
        });

        let total: usize = panes.iter().map(|p| p.input().total_read).sum();
        for pane in &panes {
            assert_eq!(pane.input().total_read, budget);
        }
        assert_eq!(total, 4 * budget);
    }

    #[test]
    fn f21_t2_06_background_pane_osc52_does_not_hijack_focused_pane() {
        let mut panes = MultiPaneHarness::new(ScriptedInput::new(), ScriptedInput::new());
        panes.set_viewport_size(800.0, 600.0);
        let _ = panes.run_split_frame();

        panes.queue_event(Event::PointerMoved(pos2(100.0, 100.0)));
        panes.queue_event(Event::PointerButton {
            pos: pos2(100.0, 100.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        panes.queue_event(Event::PointerButton {
            pos: pos2(100.0, 100.0),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        let _ = panes.run_split_frame();

        panes.right.input_mut().push_str("\x1b]52;c;QUJD\x07");
        panes.queue_event(Event::Text("k".to_owned()));
        let (ra, rb, _) = panes.run_split_frame();

        assert!(
            rb.events
                .contains(&TerminalEvent::ClipboardWrite("ABC".to_owned()))
        );
        assert!(ra.response.has_focus());
        assert_eq!(panes.left.input().written_bytes(), b"k");
        assert!(panes.right.input().written_bytes().is_empty());
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "pty"))]
#[test]
fn f21_t2_02_closing_pty_pane_does_not_affect_channel_pane() {
    use std::time::{Duration, Instant};

    use egui::{Event, Modifiers, PointerButton, pos2};
    use egui_xterm::pty::{PtyBackend, PtyConfig};
    use egui_xterm::{ChannelInput, TerminalInput, TerminalSize};
    use egui_xterm_e2e::MultiPaneHarness;

    #[cfg(windows)]
    let (shell, args) = ("cmd".to_owned(), vec!["/C".to_owned(), "exit 0".to_owned()]);
    #[cfg(not(windows))]
    let (shell, args) = ("sh".to_owned(), vec!["-c".to_owned(), "exit 0".to_owned()]);

    let pty = PtyBackend::spawn(PtyConfig {
        shell: Some(shell),
        args,
        cwd: None,
        env: Vec::new(),
        initial_size: TerminalSize::new(80, 24),
    })
    .expect("spawn fast-exiting PTY");

    let (tx, ch_written_rx, ch_input) = ChannelInput::duplex_pair();
    let mut panes = MultiPaneHarness::new(pty, ch_input);
    panes.set_viewport_size(800.0, 600.0);

    let deadline = Instant::now() + Duration::from_secs(5);
    while !panes.left.input().is_closed() && Instant::now() < deadline {
        panes.advance_time(1.0 / 60.0);
        let _ = panes.run_split_frame();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(panes.left.input().is_closed());

    panes.queue_event(Event::PointerMoved(pos2(600.0, 100.0)));
    panes.queue_event(Event::PointerButton {
        pos: pos2(600.0, 100.0),
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    panes.queue_event(Event::PointerButton {
        pos: pos2(600.0, 100.0),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    let _ = panes.run_split_frame();

    tx.send(b"CH_ALIVE".to_vec()).expect("send to channel");
    panes.queue_event(Event::Text("ok".to_owned()));
    let _ = panes.run_split_frame();

    assert_eq!(
        panes.right.terminal().visible_line_text(0).as_deref(),
        Some("CH_ALIVE")
    );
    let written = ch_written_rx.try_recv().expect("channel received 'ok'");
    assert_eq!(written, b"ok");
}
