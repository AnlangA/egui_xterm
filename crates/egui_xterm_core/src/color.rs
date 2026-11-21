//! Terminal color representation and xterm 16/256/`TrueColor` palette resolution.

/// Compact 4-byte terminal color value with niche optimization for `Option<PackedColor>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PackedColor {
    /// Inherit the theme's default foreground or background color.
    #[default]
    Default,
    /// Xterm 256-color palette index (`0..=15` ANSI, `16..=231` 6x6x6 cube, `232..=255` gray).
    Indexed(u8),
    /// 24-bit `TrueColor` RGB triplet.
    Rgb(u8, u8, u8),
}

const _: () = {
    assert!(std::mem::size_of::<PackedColor>() == 4);
    assert!(std::mem::size_of::<Option<PackedColor>>() == 4);
};

impl PackedColor {
    /// Alias for [`PackedColor::Default`].
    pub const DEFAULT: Self = Self::Default;

    /// Constructs an indexed xterm 256-color value.
    #[must_use]
    pub const fn indexed(index: u8) -> Self {
        Self::Indexed(index)
    }

    /// Constructs a 24-bit RGB color value.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::Rgb(r, g, b)
    }

    /// Returns `true` if this color is [`PackedColor::Default`].
    #[must_use]
    pub const fn is_default(self) -> bool {
        matches!(self, Self::Default)
    }

    /// Promotes standard ANSI colors (`0..=7`) to their bright variants (`8..=15`).
    #[must_use]
    pub const fn to_bright_ansi(self) -> Self {
        match self {
            Self::Indexed(idx @ 0..=7) => Self::Indexed(idx + 8),
            other => other,
        }
    }

    /// Resolves this color to `[r, g, b]` using `default_rgb` for [`Self::Default`] and
    /// `palette` for indexed entries.
    #[must_use]
    pub const fn resolve(self, default_rgb: [u8; 3], palette: &ColorScheme) -> [u8; 3] {
        match self {
            Self::Default => default_rgb,
            Self::Indexed(idx) => palette.color(idx),
            Self::Rgb(r, g, b) => [r, g, b],
        }
    }

    /// Resolves this color as a foreground color against `palette`.
    #[must_use]
    pub const fn resolve_fg(self, palette: &ColorScheme) -> [u8; 3] {
        self.resolve(palette.foreground, palette)
    }

    /// Resolves this color as a background color against `palette`.
    #[must_use]
    pub const fn resolve_bg(self, palette: &ColorScheme) -> [u8; 3] {
        self.resolve(palette.background, palette)
    }

    /// Resolves this color to a `(r, g, b)` tuple using `default_rgb` and `palette`.
    #[must_use]
    pub const fn resolve_tuple(
        self,
        default_rgb: (u8, u8, u8),
        palette: &ColorScheme,
    ) -> (u8, u8, u8) {
        let [r, g, b] = self.resolve([default_rgb.0, default_rgb.1, default_rgb.2], palette);
        (r, g, b)
    }
}

/// Terminal color palette defining default colors, selection highlight, and 16 ANSI slots.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ColorScheme {
    /// Default foreground RGB color.
    pub foreground: [u8; 3],
    /// Default background RGB color.
    pub background: [u8; 3],
    /// Cursor fill RGB color.
    pub cursor: [u8; 3],
    /// Text RGB color painted inside a solid block cursor.
    pub cursor_text: [u8; 3],
    /// Selection highlight background RGB color.
    pub selection_bg: [u8; 3],
    /// Optional override RGB color for selected text; `None` preserves cell foregrounds.
    pub selection_fg: Option<[u8; 3]>,
    /// The 16 base ANSI colors (`0..=7` normal, `8..=15` bright).
    pub ansi: [[u8; 3]; 16],
}

impl Default for ColorScheme {
    fn default() -> Self {
        Self::dark()
    }
}

