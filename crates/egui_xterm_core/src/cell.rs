//! Compact terminal cell, attribute flags, sparse extra storage, and hyperlink interning.

use std::collections::HashMap;
#[cfg(any(test, feature = "hyperlinks"))]
use std::collections::HashSet;

use crate::color::PackedColor;

/// Maximum UTF-8 byte length accepted for an `OSC 8` hyperlink URI.
pub const MAX_HYPERLINK_URI_BYTES: usize = 8192;

/// Maximum UTF-8 byte length accepted for an `OSC 8` `id=` grouping parameter.
pub const MAX_HYPERLINK_ID_BYTES: usize = 256;

/// SGR underline style encoded in 3 bits of [`CellFlags`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum UnderlineStyle {
    /// No underline.
    #[default]
    None = 0,
    /// Single straight underline (`SGR 4` or `4:1`).
    Single = 1,
    /// Double underline (`SGR 21` or `4:2`).
    Double = 2,
    /// Curly/wavy underline (`SGR 4:3`).
    Curly = 3,
    /// Dotted underline (`SGR 4:4`).
    Dotted = 4,
    /// Dashed underline (`SGR 4:5`).
    Dashed = 5,
}

impl UnderlineStyle {
    /// Maps an `SGR 4:<subparam>` value (`0..=5`) to an [`UnderlineStyle`].
    #[must_use]
    pub const fn from_sgr_subparam(value: u16) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::Single),
            2 => Some(Self::Double),
            3 => Some(Self::Curly),
            4 => Some(Self::Dotted),
            5 => Some(Self::Dashed),
            _ => None,
        }
    }

    /// Returns `true` for any visible underline style.
    #[must_use]
    pub const fn is_underlined(self) -> bool {
        !matches!(self, Self::None)
    }
}

bitflags::bitflags! {
    /// Visual SGR and grid layout flags packed into 32 bits per [`Cell`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct CellFlags: u32 {
        /// Bold text (`SGR 1`).
        const BOLD           = 1 << 0;
        /// Faint / dim text (`SGR 2`).
        const DIM            = 1 << 1;
        /// Italic text (`SGR 3`).
        const ITALIC         = 1 << 2;
        /// Slow blink (`SGR 5`).
        const BLINK_SLOW     = 1 << 3;
        /// Rapid blink (`SGR 6`).
        const BLINK_RAPID    = 1 << 4;
        /// Inverted foreground and background (`SGR 7`).
        const INVERSE        = 1 << 5;
        /// Concealed / hidden text (`SGR 8`).
        const HIDDEN         = 1 << 6;
        /// Crossed-out / strikethrough text (`SGR 9`).
        const STRIKETHROUGH  = 1 << 7;
        /// Overline (`SGR 53`).
        const OVERLINE       = 1 << 8;
        /// Leading cell of a double-width character.
        const WIDE_LEAD      = 1 << 9;
        /// Trailing spacer cell following a double-width character.
        const WIDE_SPACER    = 1 << 10;
        /// Margin spacer inserted when a double-width character wraps atomically.
        const WRAP_SPACER    = 1 << 11;
        /// Least-significant bit of the 3-bit [`UnderlineStyle`] field.
        const UNDERLINE_BIT0 = 1 << 12;
        /// Middle bit of the 3-bit [`UnderlineStyle`] field.
        const UNDERLINE_BIT1 = 1 << 13;
        /// Most-significant bit of the 3-bit [`UnderlineStyle`] field.
        const UNDERLINE_BIT2 = 1 << 14;
    }
}

impl CellFlags {
    const UNDERLINE_SHIFT: u32 = 12;
    const UNDERLINE_MASK: u32 = 0b111 << Self::UNDERLINE_SHIFT;

    /// Mask of all visual SGR flags, excluding grid-layout spacer flags.
    pub const SGR_MASK: Self = Self::from_bits_retain(
        Self::BOLD.bits()
            | Self::DIM.bits()
            | Self::ITALIC.bits()
            | Self::BLINK_SLOW.bits()
            | Self::BLINK_RAPID.bits()
            | Self::INVERSE.bits()
            | Self::HIDDEN.bits()
            | Self::STRIKETHROUGH.bits()
            | Self::OVERLINE.bits()
            | Self::UNDERLINE_MASK,
    );

