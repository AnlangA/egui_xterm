//! Terminal ANSI/DEC modes, cursor style, and mouse protocol/encoding state.

#[cfg(feature = "serde")]
const fn default_true() -> bool {
    true
}

/// Visual cursor geometry configured by `DECSCUSR` or [`CoreConfig`](super::CoreConfig).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CursorShape {
    /// Solid or hollow rectangle block cursor (`DECSCUSR 0/1/2`).
    #[default]
    Block,
    /// Horizontal bottom underline cursor (`DECSCUSR 3/4`).
    Underline,
    /// Vertical thin beam cursor (`DECSCUSR 5/6`).
    Bar,
}

/// Combined cursor shape and blink state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CursorStyle {
    /// Cursor geometry.
    pub shape: CursorShape,
    /// Whether the cursor blinks when focused.
    pub blink: bool,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blink: true,
        }
    }
}

/// Active mouse reporting protocol negotiated via DECSET/DECRST (`?9/?1000/?1002/?1003`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MouseProtocol {
    /// Mouse reporting disabled.
    #[default]
    None,
    /// X10 compatibility mode (`CSI ? 9 h`): button press only, no modifiers.
    X10,
    /// Normal tracking (`CSI ? 1000 h`): press, release, and wheel.
    Normal,
    /// Button-event tracking (`CSI ? 1002 h`): press, release, wheel, and drag motion.
    ButtonEvent,
    /// Any-event tracking (`CSI ? 1003 h`): all presses, releases, wheel, and hover motion.
    AnyEvent,
}

/// Wire encoding used for mouse reports (`?1005` / `?1006` / `?1016`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MouseEncoding {
    /// Legacy 6-byte `CSI M Cb Cx Cy` encoding (`+32` offset, max coordinate 223).
    #[default]
    Default,
    /// UTF-8 extended coordinate encoding (`CSI ? 1005 h`).
    Utf8,
    /// SGR extended decimal encoding (`CSI ? 1006 h`): `CSI < Cb ; Cx ; Cy M/m`.
    Sgr,
    /// SGR-Pixels extended decimal encoding (`CSI ? 1016 h`): `CSI < Cb ; Px ; Py M/m`.
    SgrPixels,
}

/// Complete ANSI and DEC private mode state for a [`Terminal`](super::Terminal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[expect(
    clippy::struct_excessive_bools,
    reason = "terminal ANSI/DEC private modes are independent boolean flags"
)]
pub struct Modes {
    /// ANSI `IRM` (`CSI 4 h/l`): insert characters instead of overwriting.
    pub insert_mode: bool,
    /// ANSI `LNM` (`CSI 20 h/l`): linefeed implies carriage return; Return sends `\r\n`.
    pub linefeed_newline: bool,
    /// DEC `DECCKM` (`CSI ? 1 h/l`): application cursor keys.
    pub app_cursor_keys: bool,
    /// DEC `DECCOLM` (`CSI ? 3 h/l`): 132-column vs 80-column mode flag.
    pub column_132: bool,
    /// DEC `DECSCNM` (`CSI ? 5 h/l`): screen-wide reverse video.
    pub reverse_video: bool,
    /// DEC `DECOM` (`CSI ? 6 h/l`): origin mode relative to scroll margins.
    pub origin_mode: bool,
    /// DEC `DECAWM` (`CSI ? 7 h/l`): auto-wrap at right margin.
    pub auto_wrap: bool,
    /// DEC `ATT610` (`CSI ? 12 h/l`): cursor blink enable.
    pub cursor_blink: bool,
    /// DEC `DECTCEM` (`CSI ? 25 h/l`): cursor visibility.
    pub cursor_visible: bool,
    /// DEC `DECSDM` (`CSI ? 80 h/l`): sixel display mode (disables sixel scrolling when set).
    pub sixel_display_mode: bool,
    /// DEC `?1070`: private color registers per Sixel image (default: `true`).
    #[cfg_attr(feature = "serde", serde(default = "default_true"))]
    pub sixel_private_color_registers: bool,
    /// DEC `?8452`: position cursor to the right of the last Sixel row instead of next line.
    #[cfg_attr(feature = "serde", serde(default))]
    pub sixel_cursor_right: bool,
    /// Active alternate screen buffer (`?47`, `?1047`, `?1049`).
    pub alt_screen: bool,
    /// Active mouse reporting protocol (`?9`, `?1000`, `?1002`, `?1003`).
    pub mouse_protocol: MouseProtocol,
    /// Active mouse coordinate encoding (`?1005`, `?1006`).
    pub mouse_encoding: MouseEncoding,
    /// DEC `?1004`: emit `\x1b[I` / `\x1b[O` on focus gain/loss.
    pub focus_reporting: bool,
    /// DEC `?1007`: translate mouse wheel to Up/Down arrows on alternate screen.
    pub alt_scroll: bool,
    /// DEC `?2004`: bracketed paste mode (`\x1b[200~` .. `\x1b[201~`).
    pub bracketed_paste: bool,
    /// DEC `?2026`: synchronized output latched.
    pub sync_output: bool,
    /// DEC `DECLRMM` (`CSI ? 69 h/l`): enable left/right margin mode (`DECSLRM`).
    pub left_right_margin_mode: bool,
    /// DEC `?2027`: UAX #29 grapheme clustering mode.
    pub grapheme_clustering: bool,
    /// DEC `?2031`: color scheme / theme change notification mode.
    pub color_scheme_updates: bool,
    /// `DECKPAM` / `DECKPNM` (`ESC =` / `ESC >`): application keypad mode.
    pub app_keypad: bool,
    /// xterm `modifyOtherKeys` level (`0` = disabled, `1` = enabled, `2` = always; `CSI > 4 ; Pv m`).
    #[cfg_attr(feature = "serde", serde(default))]
    pub modify_other_keys: u8,
}