impl ColorScheme {
    /// Channel intensity levels for the xterm 6x6x6 color cube (`16..=231`).
    pub const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

    /// Computes the `[r, g, b]` color for xterm 256-color `index` given a 16-entry ANSI table.
    #[must_use]
    pub const fn xterm_256_color(index: u8, ansi: &[[u8; 3]; 16]) -> [u8; 3] {
        match index {
            0..=15 => ansi[index as usize],
            16..=231 => {
                let idx = index - 16;
                let r = Self::CUBE_LEVELS[(idx / 36) as usize];
                let g = Self::CUBE_LEVELS[((idx / 6) % 6) as usize];
                let b = Self::CUBE_LEVELS[(idx % 6) as usize];
                [r, g, b]
            }
            232..=255 => {
                let v = 8 + 10 * (index - 232);
                [v, v, v]
            }
        }
    }

    /// Resolves xterm 256-color `index` against this scheme's 16 ANSI colors.
    #[must_use]
    pub const fn color(&self, index: u8) -> [u8; 3] {
        Self::xterm_256_color(index, &self.ansi)
    }

    /// Resolves a `(fg, bg)` [`PackedColor`] pair into `[r, g, b]` triplets.
    #[must_use]
    pub const fn resolve_pair(&self, fg: PackedColor, bg: PackedColor) -> ([u8; 3], [u8; 3]) {
        (
            fg.resolve(self.foreground, self),
            bg.resolve(self.background, self),
        )
    }

    /// Builds a complete 256-entry RGB lookup table from this scheme.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "loop counter i is bounded strictly below 256"
    )]
    pub const fn build_256_palette(&self) -> [[u8; 3]; 256] {
        let mut table = [[0, 0, 0]; 256];
        let mut i = 0usize;
        while i < 256 {
            table[i] = self.color(i as u8);
            i += 1;
        }
        table
    }

    /// Modern dark terminal color scheme preset.
    #[must_use]
    pub const fn dark() -> Self {
        Self {
            foreground: [224, 228, 236],
            background: [20, 22, 28],
            cursor: [244, 191, 117],
            cursor_text: [20, 22, 28],
            selection_bg: [58, 76, 110],
            selection_fg: None,
            ansi: [
                [28, 31, 38],
                [224, 108, 117],
                [152, 195, 121],
                [229, 192, 123],
                [97, 175, 239],
                [198, 120, 221],
                [86, 182, 194],
                [192, 197, 206],
                [92, 99, 112],
                [242, 139, 146],
                [175, 217, 144],
                [245, 212, 145],
                [130, 196, 248],
                [216, 148, 235],
                [114, 207, 219],
                [242, 244, 248],
            ],
        }
    }

    /// High-contrast light terminal color scheme preset.
    #[must_use]
    pub const fn light() -> Self {
        Self {
            foreground: [36, 41, 47],
            background: [250, 250, 248],
            cursor: [36, 41, 47],
            cursor_text: [250, 250, 248],
            selection_bg: [188, 214, 245],
            selection_fg: None,
            ansi: [
                [36, 41, 47],
                [207, 34, 46],
                [26, 127, 55],
                [154, 103, 0],
                [9, 105, 218],
                [130, 80, 223],
                [27, 124, 131],
                [110, 119, 129],
                [87, 96, 106],
                [224, 62, 74],
                [45, 164, 78],
                [191, 135, 0],
                [33, 139, 255],
                [164, 117, 249],
                [49, 155, 163],
                [246, 248, 250],
            ],
        }
    }
}

/// Terminal visual theme combining a [`ColorScheme`] with presentation options.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Theme {
    /// Base color palette for foreground, background, cursor, selection, and 16 ANSI colors.
    pub scheme: ColorScheme,
    /// When `true`, bold text with ANSI foreground `0..=7` promotes to bright `8..=15`.
    pub bold_is_bright: bool,
}

