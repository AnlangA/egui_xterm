//! Clean-room DEC VT500 / Paul Williams state machine with SWAR and `memchr` fast-paths.

use super::handler::VtHandler;
use super::params::{CsiParams, DEFAULT_MAX_OSC_BYTES, MAX_INTERMEDIATES};
use super::state::{C1Policy, OscTerminator, State};
use super::utf8::{Reprocess, Utf8Decoder, Utf8Step};

/// Returns the byte length of the longest prefix in `bytes` whose bytes all lie in `0x20..=0x7E`.
#[inline]
fn scan_printable_ascii_prefix(bytes: &[u8]) -> usize {
    const HI_BITS: u64 = 0x8080_8080_8080_8080;
    const LO_MASK: u64 = 0x2020_2020_2020_2020;
    const DEL_ADD: u64 = 0x0101_0101_0101_0101;

    let mut i = 0;
    for &chunk in bytes.as_chunks::<8>().0 {
        let w = u64::from_ne_bytes(chunk);
        let high_or_del = (w | w.wrapping_add(DEL_ADD)) & HI_BITS;
        let ctrl = w.wrapping_sub(LO_MASK) & HI_BITS;
        if (high_or_del | ctrl) != 0 {
            break;
        }
        i += 8;
    }
    while i < bytes.len() && matches!(bytes[i], 0x20..=0x7E) {
        i += 1;
    }
    i
}

/// Stateful, streaming DEC VT500 / ANSI control-sequence parser.
#[derive(Debug)]
pub struct Parser {
    state: State,
    c1_policy: C1Policy,
    utf8: Utf8Decoder,
    params: CsiParams,
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediate_len: u8,
    intermediate_overflow: bool,
    osc_buf: Vec<u8>,
    osc_ranges: Vec<(usize, usize)>,
    osc_overflow: bool,
    max_osc_bytes: usize,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    /// Creates a [`Parser`] configured with [`C1Policy::Utf8Only`] and a 4 MiB OSC limit.
    #[must_use]
    pub fn new() -> Self {
        Self::with_c1_policy(C1Policy::Utf8Only)
    }

    /// Creates a [`Parser`] with an explicit [`C1Policy`].
    #[must_use]
    pub fn with_c1_policy(c1_policy: C1Policy) -> Self {
        Self {
            state: State::Ground,
            c1_policy,
            utf8: Utf8Decoder::default(),
            params: CsiParams::new(),
            intermediates: [0; MAX_INTERMEDIATES],
            intermediate_len: 0,
            intermediate_overflow: false,
            osc_buf: Vec::new(),
            osc_ranges: Vec::new(),
            osc_overflow: false,
            max_osc_bytes: DEFAULT_MAX_OSC_BYTES,
        }
    }

    /// Sets the maximum retained `OSC` payload byte budget before dropping overflowing payloads.
    #[must_use]
    pub fn with_max_osc_bytes(mut self, max_bytes: usize) -> Self {
        self.max_osc_bytes = max_bytes;
        self
    }

    /// Resets the parser to [`State::Ground`] and clears all partial sequences.
    pub fn reset(&mut self) {
        self.state = State::Ground;
        self.utf8.reset();
        self.clear_sequence();
        self.clear_and_shrink_osc();
    }

    /// Returns the current VT500 parser state.
    #[must_use]
    pub const fn state(&self) -> State {
        self.state
    }

    /// Returns the active [`C1Policy`].
    #[must_use]
    pub const fn c1_policy(&self) -> C1Policy {
        self.c1_policy
    }

    /// Streams `bytes` through the state machine, dispatching events to `handler`.
    pub fn advance(&mut self, bytes: &[u8], handler: &mut impl VtHandler) {
        let mut pos = 0usize;
        while pos < bytes.len() {
            // Fast-path 1: contiguous printable ASCII in Ground when UTF-8 decoder is idle.
            if self.state == State::Ground && self.utf8.is_idle() {
                let run_len = scan_printable_ascii_prefix(&bytes[pos..]);
                if run_len > 0 {
                    handler.print_ascii_run(&bytes[pos..pos + run_len]);
                    pos += run_len;
                    continue;
                }
            }

            // Fast-path 2: SIMD memchr skip in bulk DCS/ignore states under Utf8Only.
            if self.c1_policy == C1Policy::Utf8Only {
                match self.state {
                    State::DcsPassthrough => {
                        let rest = &bytes[pos..];
                        let stop = memchr::memchr3(0x1B, 0x18, 0x1A, rest).unwrap_or(rest.len());
                        if stop > 0 {
                            let chunk = &rest[..stop];
                            if memchr::memchr(0x7F, chunk).is_none() {
                                for &b in chunk {
                                    handler.put(b);
                                }
                            } else {
                                for &b in chunk {
                                    if b != 0x7F {
                                        handler.put(b);
                                    }
                                }
                            }
                            pos += stop;
                            continue;
                        }
                    }
                    State::DcsIgnore | State::SosPmApcString => {
                        let rest = &bytes[pos..];
                        let stop = memchr::memchr3(0x1B, 0x18, 0x1A, rest).unwrap_or(rest.len());
                        if stop > 0 {
                            pos += stop;
                            continue;
                        }
                    }
                    _ => {}
                }
            }

            let byte = bytes[pos];
            pos += 1;
            self.step(byte, handler);
        }
    }