    /// Extracts the packed [`UnderlineStyle`].
    #[must_use]
    pub const fn underline_style(self) -> UnderlineStyle {
        match (self.bits() & Self::UNDERLINE_MASK) >> Self::UNDERLINE_SHIFT {
            1 => UnderlineStyle::Single,
            2 => UnderlineStyle::Double,
            3 => UnderlineStyle::Curly,
            4 => UnderlineStyle::Dotted,
            5 => UnderlineStyle::Dashed,
            _ => UnderlineStyle::None,
        }
    }

    /// Updates the packed [`UnderlineStyle`] in place without altering other flags.
    pub fn set_underline_style(&mut self, style: UnderlineStyle) {
        *self = self.with_underline_style(style);
    }

    /// Returns a copy of these flags with the [`UnderlineStyle`] replaced.
    #[must_use]
    pub const fn with_underline_style(self, style: UnderlineStyle) -> Self {
        let bits =
            (self.bits() & !Self::UNDERLINE_MASK) | ((style as u32) << Self::UNDERLINE_SHIFT);
        Self::from_bits_retain(bits)
    }
}

/// Compact 20-byte terminal grid cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cell {
    /// Primary Unicode scalar value (`' '` for blank cells).
    pub codepoint: char,
    /// Foreground color.
    pub fg: PackedColor,
    /// Background color.
    pub bg: PackedColor,
    /// Attribute and layout flags.
    pub flags: CellFlags,
    /// 1-based index into sparse [`CellExtra`] storage; `0` means no extra data.
    pub extra_id: u32,
}

const _: () = {
    const fn assert_copy_eq_send_sync<T: Copy + Eq + Send + Sync>() {}
    assert_copy_eq_send_sync::<Cell>();
    assert!(std::mem::size_of::<CellFlags>() == 4);
    assert!(std::mem::size_of::<Cell>() == 20);
    assert!(std::mem::size_of::<Cell>() <= 24);
};

impl Default for Cell {
    fn default() -> Self {
        Self::BLANK
    }
}

impl Cell {
    /// Blank default cell (`' '`, default colors, empty flags, no extra data).
    pub const BLANK: Self = Self::new(
        ' ',
        PackedColor::Default,
        PackedColor::Default,
        CellFlags::empty(),
    );

    /// Constructs a [`Cell`] without sparse extra data (`extra_id = 0`).
    #[must_use]
    pub const fn new(codepoint: char, fg: PackedColor, bg: PackedColor, flags: CellFlags) -> Self {
        Self {
            codepoint,
            fg,
            bg,
            flags,
            extra_id: 0,
        }
    }

    /// Returns `true` if this cell has default blank content and no attributes or extras.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.codepoint == ' '
            && self.fg.is_default()
            && self.bg.is_default()
            && self.flags.is_empty()
            && self.extra_id == 0
    }

    /// Returns `true` if this cell leads a double-width glyph.
    #[must_use]
    pub const fn is_wide_lead(&self) -> bool {
        self.flags.contains(CellFlags::WIDE_LEAD)
    }

    /// Returns `true` if this cell is the trailing spacer of a double-width glyph.
    #[must_use]
    pub const fn is_wide_spacer(&self) -> bool {
        self.flags.contains(CellFlags::WIDE_SPACER)
    }

    /// Returns `true` if this cell is a soft-wrap margin spacer.
    #[must_use]
    pub const fn is_wrap_spacer(&self) -> bool {
        self.flags.contains(CellFlags::WRAP_SPACER)
    }

    /// Returns the cell's [`UnderlineStyle`].
    #[must_use]
    pub const fn underline_style(&self) -> UnderlineStyle {
        self.flags.underline_style()
    }

    /// Returns `true` if this cell references sparse [`CellExtra`] storage.
    #[must_use]
    pub const fn has_extra(&self) -> bool {
        self.extra_id != 0
    }

    /// Resets the cell to `' '` with Background Color Erase (`bg`) and cleared flags/extras.
    pub fn reset(&mut self, bg: PackedColor) {
        self.codepoint = ' ';
        self.fg = PackedColor::Default;
        self.bg = bg;
        self.flags = CellFlags::empty();
        self.extra_id = 0;
    }
}