/// Special SGR attribute color overrides configured via `OSC 5` / `OSC 105`.
///
/// Register indices follow xterm `OSC 5`: `0` = bold, `1` = underline, `2` = blink,
/// `3` = reverse, `4` = italic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpecialColors {
    /// Bold attribute foreground override (`OSC 5 ; 0`).
    pub bold: Option<[u8; 3]>,
    /// Underline attribute foreground override (`OSC 5 ; 1`).
    pub underline: Option<[u8; 3]>,
    /// Blink attribute foreground override (`OSC 5 ; 2`).
    pub blink: Option<[u8; 3]>,
    /// Reverse attribute color override (`OSC 5 ; 3`).
    pub reverse: Option<[u8; 3]>,
    /// Italic attribute foreground override (`OSC 5 ; 4`).
    pub italic: Option<[u8; 3]>,
}

impl SpecialColors {
    /// Returns the special color override at register `index` (`0..=4`), or `None` if out of range.
    #[must_use]
    pub const fn get(&self, index: usize) -> Option<[u8; 3]> {
        match index {
            0 => self.bold,
            1 => self.underline,
            2 => self.blink,
            3 => self.reverse,
            4 => self.italic,
            _ => None,
        }
    }

    /// Sets or clears register `index` (`0..=4`), returning `true` if `index <= 4` and the value changed.
    pub fn set(&mut self, index: usize, value: Option<[u8; 3]>) -> bool {
        let slot = match index {
            0 => &mut self.bold,
            1 => &mut self.underline,
            2 => &mut self.blink,
            3 => &mut self.reverse,
            4 => &mut self.italic,
            _ => return false,
        };
        if *slot == value {
            return false;
        }
        *slot = value;
        true
    }

    /// Clears all five special color registers, returning `true` if any was previously set.
    pub fn clear(&mut self) -> bool {
        let any_set = self.bold.is_some()
            || self.underline.is_some()
            || self.blink.is_some()
            || self.reverse.is_some()
            || self.italic.is_some();
        *self = Self::default();
        any_set
    }