    fn clear_sequence(&mut self) {
        self.params.clear();
        self.intermediate_len = 0;
        self.intermediate_overflow = false;
    }

    fn intermediates(&self) -> &[u8] {
        &self.intermediates[..usize::from(self.intermediate_len)]
    }

    fn collect_intermediate(&mut self, byte: u8) -> bool {
        let idx = usize::from(self.intermediate_len);
        if idx < MAX_INTERMEDIATES {
            self.intermediates[idx] = byte;
            self.intermediate_len += 1;
            true
        } else {
            self.intermediate_overflow = true;
            false
        }
    }

    fn osc_start(&mut self) {
        self.osc_buf.clear();
        self.osc_overflow = false;
    }

    fn osc_put(&mut self, byte: u8) {
        if !self.osc_overflow {
            if self.osc_buf.len() < self.max_osc_bytes {
                self.osc_buf.push(byte);
            } else {
                self.osc_overflow = true;
                self.osc_buf.clear();
            }
        }
    }

    fn clear_and_shrink_osc(&mut self) {
        self.osc_buf.clear();
        self.osc_ranges.clear();
        self.osc_overflow = false;
        if self.osc_buf.capacity() > 64 * 1024 {
            self.osc_buf.shrink_to(4096);
        }
        if self.osc_ranges.capacity() > 4096 {
            self.osc_ranges.shrink_to(256);
        }
    }

    fn osc_end(&mut self, terminator: OscTerminator, handler: &mut impl VtHandler) {
        if !self.osc_overflow {
            if self.osc_buf.is_empty() {
                handler.osc_dispatch(&[], terminator);
            } else {
                self.osc_ranges.clear();
                let mut start = 0usize;
                for semi in memchr::memchr_iter(b';', &self.osc_buf) {
                    self.osc_ranges.push((start, semi));
                    start = semi + 1;
                }
                self.osc_ranges.push((start, self.osc_buf.len()));

                if self.osc_ranges.len() <= 16 {
                    let mut stack: [&[u8]; 16] = [&[]; 16];
                    for (dst, &(s, e)) in stack.iter_mut().zip(&self.osc_ranges) {
                        *dst = &self.osc_buf[s..e];
                    }
                    handler.osc_dispatch(&stack[..self.osc_ranges.len()], terminator);
                } else {
                    let heap: Vec<&[u8]> = self
                        .osc_ranges
                        .iter()
                        .map(|&(s, e)| &self.osc_buf[s..e])
                        .collect();
                    handler.osc_dispatch(&heap, terminator);
                }
            }
        }

        self.clear_and_shrink_osc();
    }

    fn exit_current_state_for_abort(&mut self, handler: &mut impl VtHandler) {
        match self.state {
            State::OscString => {
                // CAN/SUB abort OSC without dispatching.
                self.clear_and_shrink_osc();
            }
            State::DcsPassthrough => handler.unhook(),
            State::Ground => {
                if let Some(repl) = self.utf8.take_incomplete() {
                    handler.print(repl);
                }
            }
            _ => {}
        }
    }

    fn exit_current_state_for_control(&mut self, handler: &mut impl VtHandler) {
        match self.state {
            State::OscString => self.osc_end(OscTerminator::St, handler),
            State::DcsPassthrough => handler.unhook(),
            State::Ground => {
                if let Some(repl) = self.utf8.take_incomplete() {
                    handler.print(repl);
                }
            }
            _ => {}
        }
    }