/// Sparse per-cell extension data for combining marks, underline colors, and hyperlinks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CellExtra {
    /// Additional Unicode scalar values forming a grapheme cluster after [`Cell::codepoint`].
    pub combining: Vec<char>,
    /// Optional `SGR 58` underline color override.
    pub underline_color: Option<PackedColor>,
    /// Optional interned `OSC 8` hyperlink handle.
    pub hyperlink_id: Option<HyperlinkId>,
}

impl CellExtra {
    /// Returns `true` if no combining marks, underline color, or hyperlink are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.combining.is_empty() && self.underline_color.is_none() && self.hyperlink_id.is_none()
    }

    /// Appends a combining scalar value to the cell's grapheme cluster.
    pub fn push_combining(&mut self, ch: char) {
        self.combining.push(ch);
    }

    /// Appends `base` followed by all combining scalar values to `out`.
    pub fn write_grapheme(&self, base: char, out: &mut String) {
        out.push(base);
        for &ch in &self.combining {
            out.push(ch);
        }
    }
}

/// Compact handle referencing an interned [`Hyperlink`] inside a [`HyperlinkPool`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HyperlinkId(u32);

impl HyperlinkId {
    /// Returns the zero-based slot index as `usize`.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Returns the raw `u32` identifier.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    #[cfg(all(test, feature = "serde"))]
    pub(crate) const fn from_raw_for_test(raw: u32) -> Self {
        Self(raw)
    }
}

/// Interned `OSC 8` hyperlink target and optional grouping identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Hyperlink {
    /// Optional `id=` parameter from `OSC 8;id=...;URI`.
    pub id: Option<String>,
    /// Destination URI string.
    pub uri: String,
}

/// Deduplicating pool of [`Hyperlink`] entries keyed by `(id, uri)` with slot-based reclamation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HyperlinkPool {
    slots: Vec<Option<Hyperlink>>,
    free_slots: Vec<u32>,
    live_count: usize,
    by_uri: HashMap<String, Vec<HyperlinkId>>,
}

impl HyperlinkPool {
    /// Creates an empty [`HyperlinkPool`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Validates `id` and `uri` bounds/control safety and interns `(id, uri)` on success.
    ///
    /// Returns `None` if `uri` is empty, exceeds [`MAX_HYPERLINK_URI_BYTES`], `id` exceeds
    /// [`MAX_HYPERLINK_ID_BYTES`], or either contains control characters.
    pub fn try_intern(&mut self, id: Option<&str>, uri: &str) -> Option<HyperlinkId> {
        if uri.is_empty()
            || uri.len() > MAX_HYPERLINK_URI_BYTES
            || uri.chars().any(char::is_control)
            || id.is_some_and(|s| {
                s.len() > MAX_HYPERLINK_ID_BYTES || s.chars().any(char::is_control)
            })
        {
            return None;
        }
        Some(self.intern(id, uri))
    }

    /// Interns `(id, uri)` and returns a stable [`HyperlinkId`], allocating only on cache miss.
    ///
    /// # Panics
    ///
    /// Panics if more than `u32::MAX` distinct hyperlinks are interned in a single pool.
    pub fn intern(&mut self, id: Option<&str>, uri: &str) -> HyperlinkId {
        if let Some(candidates) = self.by_uri.get(uri) {
            for &hid in candidates {
                if self
                    .slots
                    .get(hid.index())
                    .and_then(Option::as_ref)
                    .is_some_and(|link| link.id.as_deref() == id)
                {
                    return hid;
                }
            }
        }

        let entry = Hyperlink {
            id: id.map(str::to_owned),
            uri: uri.to_owned(),
        };
        let hid = if let Some(slot_idx) = self.free_slots.pop() {
            self.slots[slot_idx as usize] = Some(entry);
            HyperlinkId(slot_idx)
        } else {
            let raw_id =
                u32::try_from(self.slots.len()).expect("hyperlink pool capacity exceeded u32::MAX");
            self.slots.push(Some(entry));
            HyperlinkId(raw_id)
        };
        self.live_count += 1;
        self.by_uri.entry(uri.to_owned()).or_default().push(hid);
        hid
    }