    /// Converts all five registers (`0..=4`) into a fixed array for [`RenderSnapshot`](crate::RenderSnapshot).
    #[must_use]
    pub const fn as_array(&self) -> [Option<[u8; 3]>; 5] {
        [
            self.bold,
            self.underline,
            self.blink,
            self.reverse,
            self.italic,
        ]
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl From<ColorScheme> for Theme {
    fn from(scheme: ColorScheme) -> Self {
        Self::from_scheme(scheme)
    }
}

impl Theme {
    /// Creates a [`Theme`] wrapping `scheme` with `bold_is_bright` enabled.
    #[must_use]
    pub const fn from_scheme(scheme: ColorScheme) -> Self {
        Self {
            scheme,
            bold_is_bright: true,
        }
    }

    /// Modern dark terminal theme preset.
    #[must_use]
    pub const fn dark() -> Self {
        Self::from_scheme(ColorScheme::dark())
    }

    /// High-contrast light terminal theme preset.
    #[must_use]
    pub const fn light() -> Self {
        Self::from_scheme(ColorScheme::light())
    }

    /// Resolves foreground color `fg`, promoting `0..=7` to `8..=15` when `is_bold && self.bold_is_bright`.
    #[must_use]
    pub const fn resolve_fg(&self, fg: PackedColor, is_bold: bool) -> [u8; 3] {
        let effective = if is_bold && self.bold_is_bright {
            fg.to_bright_ansi()
        } else {
            fg
        };
        effective.resolve_fg(&self.scheme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_color_size_and_niche() {
        assert_eq!(std::mem::size_of::<PackedColor>(), 4);
        assert_eq!(std::mem::size_of::<Option<PackedColor>>(), 4);
    }

    #[test]
    fn packed_color_resolve_default_indexed_rgb() {
        let scheme = ColorScheme::dark();
        assert!(PackedColor::DEFAULT.is_default());
        assert!(!PackedColor::indexed(1).is_default());
        assert!(!PackedColor::rgb(1, 2, 3).is_default());

        assert_eq!(PackedColor::Default.resolve_fg(&scheme), scheme.foreground);
        assert_eq!(PackedColor::Default.resolve_bg(&scheme), scheme.background);
        assert_eq!(PackedColor::indexed(1).resolve_fg(&scheme), scheme.ansi[1]);
        assert_eq!(
            PackedColor::rgb(10, 20, 30).resolve_fg(&scheme),
            [10, 20, 30]
        );
        assert_eq!(
            PackedColor::Default.resolve_tuple((1, 2, 3), &scheme),
            (1, 2, 3)
        );

        assert_eq!(
            PackedColor::indexed(3).to_bright_ansi(),
            PackedColor::indexed(11)
        );
        assert_eq!(
            PackedColor::indexed(8).to_bright_ansi(),
            PackedColor::indexed(8)
        );
        assert_eq!(
            PackedColor::indexed(200).to_bright_ansi(),
            PackedColor::indexed(200)
        );
        assert_eq!(PackedColor::Default.to_bright_ansi(), PackedColor::Default);
        assert_eq!(
            PackedColor::rgb(1, 2, 3).to_bright_ansi(),
            PackedColor::rgb(1, 2, 3)
        );
    }

    #[test]
    fn color_scheme_256_palette_derivation() {
        let scheme = ColorScheme::dark();
        for i in 0..16u8 {
            assert_eq!(scheme.color(i), scheme.ansi[i as usize]);
        }
        assert_eq!(scheme.color(16), [0, 0, 0]);
        assert_eq!(scheme.color(17), [0, 0, 95]);
        assert_eq!(scheme.color(21), [0, 0, 255]);
        assert_eq!(scheme.color(196), [255, 0, 0]);
        assert_eq!(scheme.color(231), [255, 255, 255]);
        assert_eq!(scheme.color(232), [8, 8, 8]);
        assert_eq!(scheme.color(255), [238, 238, 238]);

        let palette = scheme.build_256_palette();
        for i in 0..=255u8 {
            assert_eq!(palette[i as usize], scheme.color(i));
        }

        assert_eq!(
            scheme.resolve_pair(PackedColor::Default, PackedColor::rgb(1, 2, 3)),
            (scheme.foreground, [1, 2, 3])
        );
    }

    #[test]
    fn color_scheme_dark_and_light_presets() {
        fn luma(rgb: [u8; 3]) -> u32 {
            u32::from(rgb[0]) * 299 + u32::from(rgb[1]) * 587 + u32::from(rgb[2]) * 114
        }

        let dark = ColorScheme::dark();
        let light = ColorScheme::light();
        assert_eq!(ColorScheme::default(), dark);
        assert!(luma(dark.background) < luma(dark.foreground));
        assert!(luma(light.background) > luma(light.foreground));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_bounds_color() {
        const fn assert_serde<T: serde::Serialize + for<'de> serde::Deserialize<'de>>() {}
        assert_serde::<PackedColor>();
        assert_serde::<ColorScheme>();
        assert_serde::<Theme>();
    }

    #[test]
    fn theme_presets_and_bold_bright() {
        let dark = Theme::dark();
        assert_eq!(Theme::default(), dark);
        assert!(dark.bold_is_bright);
        assert_eq!(dark.scheme, ColorScheme::dark());

        let light = Theme::light();
        assert_eq!(light.scheme, ColorScheme::light());

        let from_scheme: Theme = ColorScheme::light().into();
        assert_eq!(from_scheme, light);

        assert_eq!(
            dark.resolve_fg(PackedColor::indexed(1), true),
            dark.scheme.ansi[9]
        );
        let mut no_bright = dark.clone();
        no_bright.bold_is_bright = false;
        assert_eq!(
            no_bright.resolve_fg(PackedColor::indexed(1), true),
            dark.scheme.ansi[1]
        );
    }
}