    fn step(&mut self, byte: u8, handler: &mut impl VtHandler) {
        // Check `anywhere` transitions first.
        match byte {
            0x18 | 0x1A => {
                self.exit_current_state_for_abort(handler);
                handler.execute(byte);
                self.state = State::Ground;
                return;
            }
            0x1B => {
                self.exit_current_state_for_control(handler);
                self.clear_sequence();
                self.state = State::Escape;
                return;
            }
            0x80..=0x9F if self.c1_policy == C1Policy::Vt500EightBit => {
                self.exit_current_state_for_control(handler);
                match byte {
                    0x80..=0x8F | 0x91..=0x97 | 0x99 | 0x9A => {
                        handler.execute(byte);
                        self.state = State::Ground;
                    }
                    0x90 => {
                        self.clear_sequence();
                        self.state = State::DcsEntry;
                    }
                    0x98 | 0x9E | 0x9F => {
                        self.state = State::SosPmApcString;
                    }
                    0x9B => {
                        self.clear_sequence();
                        self.state = State::CsiEntry;
                    }
                    0x9C => {
                        self.state = State::Ground;
                    }
                    0x9D => {
                        self.osc_start();
                        self.state = State::OscString;
                    }
                    _ => {}
                }
                return;
            }
            _ => {}
        }

        // State-specific transitions.
        match self.state {
            State::Ground => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => {
                    if let Some(repl) = self.utf8.take_incomplete() {
                        handler.print(repl);
                    }
                    handler.execute(byte);
                }
                0x20..=0x7E => {
                    if let Some(repl) = self.utf8.take_incomplete() {
                        handler.print(repl);
                    }
                    handler.print(char::from(byte));
                }
                0x7F => {
                    if let Some(repl) = self.utf8.take_incomplete() {
                        handler.print(repl);
                    }
                }
                0x80..=0xFF => match self.c1_policy {
                    C1Policy::Utf8Only => match self.utf8.push(byte) {
                        Utf8Step::Incomplete => {}
                        Utf8Step::Char(ch) => handler.print(ch),
                        Utf8Step::Invalid {
                            reprocess: Reprocess::Consumed,
                        } => handler.print(char::REPLACEMENT_CHARACTER),
                        Utf8Step::Invalid {
                            reprocess: Reprocess::CurrentByte,
                        } => {
                            handler.print(char::REPLACEMENT_CHARACTER);
                            match self.utf8.push(byte) {
                                Utf8Step::Incomplete => {}
                                Utf8Step::Char(ch) => handler.print(ch),
                                Utf8Step::Invalid { .. } => {
                                    handler.print(char::REPLACEMENT_CHARACTER);
                                }
                            }
                        }
                    },
                    C1Policy::Vt500EightBit => {
                        // 0x80..=0x9F already handled by `anywhere` above.
                        handler.print(char::from(byte));
                    }
                },
                0x18 | 0x1A | 0x1B => {}
            },

            State::Escape => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => handler.execute(byte),
                0x20..=0x2F => {
                    let _ = self.collect_intermediate(byte);
                    self.state = State::EscapeIntermediate;
                }
                0x50 => {
                    self.clear_sequence();
                    self.state = State::DcsEntry;
                }
                0x58 | 0x5E | 0x5F => {
                    self.state = State::SosPmApcString;
                }
                0x5B => {
                    self.clear_sequence();
                    self.state = State::CsiEntry;
                }
                0x5D => {
                    self.osc_start();
                    self.state = State::OscString;
                }
                0x30..=0x4F | 0x51..=0x57 | 0x59..=0x5A | 0x5C | 0x60..=0x7E => {
                    handler.esc_dispatch(self.intermediates(), byte);
                    self.state = State::Ground;
                }
                0x18 | 0x1A | 0x1B | 0x7F | 0x80..=0xFF => {}
            },

