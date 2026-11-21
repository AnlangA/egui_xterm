//! Cursor state, saved cursor (`DECSC`/`DECRC`), attributes, and DEC character sets.

use crate::cell::{CellFlags, HyperlinkId};
use crate::color::PackedColor;
use crate::term::modes::CursorStyle;

/// Character set designation for G0–G3 slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Charset {
    /// Standard US-ASCII / UTF-8 pass-through (`ESC ( B`).
    #[default]
    Ascii,
    /// VT100 DEC Special Graphics line-drawing set (`ESC ( 0`).
    DecSpecialGraphics,
    /// British National Replacement Character Set (`ESC ( A`, maps `#` to `£`).
    Uk,
}

impl Charset {
    /// Translates `ch` according to this charset (`0x5F..=0x7E` when [`Self::DecSpecialGraphics`],
    /// `#` to `£` when [`Self::Uk`]).
    #[must_use]
    pub const fn map_char(self, ch: char) -> char {
        match self {
            Self::Ascii => ch,
            Self::Uk => {
                if ch == '#' {
                    '\u{00A3}'
                } else {
                    ch
                }
            }
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

#[cfg(feature = "serde")]
const fn default_gr_slot() -> u8 {
    2
}

/// Active G0–G3 charset slots, GL/GR selectors (`SO`/`SI`/`LS2`/`LS3`/`LS1R`–`LS3R`),
/// and pending `SS2`/`SS3` single-shift slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CharsetSlots {
    /// Designated G0 character set (`ESC ( ...`).
    pub g0: Charset,
    /// Designated G1 character set (`ESC ) ...` / `ESC - ...`).
    pub g1: Charset,
    /// Designated G2 character set (`ESC * ...` / `ESC . ...`).
    #[cfg_attr(feature = "serde", serde(default))]
    pub g2: Charset,
    /// Designated G3 character set (`ESC + ...` / `ESC / ...`).
    #[cfg_attr(feature = "serde", serde(default))]
    pub g3: Charset,
    /// Active GL slot index (`0..=3` for G0–G3).
    #[cfg_attr(feature = "serde", serde(default))]
    pub gl: u8,
    /// Active GR slot index (`0..=3` for G0–G3; default `2`).
    #[cfg_attr(feature = "serde", serde(default = "default_gr_slot"))]
    pub gr: u8,
    /// Pending ISO 2022 single-shift slot (`Some(2)` for `SS2`, `Some(3)` for `SS3`).
    #[cfg_attr(feature = "serde", serde(default))]
    pub single_shift: Option<u8>,
}

impl Default for CharsetSlots {
    fn default() -> Self {
        Self {
            g0: Charset::Ascii,
            g1: Charset::Ascii,
            g2: Charset::Ascii,
            g3: Charset::Ascii,
            gl: 0,
            gr: 2,
            single_shift: None,
        }
    }
}

impl CharsetSlots {
    /// Returns the [`Charset`] designated into slot `idx` (`0..=3`).
    #[must_use]
    pub const fn slot(self, idx: u8) -> Charset {
        match idx {
            0 => self.g0,
            1 => self.g1,
            2 => self.g2,
            _ => self.g3,
        }
    }

    /// Returns the currently active GL character set.
    #[must_use]
    pub const fn active(self) -> Charset {
        self.slot(self.gl)
    }

    /// Returns the currently active GR character set.
    #[must_use]
    pub const fn active_gr(self) -> Charset {
        self.slot(self.gr)
    }

    /// Returns `true` when an `SS2`/`SS3` single shift is pending for the next printable character.
    #[must_use]
    pub const fn has_single_shift(self) -> bool {
        self.single_shift.is_some()
    }

    /// Selects GL slot `slot` (`0..=3`).
    pub fn set_gl(&mut self, slot: u8) {
        self.gl = slot.min(3);
    }

    /// Selects GR slot `slot` (`0..=3`).
    pub fn set_gr(&mut self, slot: u8) {
        self.gr = slot.min(3);
    }

    /// Arms a single-shift (`Some(2)` for `SS2`, `Some(3)` for `SS3`).
    pub fn set_single_shift(&mut self, slot: u8) {
        self.single_shift = Some(slot.min(3));
    }

    /// Clears any unconsumed `SS2`/`SS3` single-shift state.
    pub fn clear_single_shift(&mut self) {
        self.single_shift = None;
    }

    /// Translates `ch` through the pending single-shift slot (consuming it) or active GL charset.
    pub fn translate(&mut self, ch: char) -> char {
        if let Some(ss) = self.single_shift.take() {
            return self.slot(ss).map_char(ch);
        }
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
    /// Visual SGR attribute flags (`CellFlags::SGR_MASK`) plus `CellFlags::PROTECTED`.
    pub flags: CellFlags,
    /// Optional `SGR 58` underline color override.
    pub underline_color: Option<PackedColor>,
    /// Active `OSC 8` hyperlink handle, if any.
    pub hyperlink_id: Option<HyperlinkId>,
}

impl CursorAttrs {
    /// Returns `true` when `DECSCA` character protection is active.
    #[must_use]
    pub const fn is_protected(&self) -> bool {
        self.flags.contains(CellFlags::PROTECTED)
    }

    /// Sets or clears `DECSCA` character protection (`CellFlags::PROTECTED`).
    pub fn set_protected(&mut self, protected: bool) {
        self.flags.set(CellFlags::PROTECTED, protected);
    }

    /// Resets visual SGR attributes (`fg`, `bg`, `SGR_MASK`, `underline_color`) while preserving
    /// any active `OSC 8` hyperlink and `DECSCA` protection.
    pub fn reset_sgr(&mut self) {
        self.fg = PackedColor::Default;
        self.bg = PackedColor::Default;
        self.flags &= CellFlags::PROTECTED;
        self.underline_color = None;
    }

    /// Resets all cursor attributes including `DECSCA` protection and hyperlinks.
    pub fn reset_all(&mut self) {
        self.fg = PackedColor::Default;
        self.bg = PackedColor::Default;
        self.flags = CellFlags::empty();
        self.underline_color = None;
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