    /// Reclaims any interned hyperlinks not present in `live`.
    #[cfg(any(test, feature = "hyperlinks"))]
    pub(crate) fn gc_unreferenced(&mut self, live: &HashSet<HyperlinkId>) {
        for (idx, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_some() {
                let raw = u32::try_from(idx).expect("slot index fits in u32");
                let hid = HyperlinkId(raw);
                if !live.contains(&hid) {
                    *slot = None;
                    self.free_slots.push(raw);
                    self.live_count = self.live_count.saturating_sub(1);
                }
            }
        }
        self.by_uri.retain(|_, ids| {
            ids.retain(|id| live.contains(id));
            !ids.is_empty()
        });
        if self.live_count == 0 {
            self.slots.clear();
            self.free_slots.clear();
            self.by_uri.clear();
        }
    }

    /// Looks up the [`Hyperlink`] for `id`.
    #[must_use]
    pub fn get(&self, id: HyperlinkId) -> Option<&Hyperlink> {
        self.slots.get(id.index()).and_then(Option::as_ref)
    }

    /// Returns the URI string for `id`, if present.
    #[must_use]
    pub fn uri(&self, id: HyperlinkId) -> Option<&str> {
        self.get(id).map(|h| h.uri.as_str())
    }

    /// Returns the number of currently live interned hyperlinks.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.live_count
    }

    /// Returns `true` if no live hyperlinks are stored.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.live_count == 0
    }

    /// Removes all interned hyperlinks from the pool.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free_slots.clear();
        self.live_count = 0;
        self.by_uri.clear();
    }

    /// Rebuilds internal `free_slots`, `live_count`, and `by_uri` invariants after deserialization
    /// and discards any invalid or out-of-bounds hyperlink entries.
    #[cfg(feature = "serde")]
    pub(crate) fn sanitize(&mut self) {
        for slot in &mut self.slots {
            let valid = slot.as_ref().is_some_and(|link| {
                !link.uri.is_empty()
                    && link.uri.len() <= MAX_HYPERLINK_URI_BYTES
                    && !link.uri.chars().any(char::is_control)
                    && link.id.as_deref().is_none_or(|id| {
                        id.len() <= MAX_HYPERLINK_ID_BYTES && !id.chars().any(char::is_control)
                    })
            });
            if !valid {
                *slot = None;
            }
        }

        self.free_slots.clear();
        self.by_uri.clear();
        self.live_count = 0;

        for (idx, slot) in self.slots.iter_mut().enumerate() {
            let Ok(raw_id) = u32::try_from(idx) else {
                *slot = None;
                continue;
            };
            if let Some(link) = slot {
                self.live_count += 1;
                self.by_uri
                    .entry(link.uri.clone())
                    .or_default()
                    .push(HyperlinkId(raw_id));
            } else {
                self.free_slots.push(raw_id);
            }
        }

        if self.live_count == 0 {
            self.slots.clear();
            self.free_slots.clear();
            self.by_uri.clear();
        }
    }

    #[cfg(all(test, feature = "serde"))]
    pub(crate) fn corrupt_for_test(&mut self) {
        self.free_slots.push(9999);
        self.live_count = 42;
        self.slots.push(Some(Hyperlink {
            id: None,
            uri: "bad\x07uri".to_owned(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_size_and_copy_invariants() {
        assert_eq!(std::mem::size_of::<Cell>(), 20);
        assert!(std::mem::size_of::<Cell>() <= 24);
        let a = Cell::BLANK;
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn cell_default_blank_and_reset() {
        assert_eq!(Cell::default(), Cell::BLANK);
        assert!(Cell::BLANK.is_empty());

        let mut c = Cell::new(
            'a',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::empty(),
        );
        assert!(!c.is_empty());

        c = Cell::new(
            ' ',
            PackedColor::indexed(1),
            PackedColor::Default,
            CellFlags::empty(),
        );
        assert!(!c.is_empty());

        c = Cell::new(
            ' ',
            PackedColor::Default,
            PackedColor::indexed(2),
            CellFlags::empty(),
        );
        assert!(!c.is_empty());

        c = Cell::new(
            ' ',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::BOLD,
        );
        assert!(!c.is_empty());

        c = Cell::BLANK;
        c.extra_id = 1;
        assert!(!c.is_empty());

        c.flags = CellFlags::BOLD | CellFlags::ITALIC;
        c.fg = PackedColor::rgb(1, 2, 3);
        c.reset(PackedColor::indexed(4));
        assert_eq!(c.codepoint, ' ');
        assert_eq!(c.fg, PackedColor::Default);
        assert_eq!(c.bg, PackedColor::indexed(4));
        assert_eq!(c.flags, CellFlags::empty());
        assert_eq!(c.extra_id, 0);
    }

    #[test]
    fn underline_style_bit_packing() {
        let styles = [
            UnderlineStyle::None,
            UnderlineStyle::Single,
            UnderlineStyle::Double,
            UnderlineStyle::Curly,
            UnderlineStyle::Dotted,
            UnderlineStyle::Dashed,
        ];
        let base = CellFlags::BOLD
            | CellFlags::DIM
            | CellFlags::ITALIC
            | CellFlags::BLINK_SLOW
            | CellFlags::BLINK_RAPID
            | CellFlags::INVERSE
            | CellFlags::HIDDEN
            | CellFlags::STRIKETHROUGH
            | CellFlags::OVERLINE
            | CellFlags::WIDE_LEAD
            | CellFlags::WIDE_SPACER
            | CellFlags::WRAP_SPACER;

        for style in styles {
            let mut flags = base.with_underline_style(style);
            assert_eq!(flags.underline_style(), style);
            assert!(flags.contains(base));
            flags.set_underline_style(UnderlineStyle::Curly);
            assert_eq!(flags.underline_style(), UnderlineStyle::Curly);
            assert!(flags.contains(base));
            assert!(flags.underline_style().is_underlined());
        }
        assert!(!UnderlineStyle::None.is_underlined());

        assert_eq!(
            UnderlineStyle::from_sgr_subparam(0),
            Some(UnderlineStyle::None)
        );
        assert_eq!(
            UnderlineStyle::from_sgr_subparam(1),
            Some(UnderlineStyle::Single)
        );
        assert_eq!(
            UnderlineStyle::from_sgr_subparam(2),
            Some(UnderlineStyle::Double)
        );
        assert_eq!(
            UnderlineStyle::from_sgr_subparam(3),
            Some(UnderlineStyle::Curly)
        );
        assert_eq!(
            UnderlineStyle::from_sgr_subparam(4),
            Some(UnderlineStyle::Dotted)
        );
        assert_eq!(
            UnderlineStyle::from_sgr_subparam(5),
            Some(UnderlineStyle::Dashed)
        );
        assert_eq!(UnderlineStyle::from_sgr_subparam(6), None);
    }

    #[test]
    fn cell_flags_wide_and_extra_helpers() {
        let mut cell = Cell::new(
            '中',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::WIDE_LEAD | CellFlags::BOLD,
        );
        assert!(cell.is_wide_lead());
        assert!(!cell.is_wide_spacer());
        assert!(!cell.is_wrap_spacer());
        assert!(!cell.has_extra());
        assert_eq!(cell.underline_style(), UnderlineStyle::None);

        cell.flags = CellFlags::WIDE_SPACER;
        assert!(cell.is_wide_spacer());
        cell.flags = CellFlags::WRAP_SPACER;
        assert!(cell.is_wrap_spacer());
        cell.extra_id = 1;
        assert!(cell.has_extra());

        assert!(!CellFlags::SGR_MASK.contains(CellFlags::WIDE_LEAD));
        assert!(!CellFlags::SGR_MASK.contains(CellFlags::WIDE_SPACER));
        assert!(!CellFlags::SGR_MASK.contains(CellFlags::WRAP_SPACER));
        assert!(CellFlags::SGR_MASK.contains(CellFlags::BOLD | CellFlags::OVERLINE));
    }

    #[test]
    fn cell_extra_and_write_grapheme() {
        let mut extra = CellExtra::default();
        assert!(extra.is_empty());
        extra.push_combining('\u{0301}');
        assert!(!extra.is_empty());

        let mut out = String::new();
        extra.write_grapheme('e', &mut out);
        assert_eq!(out, "e\u{0301}");

        extra.combining.clear();
        extra.underline_color = Some(PackedColor::rgb(255, 0, 0));
        assert!(!extra.is_empty());
    }

    #[test]
    fn hyperlink_pool_dedup_and_lookup() {
        let mut pool = HyperlinkPool::new();
        assert!(pool.is_empty());

        let a1 = pool.intern(Some("id1"), "https://a.test");
        let a2 = pool.intern(Some("id1"), "https://a.test");
        assert_eq!(a1, a2);
        assert_eq!(pool.len(), 1);
        assert_eq!(a1.as_u32(), 0);

        let b = pool.intern(None, "https://a.test");
        let c = pool.intern(Some("id2"), "https://a.test");
        assert_ne!(a1, b);
        assert_ne!(a1, c);
        assert_ne!(b, c);
        assert_eq!(pool.len(), 3);

        assert_eq!(
            pool.get(a1),
            Some(&Hyperlink {
                id: Some("id1".to_owned()),
                uri: "https://a.test".to_owned(),
            })
        );
        assert_eq!(pool.uri(b), Some("https://a.test"));

        pool.clear();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
        assert!(pool.get(a1).is_none());
    }

    #[test]
    fn hyperlink_pool_try_intern_guards_and_gc_reuse() {
        let mut pool = HyperlinkPool::new();
        assert!(pool.try_intern(None, "").is_none());
        assert!(pool.try_intern(None, "https://a.test\x07").is_none());
        assert!(
            pool.try_intern(Some("bad\x1bid"), "https://a.test")
                .is_none()
        );

        let long_uri = format!(
            "https://example.com/{}",
            "a".repeat(MAX_HYPERLINK_URI_BYTES)
        );
        assert!(pool.try_intern(None, &long_uri).is_none());

        let long_id = "i".repeat(MAX_HYPERLINK_ID_BYTES + 1);
        assert!(pool.try_intern(Some(&long_id), "https://a.test").is_none());

        let id0 = pool.try_intern(Some("g1"), "https://one.test").unwrap();
        let id1 = pool.try_intern(None, "https://two.test").unwrap();
        let id2 = pool.try_intern(None, "https://three.test").unwrap();
        assert_eq!(pool.len(), 3);

        let mut live = HashSet::new();
        live.insert(id1);
        pool.gc_unreferenced(&live);
        assert_eq!(pool.len(), 1);
        assert!(pool.get(id0).is_none());
        assert_eq!(pool.uri(id1), Some("https://two.test"));
        assert!(pool.get(id2).is_none());

        // Freed slot is reused and interning previously evicted URI allocates cleanly.
        let id_reused = pool.try_intern(Some("g1"), "https://one.test").unwrap();
        assert_ne!(id_reused, id1);
        assert_eq!(pool.len(), 2);
        assert_eq!(pool.uri(id_reused), Some("https://one.test"));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_bounds_cell() {
        const fn assert_serde<T: serde::Serialize + for<'de> serde::Deserialize<'de>>() {}
        assert_serde::<UnderlineStyle>();
        assert_serde::<CellFlags>();
        assert_serde::<Cell>();
        assert_serde::<CellExtra>();
        assert_serde::<HyperlinkId>();
        assert_serde::<Hyperlink>();
        assert_serde::<HyperlinkPool>();
    }
}