impl Default for Modes {
    fn default() -> Self {
        Self::with_default_blink(true)
    }
}

impl Modes {
    /// Creates default [`Modes`] with `cursor_blink` initialized from `default_blink`.
    #[must_use]
    pub const fn with_default_blink(default_blink: bool) -> Self {
        Self {
            insert_mode: false,
            linefeed_newline: false,
            app_cursor_keys: false,
            column_132: false,
            reverse_video: false,
            origin_mode: false,
            auto_wrap: true,
            cursor_blink: default_blink,
            cursor_visible: true,
            sixel_display_mode: false,
            sixel_private_color_registers: true,
            sixel_cursor_right: false,
            alt_screen: false,
            mouse_protocol: MouseProtocol::None,
            mouse_encoding: MouseEncoding::Default,
            focus_reporting: false,
            alt_scroll: false,
            bracketed_paste: false,
            sync_output: false,
            left_right_margin_mode: false,
            grapheme_clustering: true,
            color_scheme_updates: false,
            app_keypad: false,
            modify_other_keys: 0,
        }
    }

    /// Applies a soft terminal reset (`DECSTR`, `CSI ! p`) preserving screen contents.
    pub fn reset_soft(&mut self, default_blink: bool) {
        self.insert_mode = false;
        self.linefeed_newline = false;
        self.app_cursor_keys = false;
        self.reverse_video = false;
        self.origin_mode = false;
        self.auto_wrap = true;
        self.cursor_blink = default_blink;
        self.cursor_visible = true;
        self.sixel_display_mode = false;
        self.sixel_private_color_registers = true;
        self.sixel_cursor_right = false;
        self.sync_output = false;
        self.left_right_margin_mode = false;
        self.grapheme_clustering = true;
        self.color_scheme_updates = false;
        self.app_keypad = false;
        self.modify_other_keys = 0;
    }

    /// Returns the `DECRQM` (`CSI ? Ps $ p`) reply code (`1` = set, `2` = reset, `0` = unknown).
    #[must_use]
    pub const fn decrqm_dec(&self, mode: u16) -> u8 {
        let is_set = match mode {
            1 => self.app_cursor_keys,
            3 => self.column_132,
            5 => self.reverse_video,
            6 => self.origin_mode,
            7 => self.auto_wrap,
            9 => matches!(self.mouse_protocol, MouseProtocol::X10),
            12 => self.cursor_blink,
            25 => self.cursor_visible,
            80 => self.sixel_display_mode,
            47 | 1047 | 1049 => self.alt_screen,
            69 => self.left_right_margin_mode,
            1000 => matches!(self.mouse_protocol, MouseProtocol::Normal),
            1002 => matches!(self.mouse_protocol, MouseProtocol::ButtonEvent),
            1003 => matches!(self.mouse_protocol, MouseProtocol::AnyEvent),
            1004 => self.focus_reporting,
            1005 => matches!(self.mouse_encoding, MouseEncoding::Utf8),
            1006 => matches!(self.mouse_encoding, MouseEncoding::Sgr),
            1007 => self.alt_scroll,
            1016 => matches!(self.mouse_encoding, MouseEncoding::SgrPixels),
            1048 => return 2,
            1070 => self.sixel_private_color_registers,
            2004 => self.bracketed_paste,
            2026 => self.sync_output,
            2027 => self.grapheme_clustering,
            2031 => self.color_scheme_updates,
            8452 => self.sixel_cursor_right,
            _ => return 0,
        };
        if is_set { 1 } else { 2 }
    }

    /// Returns the `DECRQM` (`CSI Ps $ p`) reply code for ANSI modes (`4`, `20`).
    #[must_use]
    pub const fn decrqm_ansi(&self, mode: u16) -> u8 {
        let is_set = match mode {
            4 => self.insert_mode,
            20 => self.linefeed_newline,
            _ => return 0,
        };
        if is_set { 1 } else { 2 }
    }
}
