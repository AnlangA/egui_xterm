//! Incremental RFC 3629 streaming UTF-8 decoder with maximal-subpart recovery.

/// Controls whether the caller must re-feed the current byte after an invalid sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reprocess {
    /// The current byte itself formed a 1-byte ill-formed subpart and was consumed.
    Consumed,
    /// The previously buffered prefix was the maximal subpart; re-feed the current byte.
    CurrentByte,
}

/// Single-byte progression result returned by [`Utf8Decoder::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Utf8Step {
    /// More continuation bytes are needed to complete the scalar value.
    Incomplete,
    /// Decoded a valid Unicode scalar value.
    Char(char),
    /// Emits `U+FFFD`; check [`Reprocess`] to see whether the current byte was consumed.
    Invalid { reprocess: Reprocess },
}

/// Zero-allocation incremental RFC 3629 UTF-8 state machine.
#[derive(Debug, Clone, Copy, Default)]
pub struct Utf8Decoder {
    codepoint: u32,
    remaining: u8,
    min_next: u8,
    max_next: u8,
}

impl Utf8Decoder {
    /// Returns `true` when no multi-byte UTF-8 sequence is currently in progress.
    #[must_use]
    pub const fn is_idle(self) -> bool {
        self.remaining == 0
    }

    /// Resets any pending incomplete multi-byte state.
    pub fn reset(&mut self) {
        self.codepoint = 0;
        self.remaining = 0;
        self.min_next = 0;
        self.max_next = 0;
    }

    /// Flushes `U+FFFD` if a multi-byte sequence was interrupted before completion.
    pub fn take_incomplete(&mut self) -> Option<char> {
        if self.remaining > 0 {
            self.remaining = 0;
            Some(char::REPLACEMENT_CHARACTER)
        } else {
            None
        }
    }

    /// Feeds one byte into the streaming UTF-8 decoder.
    pub fn push(&mut self, byte: u8) -> Utf8Step {
        if self.remaining == 0 {
            match byte {
                0x00..=0x7F => Utf8Step::Char(char::from(byte)),
                0xC2..=0xDF => {
                    self.codepoint = u32::from(byte & 0x1F);
                    self.remaining = 1;
                    self.min_next = 0x80;
                    self.max_next = 0xBF;
                    Utf8Step::Incomplete
                }
                0xE0 => {
                    self.codepoint = 0;
                    self.remaining = 2;
                    self.min_next = 0xA0;
                    self.max_next = 0xBF;
                    Utf8Step::Incomplete
                }
                0xE1..=0xEC | 0xEE..=0xEF => {
                    self.codepoint = u32::from(byte & 0x0F);
                    self.remaining = 2;
                    self.min_next = 0x80;
                    self.max_next = 0xBF;
                    Utf8Step::Incomplete
                }
                0xED => {
                    self.codepoint = 0x0D;
                    self.remaining = 2;
                    self.min_next = 0x80;
                    self.max_next = 0x9F;
                    Utf8Step::Incomplete
                }
                0xF0 => {
                    self.codepoint = 0;
                    self.remaining = 3;
                    self.min_next = 0x90;
                    self.max_next = 0xBF;
                    Utf8Step::Incomplete
                }
                0xF1..=0xF3 => {
                    self.codepoint = u32::from(byte & 0x07);
                    self.remaining = 3;
                    self.min_next = 0x80;
                    self.max_next = 0xBF;
                    Utf8Step::Incomplete
                }
                0xF4 => {
                    self.codepoint = 0x04;
                    self.remaining = 3;
                    self.min_next = 0x80;
                    self.max_next = 0x8F;
                    Utf8Step::Incomplete
                }
                0x80..=0xBF | 0xC0..=0xC1 | 0xF5..=0xFF => Utf8Step::Invalid {
                    reprocess: Reprocess::Consumed,
                },
            }
        } else if byte >= self.min_next && byte <= self.max_next {
            self.codepoint = (self.codepoint << 6) | u32::from(byte & 0x3F);
            self.remaining -= 1;
            self.min_next = 0x80;
            self.max_next = 0xBF;
            if self.remaining == 0 {
                let ch = char::from_u32(self.codepoint).unwrap_or(char::REPLACEMENT_CHARACTER);
                Utf8Step::Char(ch)
            } else {
                Utf8Step::Incomplete
            }
        } else {
            self.remaining = 0;
            Utf8Step::Invalid {
                reprocess: Reprocess::CurrentByte,
            }
        }
    }
}
