//! Cursor state, saved cursor (`DECSC`/`DECRC`), attributes, and DEC character sets.

use crate::cell::{CellFlags, HyperlinkId};
use crate::color::PackedColor;
use crate::term::modes::CursorStyle;

/// Character set designation for G0/G1 slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Charset {
    /// Standard US-ASCII / UTF-8 pass-through (`ESC ( B`).
    #[default]
    Ascii,
    /// VT100 DEC Special Graphics line-drawing set (`ESC ( 0`).
    DecSpecialGraphics,
}

impl Charset {
    /// Translates `ch` according to this charset (`0x5F..=0x7E` when [`Self::DecSpecialGraphics`]).
    #[must_use]
    pub const fn map_char(self, ch: char) -> char {
        match self {
            Self::Ascii => ch,
            Self::DecSpecialGraphics => match ch {
                '_' => '\u{00A0}',
                '`' => '\u{25C6}',
                'a' => '\u{2592}',
                'b' => '\u{2409}',
                'c' => '\u{240C}',
                'd' => '\u{240D}',
                'e' => '\u{240A}',
                'f' => '\u{00B0}',
                'g' => '\u{00B1}',
                'h' => '\u{2424}',
                'i' => '\u{240B}',
                'j' => '\u{2518}',
                'k' => '\u{2510}',
                'l' => '\u{250C}',
                'm' => '\u{2514}',
                'n' => '\u{253C}',
                'o' => '\u{23BA}',
                'p' => '\u{23BB}',
                'q' => '\u{2500}',
                'r' => '\u{23BC}',
                's' => '\u{23BD}',
                't' => '\u{251C}',
                'u' => '\u{2524}',
                'v' => '\u{2534}',
                'w' => '\u{252C}',
                'x' => '\u{2502}',
                'y' => '\u{2264}',
                'z' => '\u{2265}',
                '{' => '\u{03C0}',
                '|' => '\u{2260}',
                '}' => '\u{00A3}',
                '~' => '\u{00B7}',
                _ => ch,
            },
        }
    }
}

/// Active G0/G1 charset slots and GL selector (`SO`/`SI`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CharsetSlots {
    /// Designated G0 character set (`ESC ( ...`).
    pub g0: Charset,
    /// Designated G1 character set (`ESC ) ...`).
    pub g1: Charset,
    /// `true` when GL invokes G1 (`SO`, `0x0E`), `false` for G0 (`SI`, `0x0F`).
    pub gl_is_g1: bool,
}

impl CharsetSlots {
    /// Returns the currently active GL character set.
    #[must_use]
    pub const fn active(self) -> Charset {
        if self.gl_is_g1 { self.g1 } else { self.g0 }
    }

    /// Translates `ch` through the active GL character set.
    #[must_use]
    pub const fn translate(self, ch: char) -> char {
        self.active().map_char(ch)
    }
}

/// Current SGR pen attributes applied to newly printed cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CursorAttrs {
    /// Foreground color.
    pub fg: PackedColor,
    /// Background color (also used for Background Color Erase).
    pub bg: PackedColor,
    /// Visual SGR attribute flags (`CellFlags::SGR_MASK`).
    pub flags: CellFlags,
    /// Optional `SGR 58` underline color override.
    pub underline_color: Option<PackedColor>,
    /// Active `OSC 8` hyperlink handle, if any.
    pub hyperlink_id: Option<HyperlinkId>,
}

impl CursorAttrs {
    /// Resets visual SGR attributes (`fg`, `bg`, `flags`, `underline_color`) while preserving
    /// any active `OSC 8` hyperlink.
    pub fn reset_sgr(&mut self) {
        self.fg = PackedColor::Default;
        self.bg = PackedColor::Default;
        self.flags = CellFlags::empty();
        self.underline_color = None;
    }

    /// Resets all cursor attributes including hyperlinks.
    pub fn reset_all(&mut self) {
        self.reset_sgr();
        self.hyperlink_id = None;
    }
}

/// Active terminal cursor state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cursor {
    /// Zero-based live viewport row (`0..rows`).
    pub row: usize,
    /// Zero-based column (`0..cols`).
    pub col: usize,
    /// True when the cursor just printed into the right-most column under `DECAWM`.
    pub pending_wrap: bool,
    /// Active SGR attributes.
    pub attrs: CursorAttrs,
    /// Visual cursor style (`DECSCUSR`).
    pub style: CursorStyle,
    /// Designated G0/G1 character sets.
    pub charsets: CharsetSlots,
}

/// Snapshot of cursor state saved by `DECSC` (`ESC 7` / `CSI s`) per screen buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SavedCursor {
    /// Saved 0-based row.
    pub row: usize,
    /// Saved 0-based column.
    pub col: usize,
    /// Saved SGR attributes.
    pub attrs: CursorAttrs,
    /// Saved `DECOM` origin mode flag.
    pub origin_mode: bool,
    /// Saved `pending_wrap` state.
    pub pending_wrap: bool,
    /// Saved G0/G1 character set designations.
    pub charsets: CharsetSlots,
}