            State::EscapeIntermediate => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => handler.execute(byte),
                0x20..=0x2F => {
                    let _ = self.collect_intermediate(byte);
                }
                0x30..=0x7E => {
                    if !self.intermediate_overflow {
                        handler.esc_dispatch(self.intermediates(), byte);
                    }
                    self.state = State::Ground;
                }
                0x18 | 0x1A | 0x1B | 0x7F | 0x80..=0xFF => {}
            },

            State::CsiEntry => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => handler.execute(byte),
                0x20..=0x2F => {
                    if self.collect_intermediate(byte) {
                        self.state = State::CsiIntermediate;
                    } else {
                        self.state = State::CsiIgnore;
                    }
                }
                0x30..=0x39 => {
                    self.params.push_digit(byte - b'0');
                    self.state = State::CsiParam;
                }
                0x3A => {
                    self.params.next_subparam();
                    self.state = State::CsiParam;
                }
                0x3B => {
                    self.params.next_param();
                    self.state = State::CsiParam;
                }
                0x3C..=0x3F => {
                    if self.collect_intermediate(byte) {
                        self.state = State::CsiParam;
                    } else {
                        self.state = State::CsiIgnore;
                    }
                }
                0x40..=0x7E => {
                    self.params.finish();
                    handler.csi_dispatch(&self.params, self.intermediates(), byte);
                    self.state = State::Ground;
                }
                0x18 | 0x1A | 0x1B | 0x7F | 0x80..=0xFF => {}
            },

            State::CsiParam => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => handler.execute(byte),
                0x20..=0x2F => {
                    if self.collect_intermediate(byte) {
                        self.state = State::CsiIntermediate;
                    } else {
                        self.state = State::CsiIgnore;
                    }
                }
                0x30..=0x39 => self.params.push_digit(byte - b'0'),
                0x3A => self.params.next_subparam(),
                0x3B => self.params.next_param(),
                0x3C..=0x3F => self.state = State::CsiIgnore,
                0x40..=0x7E => {
                    self.params.finish();
                    handler.csi_dispatch(&self.params, self.intermediates(), byte);
                    self.state = State::Ground;
                }
                0x18 | 0x1A | 0x1B | 0x7F | 0x80..=0xFF => {}
            },

            State::CsiIntermediate => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => handler.execute(byte),
                0x20..=0x2F => {
                    if !self.collect_intermediate(byte) {
                        self.state = State::CsiIgnore;
                    }
                }
                0x30..=0x3F => self.state = State::CsiIgnore,
                0x40..=0x7E => {
                    self.params.finish();
                    handler.csi_dispatch(&self.params, self.intermediates(), byte);
                    self.state = State::Ground;
                }
                0x18 | 0x1A | 0x1B | 0x7F | 0x80..=0xFF => {}
            },

            State::CsiIgnore => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => handler.execute(byte),
                0x40..=0x7E => self.state = State::Ground,
                _ => {}
            },

            State::DcsEntry => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => {}
                0x20..=0x2F => {
                    if self.collect_intermediate(byte) {
                        self.state = State::DcsIntermediate;
                    } else {
                        self.state = State::DcsIgnore;
                    }
                }
                0x30..=0x39 => {
                    self.params.push_digit(byte - b'0');
                    self.state = State::DcsParam;
                }
                0x3A => {
                    self.params.next_subparam();
                    self.state = State::DcsParam;
                }
                0x3B => {
                    self.params.next_param();
                    self.state = State::DcsParam;
                }
                0x3C..=0x3F => {
                    if self.collect_intermediate(byte) {
                        self.state = State::DcsParam;
                    } else {
                        self.state = State::DcsIgnore;
                    }
                }
                0x40..=0x7E => {
                    self.params.finish();
                    handler.hook(&self.params, self.intermediates(), byte);
                    self.state = State::DcsPassthrough;
                }
                0x18 | 0x1A | 0x1B | 0x7F | 0x80..=0xFF => {}
            },

            State::DcsParam => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => {}
                0x20..=0x2F => {
                    if self.collect_intermediate(byte) {
                        self.state = State::DcsIntermediate;
                    } else {
                        self.state = State::DcsIgnore;
                    }
                }
                0x30..=0x39 => self.params.push_digit(byte - b'0'),
                0x3A => self.params.next_subparam(),
                0x3B => self.params.next_param(),
                0x3C..=0x3F => self.state = State::DcsIgnore,
                0x40..=0x7E => {
                    self.params.finish();
                    handler.hook(&self.params, self.intermediates(), byte);
                    self.state = State::DcsPassthrough;
                }
                0x18 | 0x1A | 0x1B | 0x7F | 0x80..=0xFF => {}
            },

            State::DcsIntermediate => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => {}
                0x20..=0x2F => {
                    if !self.collect_intermediate(byte) {
                        self.state = State::DcsIgnore;
                    }
                }
                0x30..=0x3F => self.state = State::DcsIgnore,
                0x40..=0x7E => {
                    self.params.finish();
                    handler.hook(&self.params, self.intermediates(), byte);
                    self.state = State::DcsPassthrough;
                }
                0x18 | 0x1A | 0x1B | 0x7F | 0x80..=0xFF => {}
            },

            State::DcsPassthrough => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F | 0x20..=0x7E | 0x80..=0xFF => {
                    handler.put(byte);
                }
                0x18 | 0x1A | 0x1B | 0x7F => {}
            },

            State::DcsIgnore | State::SosPmApcString => {}

            State::OscString => match byte {
                0x07 => {
                    self.osc_end(OscTerminator::Bel, handler);
                    self.state = State::Ground;
                }
                0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1C..=0x1F | 0x7F => {}
                0x20..=0x7E | 0x80..=0xFF => self.osc_put(byte),
                0x18 | 0x1A | 0x1B => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::params::MAX_CSI_SLOTS;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Event {
        Print(char),
        Execute(u8),
        Hook {
            params: Vec<Vec<u16>>,
            intermediates: Vec<u8>,
            action: u8,
        },
        Put(u8),
        Unhook,
        Osc {
            params: Vec<Vec<u8>>,
            terminator: OscTerminator,
        },
        Csi {
            params: Vec<Vec<u16>>,
            intermediates: Vec<u8>,
            action: u8,
            truncated: bool,
        },
        Esc {
            intermediates: Vec<u8>,
            action: u8,
        },
    }

    #[derive(Default)]
    struct Recorder {
        events: Vec<Event>,
        ascii_runs: Vec<Vec<u8>>,
        use_fast_path: bool,
    }

    impl Recorder {
        fn with_fast_path() -> Self {
            Self {
                events: Vec::new(),
                ascii_runs: Vec::new(),
                use_fast_path: true,
            }
        }
    }

    impl VtHandler for Recorder {
        fn print(&mut self, ch: char) {
            self.events.push(Event::Print(ch));
        }

        fn print_ascii_run(&mut self, bytes: &[u8]) {
            if self.use_fast_path {
                self.ascii_runs.push(bytes.to_vec());
            }
            for &b in bytes {
                self.print(b as char);
            }
        }

        fn execute(&mut self, byte: u8) {
            self.events.push(Event::Execute(byte));
        }

        fn hook(&mut self, params: &CsiParams, intermediates: &[u8], action: u8) {
            self.events.push(Event::Hook {
                params: params.iter().map(<[u16]>::to_vec).collect(),
                intermediates: intermediates.to_vec(),
                action,
            });
        }

        fn put(&mut self, byte: u8) {
            self.events.push(Event::Put(byte));
        }

        fn unhook(&mut self) {
            self.events.push(Event::Unhook);
        }

        fn osc_dispatch(&mut self, params: &[&[u8]], terminator: OscTerminator) {
            self.events.push(Event::Osc {
                params: params.iter().map(|s| s.to_vec()).collect(),
                terminator,
            });
        }

        fn csi_dispatch(&mut self, params: &CsiParams, intermediates: &[u8], action: u8) {
            self.events.push(Event::Csi {
                params: params.iter().map(<[u16]>::to_vec).collect(),
                intermediates: intermediates.to_vec(),
                action,
                truncated: params.is_truncated(),
            });
        }

        fn esc_dispatch(&mut self, intermediates: &[u8], action: u8) {
            self.events.push(Event::Esc {
                intermediates: intermediates.to_vec(),
                action,
            });
        }
    }

    // T1: All 14 states and transitions.
    #[test]
    fn t1_all_14_states_and_transitions() {
        let mut parser = Parser::new();
        let mut rec = Recorder::default();

        assert_eq!(parser.state(), State::Ground);

        parser.advance(b"\x1b", &mut rec);
        assert_eq!(parser.state(), State::Escape);

        parser.advance(b"(", &mut rec);
        assert_eq!(parser.state(), State::EscapeIntermediate);

        parser.advance(b"0", &mut rec);
        assert_eq!(parser.state(), State::Ground);

        parser.advance(b"\x1b[", &mut rec);
        assert_eq!(parser.state(), State::CsiEntry);

        parser.advance(b"12;34", &mut rec);
        assert_eq!(parser.state(), State::CsiParam);

        parser.advance(b" ", &mut rec);
        assert_eq!(parser.state(), State::CsiIntermediate);

        parser.advance(b"q", &mut rec);
        assert_eq!(parser.state(), State::Ground);

        parser.advance(b"\x1b[12?3m", &mut rec);
        assert_eq!(parser.state(), State::Ground);

        parser.advance(b"\x1b[12?", &mut rec);
        assert_eq!(parser.state(), State::CsiIgnore);
        parser.advance(b"m", &mut rec);
        assert_eq!(parser.state(), State::Ground);

        parser.advance(b"\x1bP", &mut rec);
        assert_eq!(parser.state(), State::DcsEntry);
        parser.advance(b"1", &mut rec);
        assert_eq!(parser.state(), State::DcsParam);
        parser.advance(b"$", &mut rec);
        assert_eq!(parser.state(), State::DcsIntermediate);
        parser.advance(b"q", &mut rec);
        assert_eq!(parser.state(), State::DcsPassthrough);
        parser.advance(b"\x1b\\", &mut rec);
        assert_eq!(parser.state(), State::Ground);

        parser.advance(b"\x1bP1?", &mut rec);
        assert_eq!(parser.state(), State::DcsIgnore);
        parser.advance(b"\x1b\\", &mut rec);
        assert_eq!(parser.state(), State::Ground);

        parser.advance(b"\x1b]2;hi", &mut rec);
        assert_eq!(parser.state(), State::OscString);
        parser.advance(b"\x07", &mut rec);
        assert_eq!(parser.state(), State::Ground);

        parser.advance(b"\x1b_apc", &mut rec);
        assert_eq!(parser.state(), State::SosPmApcString);
        parser.advance(b"\x1b\\", &mut rec);
        assert_eq!(parser.state(), State::Ground);

        // Verify ignored CSI did not dispatch and valid ones did.
        assert!(rec.events.iter().any(|e| matches!(
            e,
            Event::Esc { intermediates, action: b'0' } if intermediates == b"("
        )));
        assert!(
            rec.events
                .iter()
                .any(|e| matches!(e, Event::Csi { action: b'q', .. }))
        );
        assert!(
            !rec.events
                .iter()
                .any(|e| matches!(e, Event::Csi { action: b'm', .. }))
        );
    }

    // T2: `anywhere` transitions (CAN, SUB, ESC) & clean recovery.
    #[test]
    fn t2_anywhere_transitions_and_recovery() {
        let mut parser = Parser::new();
        let mut rec = Recorder::default();

        // CAN aborts CSI, ESC, OSC (discarded), DCS (unhooks!), SOS.
        parser.advance(b"\x1b[31\x18", &mut rec);
        assert_eq!(parser.state(), State::Ground);
        assert_eq!(rec.events, vec![Event::Execute(0x18)]);

        rec.events.clear();
        parser.advance(b"\x1b(\x1a", &mut rec);
        assert_eq!(parser.state(), State::Ground);
        assert_eq!(rec.events, vec![Event::Execute(0x1A)]);

        rec.events.clear();
        parser.advance(b"\x1b]0;aborted\x18", &mut rec);
        assert_eq!(parser.state(), State::Ground);
        assert_eq!(rec.events, vec![Event::Execute(0x18)]);

        rec.events.clear();
        parser.advance(b"\x1bPqdata\x18", &mut rec);
        assert_eq!(parser.state(), State::Ground);
        assert_eq!(
            rec.events,
            vec![
                Event::Hook {
                    params: Vec::new(),
                    intermediates: Vec::new(),
                    action: b'q',
                },
                Event::Put(b'd'),
                Event::Put(b'a'),
                Event::Put(b't'),
                Event::Put(b'a'),
                Event::Unhook,
                Event::Execute(0x18),
            ]
        );

        rec.events.clear();
        parser.advance(b"\x1bXignored\x1aok", &mut rec);
        assert_eq!(
            rec.events,
            vec![Event::Execute(0x1A), Event::Print('o'), Event::Print('k')]
        );

        // ESC mid-CSI aborts and starts new CSI.
        rec.events.clear();
        parser.advance(b"\x1b[31\x1b[32m", &mut rec);
        assert_eq!(
            rec.events,
            vec![Event::Csi {
                params: vec![vec![32]],
                intermediates: Vec::new(),
                action: b'm',
                truncated: false,
            }]
        );
    }

    // T3: Embedded C0 & DEL (0x7F).
    #[test]
    fn t3_embedded_c0_and_del() {
        let mut parser = Parser::new();
        let mut rec = Recorder::default();

        parser.advance(b"\x1b[31\nm", &mut rec);
        assert_eq!(
            rec.events,
            vec![
                Event::Execute(b'\n'),
                Event::Csi {
                    params: vec![vec![31]],
                    intermediates: Vec::new(),
                    action: b'm',
                    truncated: false,
                },
            ]
        );

        rec.events.clear();
        parser.advance(b"\x1bP\n1\rq\na\x7fb\x1b\\", &mut rec);
        assert_eq!(
            rec.events,
            vec![
                Event::Hook {
                    params: vec![vec![1]],
                    intermediates: Vec::new(),
                    action: b'q',
                },
                Event::Put(b'\n'),
                Event::Put(b'a'),
                Event::Put(b'b'),
                Event::Unhook,
                Event::Esc {
                    intermediates: Vec::new(),
                    action: b'\\',
                },
            ]
        );

        rec.events.clear();
        parser.advance(b"a\x7fb\x1b]2;\x01x\x7fy\x07", &mut rec);
        assert_eq!(
            rec.events,
            vec![
                Event::Print('a'),
                Event::Print('b'),
                Event::Osc {
                    params: vec![b"2".to_vec(), b"xy".to_vec()],
                    terminator: OscTerminator::Bel,
                },
            ]
        );
    }

    // T4: UTF-8 vs C1 policy interaction.
    #[test]
    fn t4_utf8_vs_c1_policy() {
        // Default Utf8Only: multi-byte UTF-8 containing 0x80..=0x9F works in Ground and OscString.
        let mut parser = Parser::new();
        let mut rec = Recorder::default();
        parser.advance("€Ü😀\x1b]2;Ü😀\x07".as_bytes(), &mut rec);
        assert_eq!(
            rec.events,
            vec![
                Event::Print('€'),
                Event::Print('Ü'),
                Event::Print('😀'),
                Event::Osc {
                    params: vec![b"2".to_vec(), "Ü😀".as_bytes().to_vec()],
                    terminator: OscTerminator::Bel,
                },
            ]
        );

        // Lone 0x9B / 0x9D in Utf8Only produce U+FFFD and do not hijack state.
        rec.events.clear();
        parser.advance(&[0x9B, b'3', b'1', b'm', 0x9D], &mut rec);
        assert_eq!(
            rec.events,
            vec![
                Event::Print('\u{FFFD}'),
                Event::Print('3'),
                Event::Print('1'),
                Event::Print('m'),
                Event::Print('\u{FFFD}'),
            ]
        );

        // Vt500EightBit: 0x9B, 0x9D, 0x90, 0x9C dispatch as C1 controls.
        let mut p8 = Parser::with_c1_policy(C1Policy::Vt500EightBit);
        assert_eq!(p8.c1_policy(), C1Policy::Vt500EightBit);
        rec.events.clear();
        p8.advance(
            &[
                0x9B, b'3', b'1', b'm', 0x9D, b'2', b';', b'A', 0x9C, 0x90, b'q', b'x', 0x9C,
            ],
            &mut rec,
        );
        assert_eq!(
            rec.events,
            vec![
                Event::Csi {
                    params: vec![vec![31]],
                    intermediates: Vec::new(),
                    action: b'm',
                    truncated: false,
                },
                Event::Osc {
                    params: vec![b"2".to_vec(), b"A".to_vec()],
                    terminator: OscTerminator::St,
                },
                Event::Hook {
                    params: Vec::new(),
                    intermediates: Vec::new(),
                    action: b'q',
                },
                Event::Put(b'x'),
                Event::Unhook,
            ]
        );
    }

    // T5: Streaming UTF-8 across arbitrary 1-byte chunk boundaries.
    #[test]
    fn t5_streaming_utf8_chunk_boundaries() {
        let corpus = "aé中😀\x1b[1;32mHi\x1b[0m\x1b]2;ÜМ😀\x07";
        let bytes = corpus.as_bytes();

        let mut p_bulk = Parser::new();
        let mut r_bulk = Recorder::default();
        p_bulk.advance(bytes, &mut r_bulk);

        let mut p_byte = Parser::new();
        let mut r_byte = Recorder::default();
        for b in bytes {
            p_byte.advance(&[*b], &mut r_byte);
        }

        assert_eq!(r_bulk.events, r_byte.events);
    }

    // T6: Malformed UTF-8 & C0/ESC interrupts.
    #[test]
    fn t6_malformed_utf8_and_interrupts() {
        let mut parser = Parser::new();
        let mut rec = Recorder::default();

        // Overlong sequences.
        parser.advance(
            &[0xC0, 0x80, 0xE0, 0x80, 0xAF, 0xF0, 0x80, 0x80, 0xAF],
            &mut rec,
        );
        assert!(rec.events.iter().all(|e| *e == Event::Print('\u{FFFD}')));
        assert_eq!(rec.events.len(), 9);

        // Surrogates & > U+10FFFF.
        rec.events.clear();
        parser.advance(
            &[
                0xED, 0xA0, 0x80, 0xED, 0xBF, 0xBF, 0xF4, 0x90, 0x80, 0x80, 0xF5, 0x80,
            ],
            &mut rec,
        );
        assert!(rec.events.iter().all(|e| *e == Event::Print('\u{FFFD}')));
        assert_eq!(rec.events.len(), 12);

        // Truncated prefix + ASCII, truncated prefix + new lead, C0/ESC mid-UTF-8.
        rec.events.clear();
        parser.advance(
            &[
                0xE2, 0x82, b' ', 0xE2, 0xC3, 0xA4, 0xE2, 0x82, 0x1B, b'[', b'3', b'1', b'm',
            ],
            &mut rec,
        );
        assert_eq!(
            rec.events,
            vec![
                Event::Print('\u{FFFD}'),
                Event::Print(' '),
                Event::Print('\u{FFFD}'),
                Event::Print('ä'),
                Event::Print('\u{FFFD}'),
                Event::Csi {
                    params: vec![vec![31]],
                    intermediates: Vec::new(),
                    action: b'm',
                    truncated: false,
                },
            ]
        );
    }

    // T7: CSI params & subparams.
    #[test]
    fn t7_csi_params_and_subparams() {
        let mut parser = Parser::new();
        let mut rec = Recorder::default();

        parser.advance(
            b"\x1b[m\x1b[;5;H\x1b[38:2::255:128:0;4:3m\x1b[97:65:97;2:3;65u\x1b[?2026$p\x1b[9999999999m",
            &mut rec,
        );

        assert_eq!(
            rec.events,
            vec![
                Event::Csi {
                    params: Vec::new(),
                    intermediates: Vec::new(),
                    action: b'm',
                    truncated: false,
                },
                Event::Csi {
                    params: vec![vec![0], vec![5], vec![0]],
                    intermediates: Vec::new(),
                    action: b'H',
                    truncated: false,
                },
                Event::Csi {
                    params: vec![vec![38, 2, 0, 255, 128, 0], vec![4, 3]],
                    intermediates: Vec::new(),
                    action: b'm',
                    truncated: false,
                },
                Event::Csi {
                    params: vec![vec![97, 65, 97], vec![2, 3], vec![65]],
                    intermediates: Vec::new(),
                    action: b'u',
                    truncated: false,
                },
                Event::Csi {
                    params: vec![vec![2026]],
                    intermediates: b"?$".to_vec(),
                    action: b'p',
                    truncated: false,
                },
                Event::Csi {
                    params: vec![vec![u16::MAX]],
                    intermediates: Vec::new(),
                    action: b'm',
                    truncated: false,
                },
            ]
        );

        // Verify CsiParams direct getters and DoubleEndedIterator.
        let mut direct = CsiParams::new();
        direct.push_digit(3);
        direct.push_digit(8);
        direct.next_subparam();
        direct.push_digit(5);
        direct.next_param();
        direct.push_digit(1);
        direct.finish();
        assert_eq!(direct.len(), 2);
        assert!(!direct.is_empty());
        assert!(direct.has_subparams());
        assert_eq!(direct.get(0), Some(38));
        assert_eq!(direct.get_or(2, 99), 99);
        assert_eq!(direct.subparams(0), Some(&[38, 5][..]));
        let rev: Vec<&[u16]> = direct.iter().rev().collect();
        assert_eq!(rev, vec![&[1][..], &[38, 5][..]]);
    }

    // T8: OSC & DCS lifecycle.
    #[test]
    fn t8_osc_and_dcs_lifecycle() {
        let mut parser = Parser::new();
        let mut rec = Recorder::default();

        parser.advance(b"\x1b]0;title\x07", &mut rec);
        parser.advance(b"\x1b]8;;https://a;b\x1b", &mut rec);
        parser.advance(b"\\", &mut rec);
        parser.advance(b"\x1b]\x07", &mut rec);

        // >16 OSC params exercises heap fallback path.
        let many = format!(
            "\x1b]{}\x07",
            (0..20).map(|i| i.to_string()).collect::<Vec<_>>().join(";")
        );
        parser.advance(many.as_bytes(), &mut rec);

        assert!(matches!(
            &rec.events[0],
            Event::Osc { params, terminator: OscTerminator::Bel }
            if params == &vec![b"0".to_vec(), b"title".to_vec()]
        ));
        assert!(matches!(
            &rec.events[1],
            Event::Osc { params, terminator: OscTerminator::St }
            if params == &vec![b"8".to_vec(), b"".to_vec(), b"https://a".to_vec(), b"b".to_vec()]
        ));
        assert_eq!(
            rec.events[2],
            Event::Esc {
                intermediates: Vec::new(),
                action: b'\\',
            }
        );
        assert_eq!(
            rec.events[3],
            Event::Osc {
                params: Vec::new(),
                terminator: OscTerminator::Bel,
            }
        );
        assert!(matches!(
            &rec.events[4],
            Event::Osc { params, terminator: OscTerminator::Bel }
            if params.len() == 20
        ));
    }

    // T9: Bounded overflow safety.
    #[test]
    fn t9_bounded_overflow_safety() {
        let mut parser = Parser::new().with_max_osc_bytes(64);
        let mut rec = Recorder::default();

        // > 64 CSI params.
        let mut seq = String::from("\x1b[");
        for i in 0..100 {
            if i > 0 {
                seq.push(';');
            }
            seq.push_str(&i.to_string());
        }
        seq.push('m');
        parser.advance(seq.as_bytes(), &mut rec);
        assert!(matches!(
            rec.events.last(),
            Some(Event::Csi { params, truncated: true, action: b'm', .. })
            if params.len() == MAX_CSI_SLOTS
        ));

        // > 4 intermediates -> CsiIgnore / DcsIgnore / EscapeIntermediate suppressed.
        rec.events.clear();
        parser.advance(b"\x1b     0\x1b[     m\x1bP     qdata\x1b\\ok", &mut rec);
        assert_eq!(
            rec.events,
            vec![
                Event::Esc {
                    intermediates: Vec::new(),
                    action: b'\\',
                },
                Event::Print('o'),
                Event::Print('k'),
            ]
        );

        // > max_osc_bytes -> dropped cleanly, parser recovers to Ground.
        rec.events.clear();
        let big_osc = format!("\x1b]2;{}\x07after", "A".repeat(256));
        parser.advance(big_osc.as_bytes(), &mut rec);
        assert_eq!(
            rec.events,
            vec![
                Event::Print('a'),
                Event::Print('f'),
                Event::Print('t'),
                Event::Print('e'),
                Event::Print('r'),
            ]
        );

        // Verify reset() clears mid-sequence state.
        parser.advance(b"\x1b[31", &mut rec);
        assert_eq!(parser.state(), State::CsiParam);
        parser.reset();
        assert_eq!(parser.state(), State::Ground);
    }

    // T10: SWAR / memchr fast-path parity against 1-byte-at-a-time parsing.
    #[test]
    fn t10_fast_path_parity_with_byte_by_byte() {
        let mut corpus = Vec::new();
        corpus.extend_from_slice(b"Hello, fast-path! ");
        corpus.extend_from_slice(&[b'A'; 73]);
        corpus.extend_from_slice(b"\n\t\x7f");
        corpus.extend_from_slice("中文🚀€Ü".as_bytes());
        corpus.extend_from_slice(b"\x1b[38:2::10:20:30mcolored\x1b[0m");
        corpus.extend_from_slice(b"\x1b]2;title\x07");
        corpus.extend_from_slice(b"\x1bPq");
        corpus.extend_from_slice(&[b'x'; 40]);
        corpus.push(0x7F);
        corpus.extend_from_slice(b"tail\x1b\\");
        corpus.extend_from_slice(b"\x1b_ignored_payload\x1b\\");
        corpus.extend_from_slice(&[b'Z'; 100]);

        let mut fast_parser = Parser::new();
        let mut fast_rec = Recorder::with_fast_path();
        fast_parser.advance(&corpus, &mut fast_rec);

        let mut slow_parser = Parser::new();
        let mut slow_rec = Recorder::default();
        for &b in &corpus {
            slow_parser.advance(&[b], &mut slow_rec);
        }

        assert_eq!(fast_rec.events, slow_rec.events);
        assert!(fast_rec.ascii_runs.iter().any(|r| r.len() >= 73));
        assert!(fast_rec.ascii_runs.iter().any(|r| r.len() == 100));

        // Also test every chunk size 1..=65 on the fast-path.
        for chunk_len in 1..=65 {
            let mut chunk_parser = Parser::new();
            let mut chunk_rec = Recorder::with_fast_path();
            for chunk in corpus.chunks(chunk_len) {
                chunk_parser.advance(chunk, &mut chunk_rec);
            }
            assert_eq!(
                chunk_rec.events, slow_rec.events,
                "mismatch for chunk_len={chunk_len}"
            );
        }
    }

    #[test]
    fn params_get_nonzero_or_and_osc_ranges_shrink() {
        let mut params = CsiParams::new();
        params.push_digit(0);
        params.next_param();
        params.next_param();
        params.push_digit(7);
        params.finish();
        assert_eq!(params.get_or(0, 1), 0);
        assert_eq!(params.get_nonzero_or(0, 1), 1);
        assert_eq!(params.get_nonzero_or(1, 5), 5);
        assert_eq!(params.get_nonzero_or(2, 9), 7);
        assert_eq!(params.get_nonzero_or(3, 4), 4);

        let mut parser = Parser::new();
        let mut rec = Recorder::default();
        let mut osc = b"\x1b]4".to_vec();
        for _ in 0..5000 {
            osc.extend_from_slice(b";1");
        }
        osc.push(0x07);
        parser.advance(&osc, &mut rec);
        assert!(parser.osc_ranges.capacity() <= 256);

        let big_osc = format!("\x1b]2;{}\x07", "X".repeat(80_000));
        parser.advance(big_osc.as_bytes(), &mut rec);
        assert!(parser.osc_buf.capacity() <= 4096);
    }
}
