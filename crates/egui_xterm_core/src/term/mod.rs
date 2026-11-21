//! Core terminal state machine (`Terminal`, `TerminalState`), configuration, events, snapshots,
//! and VT handlers.

pub mod cursor;
pub mod dcs;
pub mod modes;
pub mod mouse;
pub mod osc;
pub mod selection;
pub mod sgr;
pub mod tabs;

#[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::iter::FusedIterator;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::cell::{Cell, CellExtra, CellFlags, HyperlinkPool, TextSizing, UnderlineStyle};
use crate::color::{ColorScheme, PackedColor, SpecialColors, Theme};
#[cfg(feature = "hyperlinks")]
use crate::features::hyperlinks::{
    HyperlinkInfo, HyperlinkSpan, ImplicitLinkStore, explicit_visible_spans,
};
#[cfg(feature = "kitty-graphics")]
use crate::features::inline_images::kitty::{
    KITTY_PLACEHOLDER_CHAR, KittyApcContext, KittyFileMediumPolicy, KittyShmReader,
    KittyTransportPolicy, diacritic_index, handle_kitty_apc_with_context,
};
#[cfg(feature = "sixel")]
use crate::features::inline_images::sixel::SixelDecoder;
#[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
use crate::features::inline_images::{ImagePlacement, ImageStore, StoredImage};
#[cfg(feature = "kitty-keyboard")]
use crate::features::kitty_keyboard::{
    KittyKeyEvent, KittyKeyboardFlags, KittyKeyboardState, ModifyOtherKeys,
    encode_modify_other_keys_into,
};
#[cfg(feature = "metrics")]
use crate::features::metrics::TerminalMetrics;
#[cfg(feature = "search")]
use crate::features::search::{
    SearchError, SearchMatch, SearchQuery, SearchState, SearchStepProgress,
};
#[cfg(feature = "shell-integration")]
use crate::features::shell_integration::{ShellIntegrationState, ShellZonesView};
#[cfg(feature = "serde")]
use crate::grid::GridSnapshot;
use crate::grid::reflow::{EvictionColPolicy, SelectionReflowInput};
use crate::grid::{
    AbsLine, CellExtraTable, EraseProtection, Grid, GridPoint, ReflowAnchors, Row, ScreenKind,
};
use crate::parser::{C1Policy, CsiParams, DEFAULT_MAX_OSC_BYTES, OscTerminator, Parser, VtHandler};
use crate::term::dcs::{DcsKind, DcsState, DecrqssState};
use crate::term::osc::{Osc99Pending, OscContext};
use crate::term::sgr::SgrStack;

pub use self::cursor::{Charset, CharsetSlots, Cursor, CursorAttrs, SavedCursor};
pub use self::modes::{CursorShape, CursorStyle, Modes, MouseEncoding, MouseProtocol};
pub use self::mouse::{
    MouseButton, MouseEventKind, MouseInputEvent, MouseModifiers, encode_mouse_event,
    encode_mouse_event_into, encode_mouse_event_pixels, encode_mouse_event_pixels_into,
    focus_event_bytes, format_paste_bytes, format_paste_bytes_into,
};
pub use self::selection::{Selection, SelectionKind, SelectionMode};
pub use self::tabs::TabStops;

const MAX_COMBINING_PER_CELL: usize = 32;

/// Policy controlling how `OSC 52 ; Pc ; ?` clipboard read queries are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Osc52ReadPolicy {
    /// Silently reject all `OSC 52` read queries (default).
    #[default]
    Deny,
    /// Allow `OSC 52` read queries and emit [`TerminalEvent::ClipboardReadRequested`].
    Allow,
    /// Prompt the host UI before fulfilling reads via [`TerminalEvent::ClipboardReadPrompt`].
    Prompt,
}

/// Configuration options for constructing a [`Terminal`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default, deny_unknown_fields))]
pub struct CoreConfig {
    /// Maximum scrollback lines retained on the primary screen (default: `10_000`).
    pub scrollback_limit: usize,
    /// Parser C1 control handling policy (default: [`C1Policy::Utf8Only`]).
    pub c1_policy: C1Policy,
    /// Visual theme and base color palette (default: [`Theme::dark()`]).
    pub theme: Theme,
    /// Default cursor shape before `DECSCUSR` overrides (default: [`CursorShape::Block`]).
    pub default_cursor_shape: CursorShape,
    /// Default cursor blink setting before `DECSCUSR`/`ATT610` overrides (default: `true`).
    pub default_cursor_blink: bool,
    /// Watchdog timeout in seconds for DEC Mode 2026 synchronized output (default: `0.250`).
    pub sync_output_timeout_secs: f64,
    /// Legacy boolean flag enabling `OSC 52` clipboard read requests when `osc52_read_policy`
    /// is [`Osc52ReadPolicy::Deny`] (default: `false`).
    pub allow_osc52_read: bool,
    /// Policy gating `OSC 52 ; Pc ; ?` clipboard read requests (default: [`Osc52ReadPolicy::Deny`]).
    #[cfg_attr(feature = "serde", serde(default))]
    pub osc52_read_policy: Osc52ReadPolicy,
    /// Policy gating Kitty graphics file-path (`t=f`) and temporary-file (`t=t`) loading
    /// (default: [`KittyFileMediumPolicy::LocalFiles`]).
    #[cfg(feature = "kitty-graphics")]
    #[cfg_attr(feature = "serde", serde(default))]
    pub kitty_file_medium_policy: KittyFileMediumPolicy,
    /// Delimiter characters used to bound tokens during word selection.
    pub word_separators: String,
    /// Maximum `OSC` payload byte budget passed to [`Parser`] (default: `4 MiB`).
    pub max_osc_bytes: usize,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            scrollback_limit: 10_000,
            c1_policy: C1Policy::Utf8Only,
            theme: Theme::dark(),
            default_cursor_shape: CursorShape::Block,
            default_cursor_blink: true,
            sync_output_timeout_secs: 0.250,
            allow_osc52_read: false,
            osc52_read_policy: Osc52ReadPolicy::Deny,
            #[cfg(feature = "kitty-graphics")]
            kitty_file_medium_policy: KittyFileMediumPolicy::LocalFiles,
            word_separators: " \t\r\n\"'`()[]{}<>|;:,=*+-/\\".to_owned(),
            max_osc_bytes: DEFAULT_MAX_OSC_BYTES,
        }
    }
}

/// Target clipboard buffer named in an `OSC 52` sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ClipboardSelection {
    /// System clipboard (`c`).
    Clipboard,
    /// X11 primary selection (`p`).
    Primary,
    /// X11 secondary selection (`s`).
    Secondary,
    /// Cut buffer / select (`q`, `0..=7`).
    Select,
}

/// Semantic events emitted by the terminal engine for the host UI or runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum TerminalEvent {
    /// Window title updated (`OSC 0` or `OSC 2`).
    TitleChanged(String),
    /// Icon name updated (`OSC 0` or `OSC 1`).
    IconNameChanged(String),
    /// Current working directory updated (`OSC 7`).
    CwdChanged(String),
    /// Audible or visual bell (`BEL`, `0x07`).
    Bell,
    /// Host clipboard write requested (`OSC 52`).
    ClipboardWrite(String),
    /// Host clipboard read requested (`OSC 52 ; Pc ; ?`) when `allow_osc52_read` is enabled.
    ClipboardReadRequested {
        /// Requested clipboard target buffer.
        selection: ClipboardSelection,
    },
    /// Palette colors updated (`OSC 4/10/11/12/104/110/111/112`).
    PaletteChanged,
    /// Cursor shape or blink updated (`DECSCUSR`).
    CursorStyleChanged(CursorStyle),
    /// Hyperlink activated by explicit click or host action (`hyperlinks` feature).
    HyperlinkActivated {
        /// Target URI of the activated hyperlink.
        uri: String,
        /// Optional `id=` parameter from `OSC 8`.
        id: Option<String>,
    },
    /// Shell command finished as reported by `OSC 133;D` (`shell-integration` feature).
    CommandFinished {
        /// Exit status code reported by the shell, if present and valid.
        exit_code: Option<i32>,
    },
    /// Task progress indicator updated (`OSC 9 ; 4 ; <state> ; <progress>`).
    Progress {
        /// Progress state (`0` = hidden, `1` = normal, `2` = error, `3` = indeterminate, `4` = paused).
        state: u8,
        /// Percentage in `0..=100`.
        progress: u8,
    },
    /// Desktop notification requested (`OSC 9`, `OSC 99`, or `OSC 777`).
    Notification {
        /// Notification title (may be empty for `OSC 9`).
        title: String,
        /// Notification body text.
        body: String,
    },
    /// Pointer cursor shape requested (`OSC 22 ; <shape>`).
    MouseShape(String),
    /// Current document URI updated (`OSC 6 ; file://...`).
    DocumentChanged(Option<String>),
    /// Host clipboard read requested under [`Osc52ReadPolicy::Prompt`] (`OSC 52 ; Pc ; ?`).
    ClipboardReadPrompt {
        /// Requested clipboard target buffer.
        selection: ClipboardSelection,
    },
    /// Desktop notification dismissed (`OSC 99 ; p=close`).
    NotificationClosed {
        /// Identifier (`i=<id>`) of the closed notification.
        id: String,
    },
}

/// Zero-allocation slice view over the visible viewport rows of a [`Grid`] or `LatchedFrame`.
#[derive(Debug, Clone, Copy)]
pub struct VisibleRowsView<'a> {
    first: &'a [Row],
    second: &'a [Row],
}

impl<'a> VisibleRowsView<'a> {
    /// Returns an empty [`VisibleRowsView`].
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            first: &[],
            second: &[],
        }
    }

    /// Constructs a [`VisibleRowsView`] from two contiguous backing slices.
    #[must_use]
    pub const fn from_slices(first: &'a [Row], second: &'a [Row]) -> Self {
        Self { first, second }
    }

    /// Returns the total number of visible rows.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.first.len() + self.second.len()
    }

    /// Returns `true` when there are no visible rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.first.is_empty() && self.second.is_empty()
    }

    /// Returns the visible [`Row`] at `idx`, or `None` if out of bounds.
    #[must_use]
    pub fn get(&self, idx: usize) -> Option<&'a Row> {
        if idx < self.first.len() {
            self.first.get(idx)
        } else {
            self.second.get(idx - self.first.len())
        }
    }

    /// Returns an iterator over all visible rows from top to bottom.
    #[must_use]
    pub fn iter(&self) -> VisibleRowsIter<'a> {
        VisibleRowsIter {
            first: self.first.iter(),
            second: self.second.iter(),
        }
    }

    /// Returns the two underlying contiguous slices `(first, second)`.
    #[must_use]
    pub const fn slices(&self) -> (&'a [Row], &'a [Row]) {
        (self.first, self.second)
    }
}

impl std::ops::Index<usize> for VisibleRowsView<'_> {
    type Output = Row;

    fn index(&self, idx: usize) -> &Self::Output {
        let len = self.len();
        match self.get(idx) {
            Some(row) => row,
            None => panic!("visible_rows index {idx} out of bounds for len {len}"),
        }
    }
}

impl<'a> IntoIterator for VisibleRowsView<'a> {
    type Item = &'a Row;
    type IntoIter = VisibleRowsIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &VisibleRowsView<'a> {
    type Item = &'a Row;
    type IntoIter = VisibleRowsIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over [`VisibleRowsView`] yielding `&Row` from top to bottom.
#[derive(Debug, Clone)]
pub struct VisibleRowsIter<'a> {
    first: std::slice::Iter<'a, Row>,
    second: std::slice::Iter<'a, Row>,
}

impl<'a> Iterator for VisibleRowsIter<'a> {
    type Item = &'a Row;

    fn next(&mut self) -> Option<Self::Item> {
        self.first.next().or_else(|| self.second.next())
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let rem = self.first.len() + self.second.len();
        (rem, Some(rem))
    }
}

impl DoubleEndedIterator for VisibleRowsIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.second.next_back().or_else(|| self.first.next_back())
    }
}

impl ExactSizeIterator for VisibleRowsIter<'_> {}
impl FusedIterator for VisibleRowsIter<'_> {}

/// Storage-agnostic inspection snapshot of a single visible cell.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CellSnapshot {
    /// Primary Unicode scalar value (`' '` for blank cells).
    pub ch: char,
    /// Full grapheme cluster string (combining marks included; empty for spacer cells).
    pub grapheme: String,
    /// Column width occupied by this cell (`1` or `2`, `0` for `WIDE_SPACER`/`WRAP_SPACER`).
    pub width: u8,
    /// Whether this cell is the leading cell of a double-width character.
    pub is_wide: bool,
    /// Whether this cell is the trailing spacer of a double-width character.
    pub is_wide_spacer: bool,
    /// Foreground color.
    pub fg: PackedColor,
    /// Background color.
    pub bg: PackedColor,
    /// Optional `SGR 58` underline color override.
    pub underline_color: Option<PackedColor>,
    /// Active underline style.
    pub underline_style: UnderlineStyle,
    /// Raw cell attribute and layout flags.
    pub flags: CellFlags,
}

/// Cursor geometry and visibility inside a [`RenderSnapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CursorSnapshot {
    /// Zero-based visible viewport row.
    pub row: u16,
    /// Zero-based column.
    pub col: u16,
    /// Whether the cursor should be rendered (`DECTCEM` and `display_offset == 0`).
    pub visible: bool,
    /// Cursor shape and blink state.
    pub style: CursorStyle,
}

#[derive(Debug, Clone)]
enum SnapshotExtras<'a> {
    Live(&'a CellExtraTable),
    Latched(&'a [(u32, CellExtra)]),
}

/// Tracks which base palette slots have been explicitly overridden via `OSC 4/10/11/12`
/// sequences rather than inherited from [`Theme::scheme`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PaletteOverrides {
    /// Whether default foreground was overridden via `OSC 10`.
    pub foreground: bool,
    /// Whether default background was overridden via `OSC 11`.
    pub background: bool,
    /// Whether cursor fill was overridden via `OSC 12`.
    pub cursor: bool,
    /// Whether each ANSI color slot (`0..16`) was overridden via `OSC 4`.
    pub ansi: [bool; 16],
}

/// Read-only view of the visible terminal frame, frozen during DEC Mode 2026 synchronized output.
#[derive(Debug, Clone)]
pub struct RenderSnapshot<'a> {
    /// Visible column count.
    pub cols: u16,
    /// Visible row count.
    pub rows: u16,
    /// Zero-allocation slice view over visible viewport rows from top to bottom.
    pub visible_rows: VisibleRowsView<'a>,
    /// Effective cursor state for this frame.
    pub cursor: CursorSnapshot,
    /// Terminal modes active for this frame.
    pub modes: &'a Modes,
    /// Effective color palette for this frame.
    pub palette: &'a ColorScheme,
    /// Explicit `OSC 4/10/11/12` override flags for base palette slots.
    pub palette_overrides: PaletteOverrides,
    /// Extended 256-color `OSC 4` overrides for indices `16..=255`.
    pub extended_palette: &'a [Option<[u8; 3]>; 240],
    /// Special SGR attribute color overrides (`OSC 5` / `OSC 105`: bold, underline, blink,
    /// reverse, italic).
    pub special_colors: [Option<[u8; 3]>; 5],
    /// Monotonic palette/theme epoch incremented on any palette, theme, or special color change.
    pub palette_epoch: u64,
    /// Active selection, if any.
    pub selection: Option<&'a Selection>,
    /// Scrollback display offset (`0` = live bottom).
    pub display_offset: usize,
    /// Number of retained scrollback lines.
    pub scrollback_len: usize,
    /// Whether the alternate screen is active.
    pub is_alt_screen: bool,
    /// Whether DEC Mode 2026 synchronized output is currently latched.
    pub is_synchronized_output: bool,
    /// Monotonic frame/damage epoch for renderer cache invalidation.
    pub damage_epoch: u64,
    /// [`AbsLine`] corresponding to `visible_rows[0]`.
    pub top_abs_line: AbsLine,
    extras: SnapshotExtras<'a>,
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    visible_images: Cow<'a, [ImagePlacement]>,
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    image_store: &'a ImageStore,
}

impl RenderSnapshot<'_> {
    /// Looks up [`CellExtra`] for `extra_id` (`0` returns `None`).
    #[must_use]
    pub fn cell_extra(&self, extra_id: u32) -> Option<&CellExtra> {
        if extra_id == 0 {
            return None;
        }
        match self.extras {
            SnapshotExtras::Live(table) => table.get(extra_id),
            SnapshotExtras::Latched(slice) => slice
                .binary_search_by_key(&extra_id, |(id, _)| *id)
                .ok()
                .map(|idx| &slice[idx].1),
        }
    }

    /// Extracts trimmed plain text for visible `row` (`0..self.rows`).
    #[must_use]
    pub fn line_text(&self, row: u16) -> Option<String> {
        let r = self.visible_rows.get(usize::from(row))?;
        let mut out = String::new();
        let end = r.trimmed_len();
        for cell in &r.cells()[..end] {
            if cell.is_wide_spacer() || cell.is_wrap_spacer() || cell.is_sized_spacer() {
                continue;
            }
            if cell.extra_id != 0
                && let Some(ext) = self.cell_extra(cell.extra_id)
            {
                ext.write_grapheme(cell.codepoint, &mut out);
                continue;
            }
            out.push(cell.codepoint);
        }
        Some(out)
    }

    /// Returns `true` if cell `(viewport_row, col)` falls inside the active selection.
    #[must_use]
    pub fn is_cell_selected(&self, viewport_row: usize, col: usize) -> bool {
        let Some(sel) = self.selection else {
            return false;
        };
        let Some(row) = self.visible_rows.get(viewport_row) else {
            return false;
        };
        let line = AbsLine(self.top_abs_line.0 + viewport_row as u64);
        sel.row_col_range(line, row)
            .is_some_and(|(c0, c1)| col >= c0 && col <= c1)
    }

    /// Resolves xterm 256-color `index` against this snapshot's effective palette, honoring
    /// `OSC 4` overrides for both `0..16` and `16..=255`.
    #[must_use]
    pub const fn resolve_indexed_color(&self, index: u8) -> [u8; 3] {
        if index < 16 {
            self.palette.ansi[index as usize]
        } else if let Some(rgb) = self.extended_palette[(index - 16) as usize] {
            rgb
        } else {
            ColorScheme::xterm_256_color(index, &self.palette.ansi)
        }
    }

    /// Returns inline image placements intersecting the visible viewport.
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    #[must_use]
    pub fn image_placements(&self) -> &[ImagePlacement] {
        &self.visible_images
    }

    /// Looks up decoded pixel data for inline image `id`.
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    #[must_use]
    pub fn image_data(&self, id: u32) -> Option<&StoredImage> {
        self.image_store.get_image(id)
    }

    /// Looks up a virtual (`U=1`) inline image placement matching `(image_id, placement_id)`
    /// on the active screen for Unicode placeholder resolution.
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    #[must_use]
    pub fn virtual_placement(&self, image_id: u32, placement_id: u32) -> Option<ImagePlacement> {
        let screen = if self.is_alt_screen {
            ScreenKind::Alternate
        } else {
            ScreenKind::Primary
        };
        self.image_store
            .virtual_placement_on(screen, image_id, placement_id)
    }
}

#[cfg(feature = "kitty-graphics")]
#[derive(Default)]
struct ShmReaderSlot(Option<Box<dyn KittyShmReader + Send + Sync>>);

#[cfg(feature = "kitty-graphics")]
impl std::fmt::Debug for ShmReaderSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ShmReaderSlot")
            .field(&self.0.as_ref().map(|_| ".."))
            .finish()
    }
}

#[derive(Debug, Clone)]
struct LatchedFrame {
    active: bool,
    cols: u16,
    rows: u16,
    visible_rows: Vec<Row>,
    extras: Vec<(u32, CellExtra)>,
    cursor: CursorSnapshot,
    modes: Modes,
    palette: ColorScheme,
    palette_overrides: PaletteOverrides,
    extended_palette: [Option<[u8; 3]>; 240],
    special_colors: [Option<[u8; 3]>; 5],
    palette_epoch: u64,
    selection: Option<Selection>,
    display_offset: usize,
    scrollback_len: usize,
    is_alt_screen: bool,
    damage_epoch: u64,
    top_abs_line: AbsLine,
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    visible_images: Vec<ImagePlacement>,
}

impl Default for LatchedFrame {
    fn default() -> Self {
        Self {
            active: false,
            cols: 0,
            rows: 0,
            visible_rows: Vec::new(),
            extras: Vec::new(),
            cursor: CursorSnapshot::default(),
            modes: Modes::default(),
            palette: ColorScheme::default(),
            palette_overrides: PaletteOverrides::default(),
            extended_palette: [None; 240],
            special_colors: [None; 5],
            palette_epoch: 0,
            selection: None,
            display_offset: 0,
            scrollback_len: 0,
            is_alt_screen: false,
            damage_epoch: 0,
            top_abs_line: AbsLine(0),
            #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
            visible_images: Vec::new(),
        }
    }
}

impl LatchedFrame {
    fn reset(&mut self) {
        self.active = false;
        self.extras.clear();
        #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
        self.visible_images.clear();
    }
}

#[derive(Debug, Clone, Copy)]
struct LastWrittenCell {
    row: usize,
    col: usize,
    ended_with_zwj: bool,
    is_unpaired_ri: bool,
}

#[derive(Debug, Clone)]
struct LastGraphic {
    base: char,
    combining: Vec<char>,
}

/// Serializable full-state snapshot of a [`Terminal`] for persistence and session restore
/// (`serde` feature).
#[cfg(feature = "serde")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminalSnapshot {
    cols: u16,
    rows: u16,
    primary: GridSnapshot,
    alternate: GridSnapshot,
    modes: Modes,
    cursor: Cursor,
    alt_cursor_backup: Option<Cursor>,
    primary_saved_cursor: Option<SavedCursor>,
    alt_saved_cursor: Option<SavedCursor>,
    tabs: TabStops,
    scroll_top: usize,
    scroll_bottom: usize,
    #[serde(default)]
    scroll_left: usize,
    #[serde(default)]
    scroll_right: usize,
    palette: ColorScheme,
    #[serde(default)]
    palette_overrides: PaletteOverrides,
    extended_palette: Vec<(u8, [u8; 3])>,
    #[serde(default)]
    special_colors: SpecialColors,
    title: String,
    icon_name: String,
    cwd: Option<String>,
    #[serde(default)]
    document_uri: Option<String>,
    hyperlink_pool: HyperlinkPool,
    selection: Option<Selection>,
    #[cfg(feature = "shell-integration")]
    #[serde(default)]
    shell_integration: Option<ShellIntegrationState>,
    #[cfg(feature = "kitty-keyboard")]
    #[serde(default)]
    kitty_primary_stack: Vec<KittyKeyboardFlags>,
    #[cfg(feature = "kitty-keyboard")]
    #[serde(default)]
    kitty_alt_stack: Vec<KittyKeyboardFlags>,
}

#[cfg(all(test, feature = "serde", feature = "shell-integration"))]
impl TerminalSnapshot {
    pub(crate) fn shell_integration_mut(&mut self) -> Option<&mut ShellIntegrationState> {
        self.shell_integration.as_mut()
    }
}

/// Complete, pure-Rust terminal state machine separated from [`Parser`] so [`Parser::advance`]
/// can drive [`TerminalState`] without `std::mem::take`.
#[derive(Debug)]
pub struct TerminalState {
    config: CoreConfig,
    primary: Grid,
    alternate: Grid,
    modes: Modes,
    cursor: Cursor,
    alt_cursor_backup: Option<Cursor>,
    primary_saved_cursor: Option<SavedCursor>,
    alt_saved_cursor: Option<SavedCursor>,
    tabs: TabStops,
    scroll_top: usize,
    scroll_bottom: usize,
    scroll_left: usize,
    scroll_right: usize,
    pixel_width: u16,
    pixel_height: u16,
    cell_pixel_width: u16,
    cell_pixel_height: u16,
    window_pos: (i32, i32),
    screen_char_size: Option<(u16, u16)>,
    window_iconified: bool,
    palette: ColorScheme,
    palette_overrides: PaletteOverrides,
    extended_palette: [Option<[u8; 3]>; 240],
    special_colors: SpecialColors,
    palette_epoch: u64,
    title: String,
    icon_name: String,
    title_stack: Vec<(Option<String>, Option<String>)>,
    cwd: Option<String>,
    document_uri: Option<String>,
    hyperlink_pool: HyperlinkPool,
    selection: Option<Selection>,
    outgoing: Vec<u8>,
    events: Vec<TerminalEvent>,
    dcs_state: DcsState,
    mouse_shape_stack: Vec<String>,
    osc99_pending: VecDeque<Osc99Pending>,
    sgr_stack: SgrStack,
    sync_started_at: Option<f64>,
    last_time_secs: Option<f64>,
    latched_frame: LatchedFrame,
    last_written: Option<LastWrittenCell>,
    last_graphic: Option<LastGraphic>,
    grapheme_scratch: String,
    #[cfg(feature = "search")]
    search_state: SearchState,
    #[cfg(feature = "search")]
    incremental_search_active: bool,
    #[cfg(feature = "search")]
    search_dirty_abs_pri: Option<AbsLine>,
    #[cfg(feature = "search")]
    search_dirty_abs_alt: Option<AbsLine>,
    #[cfg(feature = "hyperlinks")]
    primary_implicit_links: ImplicitLinkStore,
    #[cfg(feature = "hyperlinks")]
    alt_implicit_links: ImplicitLinkStore,
    #[cfg(feature = "hyperlinks")]
    osc8_inserts_since_gc: usize,
    #[cfg(feature = "hyperlinks")]
    hyperlink_gc_threshold: usize,
    #[cfg(feature = "hyperlinks")]
    hyperlink_gc_bits: Vec<u64>,
    #[cfg(feature = "shell-integration")]
    shell_integration: ShellIntegrationState,
    #[cfg(feature = "kitty-keyboard")]
    kitty_keyboard: KittyKeyboardState,
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    image_store: ImageStore,
    #[cfg(feature = "kitty-graphics")]
    kitty_shm_reader: ShmReaderSlot,
    #[cfg(feature = "sixel")]
    active_sixel: Option<SixelDecoder>,
    #[cfg(feature = "sixel")]
    sixel_shared_palette: Vec<[u8; 4]>,
    #[cfg(feature = "metrics")]
    metrics: TerminalMetrics,
}

/// Complete, pure-Rust terminal emulator combining a VT500 [`Parser`] and [`TerminalState`].
#[derive(Debug)]
pub struct Terminal {
    parser: Parser,
    state: TerminalState,
}

impl Terminal {
    /// Creates a new [`Terminal`] sized to `(cols.max(1), rows.max(1))` with `config`.
    #[must_use]
    pub fn new(cols: u16, rows: u16, config: CoreConfig) -> Self {
        let parser =
            Parser::with_c1_policy(config.c1_policy).with_max_osc_bytes(config.max_osc_bytes);
        let state = TerminalState::new(cols, rows, config);
        Self { parser, state }
    }

    /// Returns a read-only reference to the underlying [`TerminalState`].
    #[must_use]
    pub const fn state(&self) -> &TerminalState {
        &self.state
    }

    /// Returns a mutable reference to the underlying [`TerminalState`].
    pub const fn state_mut(&mut self) -> &mut TerminalState {
        &mut self.state
    }

    /// Returns a read-only reference to the VT500 [`Parser`].
    #[must_use]
    pub const fn parser(&self) -> &Parser {
        &self.parser
    }

    /// Returns a mutable reference to the VT500 [`Parser`].
    pub const fn parser_mut(&mut self) -> &mut Parser {
        &mut self.parser
    }

    /// Streams raw VT/ANSI `bytes` into the terminal without updating the DEC 2026 clock.
    pub fn advance(&mut self, bytes: &[u8]) {
        #[cfg(feature = "metrics")]
        self.state.metrics.record_bytes(bytes.len());
        self.parser.advance(bytes, &mut self.state);
        self.state.post_advance();
    }

    /// Streams raw VT/ANSI `bytes` into the terminal using `now_secs` for the DEC Mode 2026
    /// synchronized-output watchdog.
    pub fn advance_with_time(&mut self, bytes: &[u8], now_secs: f64) {
        if now_secs.is_finite() {
            self.state.last_time_secs = Some(now_secs);
        }
        self.state.check_sync_timeout(now_secs);
        if !bytes.is_empty() {
            self.advance(bytes);
        }
        self.state.check_sync_timeout(now_secs);
    }

    /// Restores a [`Terminal`] from a [`TerminalSnapshot`] and `config`, defensively sanitizing
    /// dimensions, cursor bounds, scroll margins, hyperlink pools, and sparse `CellExtra` ownership.
    #[cfg(feature = "serde")]
    #[must_use]
    pub fn from_snapshot(snapshot: TerminalSnapshot, config: CoreConfig) -> Self {
        let parser =
            Parser::with_c1_policy(config.c1_policy).with_max_osc_bytes(config.max_osc_bytes);
        let state = TerminalState::from_snapshot(snapshot, config);
        Self { parser, state }
    }
}

impl TerminalState {
    /// Creates a new [`TerminalState`] sized to `(cols.max(1), rows.max(1))` with `config`.
    #[must_use]
    pub fn new(cols: u16, rows: u16, config: CoreConfig) -> Self {
        let cols_u = usize::from(cols.max(1));
        let rows_u = usize::from(rows.max(1));
        let primary = Grid::new(ScreenKind::Primary, cols_u, rows_u, config.scrollback_limit);
        let alternate = Grid::new(ScreenKind::Alternate, cols_u, rows_u, 0);
        let modes = Modes::with_default_blink(config.default_cursor_blink);
        let cursor = Cursor {
            style: CursorStyle {
                shape: config.default_cursor_shape,
                blink: config.default_cursor_blink,
            },
            ..Cursor::default()
        };
        let palette = config.theme.scheme.clone();
        #[cfg(feature = "shell-integration")]
        let shell_integration = ShellIntegrationState::new(config.scrollback_limit, rows_u);
        Self {
            config,
            primary,
            alternate,
            modes,
            cursor,
            alt_cursor_backup: None,
            primary_saved_cursor: None,
            alt_saved_cursor: None,
            tabs: TabStops::new(cols_u),
            scroll_top: 0,
            scroll_bottom: rows_u - 1,
            scroll_left: 0,
            scroll_right: cols_u - 1,
            pixel_width: 0,
            pixel_height: 0,
            cell_pixel_width: 0,
            cell_pixel_height: 0,
            window_pos: (0, 0),
            screen_char_size: None,
            window_iconified: false,
            palette,
            palette_overrides: PaletteOverrides::default(),
            extended_palette: [None; 240],
            special_colors: SpecialColors::default(),
            palette_epoch: 0,
            title: String::new(),
            icon_name: String::new(),
            title_stack: Vec::new(),
            cwd: None,
            document_uri: None,
            hyperlink_pool: HyperlinkPool::new(),
            selection: None,
            outgoing: Vec::new(),
            events: Vec::new(),
            dcs_state: DcsState::default(),
            mouse_shape_stack: Vec::new(),
            osc99_pending: VecDeque::new(),
            sgr_stack: SgrStack::default(),
            sync_started_at: None,
            last_time_secs: None,
            latched_frame: LatchedFrame::default(),
            last_written: None,
            last_graphic: None,
            grapheme_scratch: String::with_capacity(16),
            #[cfg(feature = "search")]
            search_state: SearchState::default(),
            #[cfg(feature = "search")]
            incremental_search_active: false,
            #[cfg(feature = "search")]
            search_dirty_abs_pri: None,
            #[cfg(feature = "search")]
            search_dirty_abs_alt: None,
            #[cfg(feature = "hyperlinks")]
            primary_implicit_links: ImplicitLinkStore::default(),
            #[cfg(feature = "hyperlinks")]
            alt_implicit_links: ImplicitLinkStore::default(),
            #[cfg(feature = "hyperlinks")]
            osc8_inserts_since_gc: 0,
            #[cfg(feature = "hyperlinks")]
            hyperlink_gc_threshold: 256,
            #[cfg(feature = "hyperlinks")]
            hyperlink_gc_bits: Vec::new(),
            #[cfg(feature = "shell-integration")]
            shell_integration,
            #[cfg(feature = "kitty-keyboard")]
            kitty_keyboard: KittyKeyboardState::default(),
            #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
            image_store: ImageStore::default(),
            #[cfg(feature = "kitty-graphics")]
            kitty_shm_reader: ShmReaderSlot::default(),
            #[cfg(feature = "sixel")]
            active_sixel: None,
            #[cfg(feature = "sixel")]
            sixel_shared_palette: Vec::new(),
            #[cfg(feature = "metrics")]
            metrics: TerminalMetrics::new(),
        }
    }

    /// Runs post-parse synchronization (`ImplicitLinkStore`, `ShellIntegrationState`, image pruning).
    pub fn post_advance(&mut self) {
        #[cfg(feature = "hyperlinks")]
        self.refresh_implicit_hyperlinks();
        #[cfg(feature = "shell-integration")]
        self.shell_integration.sync_with_grid(&self.primary);
        #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
        self.image_store
            .prune_before(ScreenKind::Primary, self.primary.oldest_abs_line());
    }

    /// Checks whether an active DEC Mode 2026 latch has exceeded `sync_output_timeout_secs` at
    /// `now_secs`, releasing the frozen snapshot if expired. Returns `true` if flushed.
    pub fn check_sync_timeout(&mut self, now_secs: f64) -> bool {
        if !self.modes.sync_output || !now_secs.is_finite() {
            return false;
        }
        let Some(started) = self.sync_started_at else {
            self.sync_started_at = Some(now_secs);
            return false;
        };
        if !started.is_finite() {
            self.sync_started_at = Some(now_secs);
            return false;
        }
        if (now_secs - started) >= self.config.sync_output_timeout_secs {
            self.exit_sync_mode();
            return true;
        }
        false
    }

    /// Returns the armed DEC Mode 2026 watchdog deadline on the same clock as
    /// [`Terminal::advance_with_time`], if it has a finite deadline.
    #[must_use]
    pub fn sync_output_deadline(&self) -> Option<f64> {
        self.sync_started_at
            .map(|started| started + self.config.sync_output_timeout_secs)
            .filter(|deadline| deadline.is_finite())
    }

    /// Resizes the terminal to `(cols.max(1), rows.max(1))`, reflowing primary scrollback and
    /// remapping cursor and selection anchors.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let new_cols = usize::from(cols.max(1));
        let new_rows = usize::from(rows.max(1));
        if new_cols == self.primary.cols() && new_rows == self.primary.viewport_rows() {
            return;
        }

        #[cfg(feature = "metrics")]
        self.metrics.record_reflow();

        if self.modes.sync_output {
            self.exit_sync_mode();
        }

        #[cfg(feature = "shell-integration")]
        let (shell_extra_points, shell_tracking_cursor, shell_pending_wrap_idx) = {
            self.shell_integration
                .sync_after_eviction_if_needed(&self.primary);
            let pri_cur = if self.modes.alt_screen {
                self.alt_cursor_backup
                    .as_ref()
                    .map(|c| (c.row, c.col, c.pending_wrap))
            } else {
                Some((self.cursor.row, self.cursor.col, self.cursor.pending_wrap))
            };
            let tracking = self
                .shell_integration
                .prepare_for_reflow(&self.primary, pri_cur);
            let mut pts = Vec::new();
            let pw_idx = self.shell_integration.collect_reflow_points(&mut pts);
            (pts, tracking, if tracking { None } else { pw_idx })
        };
        #[cfg(not(feature = "shell-integration"))]
        let (shell_extra_points, shell_pending_wrap_idx): (Vec<GridPoint>, Option<usize>) =
            (Vec::new(), None);
        #[cfg(feature = "shell-integration")]
        let shell_point_count = shell_extra_points.len();
        #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
        let (pri_extra_points, pri_img_start, pri_img_count, alt_extra_points, alt_img_count) = {
            let mut pri_pts = shell_extra_points;
            let img_start = pri_pts.len();
            let pri_cnt = self
                .image_store
                .collect_reflow_points(ScreenKind::Primary, &mut pri_pts);
            let mut alt_pts = Vec::new();
            let alt_cnt = self
                .image_store
                .collect_reflow_points(ScreenKind::Alternate, &mut alt_pts);
            (pri_pts, img_start, pri_cnt, alt_pts, alt_cnt)
        };
        #[cfg(not(any(feature = "kitty-graphics", feature = "sixel")))]
        let (pri_extra_points, alt_extra_points) = (shell_extra_points, Vec::<GridPoint>::new());

        let sel_input = self.selection.map(|s| SelectionReflowInput {
            anchor: s.anchor,
            anchor_past_eol: s.anchor_past_eol,
            head: s.head,
            head_past_eol: s.head_past_eol,
            origin_start: s.origin_start,
            origin_start_past_eol: s.origin_start_past_eol,
            origin_end: s.origin_end,
            origin_end_past_eol: s.origin_end_past_eol,
            origin_evicted: s.origin_evicted,
            origin_cropped_bottom: s.origin_cropped_bottom,
            col_policy: if s.mode == SelectionMode::Block {
                EvictionColPolicy::PreserveColumn
            } else {
                EvictionColPolicy::ResetToZero
            },
        });

        let (sel_res, pri_extra_mapped, pri_extra_pw, alt_extra_mapped) = if self.modes.alt_screen {
            let alt_anchors = ReflowAnchors {
                cursor_row: self.cursor.row,
                cursor_col: self.cursor.col,
                pending_wrap: self.cursor.pending_wrap,
                selection: None,
                extra_points: alt_extra_points,
            };
            let (alt_res, alt_sel_res, _) = self.alternate.resize_with_selection(
                new_cols,
                new_rows,
                alt_anchors,
                sel_input,
                None,
            );
            self.cursor.row = alt_res.cursor_row;
            self.cursor.col = alt_res.cursor_col;
            self.cursor.pending_wrap = alt_res.pending_wrap;

            let (pri_res, _, pri_pw) = if let Some(ref mut pri_cur) = self.alt_cursor_backup {
                let pri_anchors = ReflowAnchors {
                    cursor_row: pri_cur.row,
                    cursor_col: pri_cur.col,
                    pending_wrap: pri_cur.pending_wrap,
                    selection: None,
                    extra_points: pri_extra_points,
                };
                let (pri_res, pri_sel, pri_pw) = self.primary.resize_with_selection(
                    new_cols,
                    new_rows,
                    pri_anchors,
                    None,
                    shell_pending_wrap_idx,
                );
                pri_cur.row = pri_res.cursor_row;
                pri_cur.col = pri_res.cursor_col;
                pri_cur.pending_wrap = pri_res.pending_wrap;
                (pri_res, pri_sel, pri_pw)
            } else {
                let pri_anchors = ReflowAnchors {
                    extra_points: pri_extra_points,
                    ..ReflowAnchors::default()
                };
                self.primary.resize_with_selection(
                    new_cols,
                    new_rows,
                    pri_anchors,
                    None,
                    shell_pending_wrap_idx,
                )
            };
            (
                alt_sel_res,
                pri_res.extra_points,
                pri_pw,
                alt_res.extra_points,
            )
        } else {
            let anchors = ReflowAnchors {
                cursor_row: self.cursor.row,
                cursor_col: self.cursor.col,
                pending_wrap: self.cursor.pending_wrap,
                selection: None,
                extra_points: pri_extra_points,
            };
            let (res, pri_sel_res, pri_pw) = self.primary.resize_with_selection(
                new_cols,
                new_rows,
                anchors,
                sel_input,
                shell_pending_wrap_idx,
            );
            self.cursor.row = res.cursor_row;
            self.cursor.col = res.cursor_col;
            self.cursor.pending_wrap = res.pending_wrap;
            let alt_anchors = ReflowAnchors {
                extra_points: alt_extra_points,
                ..ReflowAnchors::default()
            };
            let (alt_res, _, _) =
                self.alternate
                    .resize_with_selection(new_cols, new_rows, alt_anchors, None, None);
            (pri_sel_res, res.extra_points, pri_pw, alt_res.extra_points)
        };

        #[cfg(feature = "shell-integration")]
        {
            let post_pri_cur = if self.modes.alt_screen {
                self.alt_cursor_backup
                    .as_ref()
                    .map(|c| (c.row, c.col, c.pending_wrap))
            } else {
                Some((self.cursor.row, self.cursor.col, self.cursor.pending_wrap))
            };
            self.shell_integration.apply_reflow_points(
                &self.primary,
                &pri_extra_mapped[..shell_point_count],
                pri_extra_pw,
                post_pri_cur,
                shell_tracking_cursor,
                self.config.scrollback_limit,
            );
        }
        #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
        {
            self.image_store.apply_reflow_points(
                ScreenKind::Primary,
                &pri_extra_mapped[pri_img_start..pri_img_start + pri_img_count],
                true,
            );
            self.image_store.apply_reflow_points(
                ScreenKind::Alternate,
                &alt_extra_mapped[..alt_img_count],
                true,
            );
        }
        #[cfg(not(feature = "shell-integration"))]
        let _unused_pri_extra_pw = pri_extra_pw;
        #[cfg(all(
            not(feature = "shell-integration"),
            not(any(feature = "kitty-graphics", feature = "sixel"))
        ))]
        let _unused_pri_extra_mapped = pri_extra_mapped;
        #[cfg(not(any(feature = "kitty-graphics", feature = "sixel")))]
        let _unused_alt_extra_mapped = alt_extra_mapped;

        if let Some(sr) = sel_res {
            if let Some(sel) = &mut self.selection {
                sel.anchor = sr.anchor;
                sel.anchor_past_eol = sr.anchor_past_eol;
                sel.head = sr.head;
                sel.head_past_eol = sr.head_past_eol;
                sel.origin_start = sr.origin_start;
                sel.origin_start_past_eol = sr.origin_start_past_eol;
                sel.origin_end = sr.origin_end;
                sel.origin_end_past_eol = sr.origin_end_past_eol;
                sel.origin_evicted = sr.origin_evicted;
                sel.origin_cropped_bottom = sr.origin_cropped_bottom;
            }
        } else {
            self.selection = None;
        }

        self.tabs.resize(new_cols);
        let clamp_saved = |saved: &mut SavedCursor| {
            saved.row = saved.row.min(new_rows - 1);
            saved.col = saved.col.min(new_cols - 1);
            saved.pending_wrap &= saved.col == new_cols - 1;
        };
        if let Some(saved) = &mut self.primary_saved_cursor {
            clamp_saved(saved);
        }
        if let Some(saved) = &mut self.alt_saved_cursor {
            clamp_saved(saved);
        }
        self.scroll_top = 0;
        self.scroll_bottom = new_rows - 1;
        self.scroll_left = 0;
        self.scroll_right = new_cols - 1;
        self.last_written = None;
        self.last_graphic = None;
        #[cfg(feature = "hyperlinks")]
        self.refresh_implicit_hyperlinks();
        #[cfg(feature = "shell-integration")]
        self.shell_integration.sync_with_grid(&self.primary);
        #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
        self.image_store
            .prune_before(ScreenKind::Primary, self.primary.oldest_abs_line());
    }

    /// Updates the reported physical pixel dimensions of the terminal viewport for `XTWINOPS`
    /// (`CSI 14 t` / `CSI 16 t`).
    pub fn set_pixel_size(&mut self, width: u16, height: u16) {
        self.pixel_width = width;
        self.pixel_height = height;
    }

    /// Returns the last reported physical pixel dimensions `(width, height)`, or `(0, 0)`.
    #[must_use]
    pub const fn pixel_size(&self) -> (u16, u16) {
        (self.pixel_width, self.pixel_height)
    }

    /// Configures the measured cell size in physical pixels (`width_px`, `height_px`) used by
    /// `XTWINOPS 16 t`, Sixel, Kitty graphics, and `DEC ?1016` SGR-pixels fallback.
    pub fn set_cell_pixel_size(&mut self, width_px: u16, height_px: u16) {
        self.cell_pixel_width = width_px;
        self.cell_pixel_height = height_px;
    }

    /// Returns the effective cell size in physical pixels `(cell_w, cell_h)`, falling back to
    /// `pixel_size / grid_size` when configured, or `(8, 16)` otherwise.
    #[must_use]
    pub fn cell_pixel_size(&self) -> (u16, u16) {
        self.effective_cell_pixel_size()
    }

    fn effective_cell_pixel_size(&self) -> (u16, u16) {
        if self.cell_pixel_width > 0 && self.cell_pixel_height > 0 {
            return (self.cell_pixel_width, self.cell_pixel_height);
        }
        if self.pixel_width > 0 && self.pixel_height > 0 {
            let (cols, rows) = self.size();
            let cw = (self.pixel_width / cols.max(1)).max(1);
            let ch = (self.pixel_height / rows.max(1)).max(1);
            return (cw, ch);
        }
        (8, 16)
    }

    /// Updates the host window position `(x, y)` reported by `XTWINOPS 13 t`.
    pub fn set_window_pos(&mut self, x: i32, y: i32) {
        self.window_pos = (x, y);
    }

    /// Updates the host screen character geometry `(cols, rows)` reported by `XTWINOPS 19 t`.
    pub fn set_screen_char_size(&mut self, size: Option<(u16, u16)>) {
        self.screen_char_size = size;
    }

    /// Updates whether the host window is iconified/minimized for `XTWINOPS 11 t`.
    pub fn set_window_iconified(&mut self, iconified: bool) {
        self.window_iconified = iconified;
    }

    /// Returns the terminal dimensions as `(cols, rows)`.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        let g = self.grid();
        #[expect(clippy::cast_possible_truncation, reason = "grid initialized from u16")]
        (g.cols() as u16, g.viewport_rows() as u16)
    }

    /// Scrolls the viewport display offset (`delta_lines > 0` scrolls up into history).
    pub fn scroll_display(&mut self, delta_lines: i32) {
        if !self.modes.alt_screen {
            self.primary.scroll_display(delta_lines);
        }
    }

    /// Scrolls the viewport back to the live bottom (`display_offset = 0`).
    pub fn scroll_to_bottom(&mut self) {
        if !self.modes.alt_screen {
            self.primary.scroll_to_bottom();
        }
    }

    /// Returns the current scrollback offset (`0` = live bottom, always `0` on alternate screen).
    #[must_use]
    pub fn display_offset(&self) -> usize {
        if self.modes.alt_screen {
            0
        } else {
            self.primary.display_offset()
        }
    }

    /// Drains all pending outgoing VT protocol response bytes (e.g., `DSR`, `DA1/2`, `DECRQM`).
    pub fn drain_outgoing(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.outgoing)
    }

    /// Drains all pending [`TerminalEvent`] notifications.
    pub fn drain_events(&mut self) -> Vec<TerminalEvent> {
        std::mem::take(&mut self.events)
    }

    /// Starts an interactive selection at visible `(viewport_row, col)`.
    pub fn selection_start(&mut self, viewport_row: usize, col: usize, kind: SelectionKind) {
        let pt = self.viewport_to_grid_point(viewport_row, col);
        match kind {
            SelectionMode::Word => {
                let (a, h) =
                    selection::word_bounds_at(self.grid(), pt, &self.config.word_separators);
                self.selection = Some(Selection::new(SelectionMode::Word, a, h));
            }
            SelectionMode::Line => {
                let (a, h) = selection::line_bounds_at(self.grid(), pt.line);
                self.selection = Some(Selection::new(SelectionMode::Line, a, h));
            }
            SelectionMode::Stream | SelectionMode::Block => {
                self.selection = Some(Selection::new(kind, pt, pt));
            }
        }
    }

    /// Updates the moving head endpoint of the active selection to `(viewport_row, col)`.
    pub fn selection_update(&mut self, viewport_row: usize, col: usize) {
        let pt = self.viewport_to_grid_point(viewport_row, col);
        let Some(sel) = self.selection else {
            return;
        };
        match sel.mode {
            SelectionMode::Stream | SelectionMode::Block => {
                if let Some(active) = &mut self.selection {
                    active.head = pt;
                    active.head_past_eol = false;
                    active.finished = false;
                }
            }
            SelectionMode::Word => {
                let (wa, wh) =
                    selection::word_bounds_at(self.grid(), pt, &self.config.word_separators);
                if let Some(active) = &mut self.selection {
                    let pt_lt_origin_start = pt.line < active.origin_start.line
                        || (pt.line == active.origin_start.line
                            && (active.origin_start_past_eol || pt.col < active.origin_start.col));
                    let pt_gt_origin_end = pt.line > active.origin_end.line
                        || (!active.origin_end_past_eol
                            && pt.line == active.origin_end.line
                            && pt.col > active.origin_end.col);
                    if active.origin_evicted {
                        active.anchor = active.origin_start;
                        active.anchor_past_eol = active.origin_start_past_eol;
                        active.head = wh;
                        active.head_past_eol = false;
                    } else if active.origin_cropped_bottom {
                        if pt.line <= active.origin_end.line {
                            active.anchor = active.origin_end;
                            active.anchor_past_eol = true;
                            active.head = wa;
                            active.head_past_eol = false;
                        } else {
                            active.anchor =
                                GridPoint::new(AbsLine(active.origin_start.line.0 + 1), 0);
                            active.anchor_past_eol = false;
                            active.head = wh;
                            active.head_past_eol = false;
                        }
                    } else if pt_lt_origin_start {
                        active.anchor = active.origin_end;
                        active.anchor_past_eol = active.origin_end_past_eol;
                        active.head = wa;
                        active.head_past_eol = false;
                    } else if pt_gt_origin_end {
                        active.anchor = active.origin_start;
                        active.anchor_past_eol = active.origin_start_past_eol;
                        active.head = wh;
                        active.head_past_eol = false;
                    } else {
                        active.anchor = active.origin_start;
                        active.anchor_past_eol = active.origin_start_past_eol;
                        active.head = active.origin_end;
                        active.head_past_eol = active.origin_end_past_eol;
                    }
                    active.finished = false;
                }
            }
            SelectionMode::Line => {
                let (la, lh) = selection::line_bounds_at(self.grid(), pt.line);
                if let Some(active) = &mut self.selection {
                    if active.origin_evicted {
                        active.anchor = active.origin_start;
                        active.anchor_past_eol = active.origin_start_past_eol;
                        active.head = lh;
                        active.head_past_eol = false;
                    } else if active.origin_cropped_bottom {
                        if pt.line <= active.origin_end.line {
                            active.anchor = active.origin_end;
                            active.anchor_past_eol = true;
                            active.head = la;
                            active.head_past_eol = false;
                        } else {
                            active.anchor =
                                GridPoint::new(AbsLine(active.origin_start.line.0 + 1), 0);
                            active.anchor_past_eol = false;
                            active.head = lh;
                            active.head_past_eol = false;
                        }
                    } else if pt.line < active.origin_start.line {
                        active.anchor = active.origin_end;
                        active.anchor_past_eol = active.origin_end_past_eol;
                        active.head = la;
                        active.head_past_eol = false;
                    } else if pt.line > active.origin_end.line {
                        active.anchor = active.origin_start;
                        active.anchor_past_eol = active.origin_start_past_eol;
                        active.head = lh;
                        active.head_past_eol = false;
                    } else {
                        active.anchor = active.origin_start;
                        active.anchor_past_eol = active.origin_start_past_eol;
                        active.head = active.origin_end;
                        active.head_past_eol = active.origin_end_past_eol;
                    }
                    active.finished = false;
                }
            }
        }
    }

    /// Marks the active selection as finished.
    pub fn selection_finish(&mut self) {
        if let Some(sel) = &mut self.selection {
            sel.finished = true;
        }
    }

    /// Clears any active selection.
    pub fn selection_clear(&mut self) {
        self.selection = None;
    }

    /// Selects the word containing visible `(viewport_row, col)`.
    pub fn selection_word(&mut self, viewport_row: usize, col: usize) {
        self.selection_start(viewport_row, col, SelectionMode::Word);
        self.selection_finish();
    }

    /// Selects the entire logical line containing visible `(viewport_row, col)`.
    pub fn selection_line(&mut self, viewport_row: usize, col: usize) {
        self.selection_start(viewport_row, col, SelectionMode::Line);
        self.selection_finish();
    }

    /// Starts a rectangular block selection at visible `(viewport_row, col)`.
    pub fn selection_block(&mut self, viewport_row: usize, col: usize) {
        self.selection_start(viewport_row, col, SelectionMode::Block);
    }

    /// Returns a reference to the active [`Selection`], if any.
    #[must_use]
    pub const fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    /// Extracts the selected text from the active grid, or `None` if no selection is active.
    #[must_use]
    pub fn selection_text(&self) -> Option<String> {
        let sel = self.selection.as_ref()?;
        selection::extract_text(self.grid(), sel)
    }

    /// Returns `true` if the active selection would yield non-empty text, without allocating.
    #[must_use]
    pub fn has_non_empty_selection(&self) -> bool {
        let Some(sel) = self.selection.as_ref() else {
            return false;
        };
        selection::has_non_empty_text(self.grid(), sel)
    }

    /// Returns `true` if visible cell `(viewport_row, col)` is covered by the active selection.
    #[must_use]
    pub fn is_cell_selected(&self, viewport_row: usize, col: usize) -> bool {
        let Some(sel) = &self.selection else {
            return false;
        };
        let grid = self.grid();
        let Some(row) = grid.visible_row(viewport_row) else {
            return false;
        };
        let line = grid.screen_to_abs(viewport_row);
        sel.row_col_range(line, row)
            .is_some_and(|(c0, c1)| col >= c0 && col <= c1)
    }

    /// Returns a read-only reference to the active [`Grid`] (`primary` or `alternate`).
    #[must_use]
    pub const fn grid(&self) -> &Grid {
        if self.modes.alt_screen {
            &self.alternate
        } else {
            &self.primary
        }
    }

    /// Returns a read-only reference to the primary [`Grid`] regardless of active screen.
    #[must_use]
    pub const fn primary_grid(&self) -> &Grid {
        &self.primary
    }

    /// Returns active terminal [`Modes`].
    #[must_use]
    pub const fn modes(&self) -> &Modes {
        &self.modes
    }

    /// Returns the live [`Cursor`].
    #[must_use]
    pub const fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    /// Returns the effective [`ColorScheme`] (including `OSC 4/10/11/12` overrides).
    #[must_use]
    pub const fn palette(&self) -> &ColorScheme {
        &self.palette
    }

    /// Returns the monotonic palette/theme epoch incremented on any palette, theme, or special
    /// color change.
    #[must_use]
    pub const fn palette_epoch(&self) -> u64 {
        self.palette_epoch
    }

    /// Returns the special SGR attribute colors (`OSC 5` / `OSC 105`).
    #[must_use]
    pub const fn special_colors(&self) -> SpecialColors {
        self.special_colors
    }

    /// Returns the base [`Theme`] configured at construction or updated via [`Self::set_theme`].
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        &self.config.theme
    }

    /// Updates the base [`Theme`], updating non-overridden palette slots while preserving
    /// explicit `OSC 4/10/11/12` overrides.
    pub fn set_theme(&mut self, theme: Theme) {
        if !self.palette_overrides.foreground {
            self.palette.foreground = theme.scheme.foreground;
        }
        if !self.palette_overrides.background {
            self.palette.background = theme.scheme.background;
        }
        if !self.palette_overrides.cursor {
            self.palette.cursor = theme.scheme.cursor;
        }
        self.palette.cursor_text = theme.scheme.cursor_text;
        self.palette.selection_bg = theme.scheme.selection_bg;
        self.palette.selection_fg = theme.scheme.selection_fg;
        for (i, overridden) in self.palette_overrides.ansi.iter().enumerate() {
            if !*overridden {
                self.palette.ansi[i] = theme.scheme.ansi[i];
            }
        }
        self.config.theme = theme;
        self.palette_epoch = self.palette_epoch.saturating_add(1);
        if self.modes.color_scheme_updates {
            let code = self.color_scheme_dsr_code();
            let mut reply = String::with_capacity(12);
            if write!(reply, "\x1b[?997;{code}n").is_err() {
                // `String`'s `fmt::Write` impl is infallible.
            }
            self.outgoing.extend_from_slice(reply.as_bytes());
        }
    }

    const fn color_scheme_dsr_code(&self) -> u8 {
        let [r, g, b] = self.palette.background;
        let luma = (r as u32) * 299 + (g as u32) * 587 + (b as u32) * 114;
        if luma < 128_000 { 1 } else { 2 }
    }

    /// Returns explicit `OSC 4/10/11/12` override flags for the base palette.
    #[must_use]
    pub const fn palette_overrides(&self) -> &PaletteOverrides {
        &self.palette_overrides
    }

    /// Returns extended 256-color `OSC 4` overrides for indices `16..=255`.
    #[must_use]
    pub const fn extended_palette(&self) -> &[Option<[u8; 3]>; 240] {
        &self.extended_palette
    }

    /// Returns the effective window title set by `OSC 0/2`.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the effective icon name set by `OSC 0/1`.
    #[must_use]
    pub fn icon_name(&self) -> &str {
        &self.icon_name
    }

    /// Returns the current working directory set by `OSC 7`, if known.
    #[must_use]
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// Returns the current document URI set by `OSC 6`, if known.
    #[must_use]
    pub fn document_uri(&self) -> Option<&str> {
        self.document_uri.as_deref()
    }

    /// Returns the effective `OSC 52` clipboard read policy.
    #[must_use]
    pub const fn osc52_read_policy(&self) -> Osc52ReadPolicy {
        self.effective_osc52_read_policy()
    }

    /// Updates the `OSC 52` clipboard read policy.
    pub fn set_osc52_read_policy(&mut self, policy: Osc52ReadPolicy) {
        self.config.osc52_read_policy = policy;
        self.config.allow_osc52_read = matches!(policy, Osc52ReadPolicy::Allow);
    }

    const fn effective_osc52_read_policy(&self) -> Osc52ReadPolicy {
        match self.config.osc52_read_policy {
            Osc52ReadPolicy::Deny if self.config.allow_osc52_read => Osc52ReadPolicy::Allow,
            other => other,
        }
    }

    /// Returns the active grid's monotonic `damage_epoch`.
    #[must_use]
    pub const fn damage_epoch(&self) -> u64 {
        self.grid().damage_epoch()
    }

    /// Returns `true` when the alternate screen buffer is active.
    #[must_use]
    pub const fn is_alt_screen(&self) -> bool {
        self.modes.alt_screen
    }

    /// Returns `true` when DEC Mode 2026 synchronized output is currently latched.
    #[must_use]
    pub const fn is_synchronized_output(&self) -> bool {
        self.modes.sync_output
    }

    /// Returns the number of scrollback history lines retained on the active grid.
    #[must_use]
    pub fn scrollback_len(&self) -> usize {
        self.grid().history_len()
    }

    /// Returns total lines retained on the active grid (`scrollback_len() + rows`).
    #[must_use]
    pub fn total_lines(&self) -> usize {
        self.grid().total_len()
    }

    /// Looks up [`CellExtra`] on the active grid (`0` returns `None`).
    #[must_use]
    pub fn cell_extra(&self, extra_id: u32) -> Option<&CellExtra> {
        self.grid().cell_extra(extra_id)
    }

    /// Returns the [`HyperlinkPool`] storing interned `OSC 8` links.
    #[must_use]
    pub const fn hyperlink_pool(&self) -> &HyperlinkPool {
        &self.hyperlink_pool
    }

    /// Returns 0-based cursor position `(col, row)`.
    #[must_use]
    #[expect(clippy::cast_possible_truncation, reason = "viewport fits in u16")]
    pub const fn cursor_pos(&self) -> (u16, u16) {
        (self.cursor.col as u16, self.cursor.row as u16)
    }

    /// Returns the active [`CursorShape`].
    #[must_use]
    pub const fn cursor_shape(&self) -> CursorShape {
        self.cursor.style.shape
    }

    /// Returns whether cursor blinking is enabled.
    #[must_use]
    pub const fn cursor_blink(&self) -> bool {
        self.cursor.style.blink
    }

    /// Returns whether the cursor is visible (`DECTCEM`).
    #[must_use]
    pub const fn cursor_visible(&self) -> bool {
        self.modes.cursor_visible
    }

    /// Returns trimmed plain text for visible viewport `row` (`0..rows`), respecting
    /// `display_offset()`.
    #[must_use]
    pub fn visible_line_text(&self, row: u16) -> Option<String> {
        let grid = self.grid();
        let r = grid.visible_row(usize::from(row))?;
        Some(r.text(grid.extras()))
    }

    /// Returns trimmed plain text for all visible viewport rows from top to bottom.
    #[must_use]
    pub fn visible_lines(&self) -> Vec<String> {
        let (_, rows) = self.size();
        (0..rows)
            .filter_map(|r| self.visible_line_text(r))
            .collect()
    }

    /// Returns trimmed plain text for historical scrollback line `index` (`0` = oldest retained).
    #[must_use]
    pub fn scrollback_line_text(&self, index: usize) -> Option<String> {
        let grid = self.grid();
        if index >= grid.history_len() {
            return None;
        }
        grid.row_by_buf(index).map(|r| r.text(grid.extras()))
    }

    /// Returns all retained scrollback and active logical lines joined by `\n`, omitting trailing
    /// completely empty logical lines and rejoining `soft_wrapped` physical lines.
    #[must_use]
    pub fn full_text(&self) -> String {
        let grid = self.grid();
        let total = grid.total_len();
        let mut out = String::new();
        let mut non_empty_out_len: Option<usize> = None;

        let mut phys_start = 0usize;
        while phys_start < total {
            let mut phys_end = phys_start;
            while phys_end + 1 < total && grid.row_by_buf(phys_end).is_some_and(Row::soft_wrapped) {
                phys_end += 1;
            }

            if phys_start > 0 {
                out.push('\n');
            }

            // Locate the last non-empty cell across the logical line `phys_start..=phys_end`,
            // matching `reflow.rs` logical-line collection and trimming trailing default blank
            // cells even when a soft-wrapped continuation row was erased (e.g., via `EL 2`).
            let mut last_non_empty_pos: Option<(usize, usize)> = None;
            for r in (phys_start..=phys_end).rev() {
                let Some(row) = grid.row_by_buf(r) else {
                    continue;
                };
                let cells = row.cells();
                let limit = if r < phys_end {
                    if !cells.is_empty() && cells[cells.len() - 1].is_wrap_spacer() {
                        cells.len() - 1
                    } else {
                        cells.len()
                    }
                } else {
                    row.trimmed_len()
                };
                if let Some(pos) = cells[..limit].iter().rposition(|c| !c.is_empty()) {
                    last_non_empty_pos = Some((r, pos + 1));
                    break;
                }
            }

            if let Some((last_r, last_end)) = last_non_empty_pos {
                for r in phys_start..=last_r {
                    let Some(row) = grid.row_by_buf(r) else {
                        continue;
                    };
                    let cells = row.cells();
                    let take_end = if r == last_r {
                        last_end
                    } else if !cells.is_empty() && cells[cells.len() - 1].is_wrap_spacer() {
                        cells.len() - 1
                    } else {
                        cells.len()
                    };
                    for cell in &cells[..take_end] {
                        if cell.is_wide_spacer() || cell.is_wrap_spacer() || cell.is_sized_spacer()
                        {
                            continue;
                        }
                        if cell.extra_id != 0
                            && let Some(ext) = grid.cell_extra(cell.extra_id)
                        {
                            ext.write_grapheme(cell.codepoint, &mut out);
                        } else {
                            out.push(cell.codepoint);
                        }
                    }
                }
                non_empty_out_len = Some(out.len());
            }

            phys_start = phys_end + 1;
        }

        match non_empty_out_len {
            Some(len) => {
                out.truncate(len);
                out
            }
            None => String::new(),
        }
    }

    /// Returns the trimmed plain text of visible `row` from the active [`RenderSnapshot`]
    /// (returning the frozen pre-sync content when DEC Mode 2026 is latched).
    #[must_use]
    pub fn render_snapshot_line_text(&self, row: u16) -> Option<String> {
        self.active_snapshot().line_text(row)
    }

    /// Returns an immutable [`CellSnapshot`] for visible cell `(col, row)` respecting
    /// `display_offset()`.
    #[must_use]
    pub fn cell_at(&self, col: u16, row: u16) -> Option<CellSnapshot> {
        let grid = self.grid();
        let r = grid.visible_row(usize::from(row))?;
        let cell = *r.cells().get(usize::from(col))?;
        let is_wide = cell.is_wide_lead();
        let is_wide_spacer = cell.is_wide_spacer();
        let width = if is_wide_spacer || cell.is_wrap_spacer() || cell.is_sized_spacer() {
            0
        } else if is_wide {
            2
        } else {
            1
        };

        let mut grapheme = String::new();
        let mut underline_color = None;
        if !is_wide_spacer && !cell.is_wrap_spacer() && !cell.is_sized_spacer() {
            if cell.extra_id != 0
                && let Some(ext) = grid.cell_extra(cell.extra_id)
            {
                ext.write_grapheme(cell.codepoint, &mut grapheme);
                underline_color = ext.underline_color;
            } else {
                grapheme.push(cell.codepoint);
            }
        }

        Some(CellSnapshot {
            ch: cell.codepoint,
            grapheme,
            width,
            is_wide,
            is_wide_spacer,
            fg: cell.fg,
            bg: cell.bg,
            underline_color,
            underline_style: cell.underline_style(),
            flags: cell.flags,
        })
    }

    /// Returns the active [`RenderSnapshot`] for rendering (latched pre-sync snapshot during
    /// DEC Mode 2026, or the live grid view otherwise), without heap-allocating visible row vectors.
    #[must_use]
    pub fn active_snapshot(&self) -> RenderSnapshot<'_> {
        if self.modes.sync_output && self.latched_frame.active {
            let latched = &self.latched_frame;
            return RenderSnapshot {
                cols: latched.cols,
                rows: latched.rows,
                visible_rows: VisibleRowsView::from_slices(&latched.visible_rows, &[]),
                cursor: latched.cursor,
                modes: &latched.modes,
                palette: &latched.palette,
                palette_overrides: latched.palette_overrides,
                extended_palette: &latched.extended_palette,
                special_colors: latched.special_colors,
                palette_epoch: latched.palette_epoch,
                selection: latched.selection.as_ref(),
                display_offset: latched.display_offset,
                scrollback_len: latched.scrollback_len,
                is_alt_screen: latched.is_alt_screen,
                is_synchronized_output: true,
                damage_epoch: latched.damage_epoch,
                top_abs_line: latched.top_abs_line,
                extras: SnapshotExtras::Latched(&latched.extras),
                #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
                visible_images: Cow::Borrowed(&latched.visible_images),
                #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
                image_store: &self.image_store,
            };
        }

        let grid = self.grid();
        let (cols, rows) = self.size();
        let (first, second) = grid.visible_slices();
        let visible_rows = VisibleRowsView::from_slices(first, second);
        let display_offset = self.display_offset();
        let cursor = self.live_cursor_snapshot(display_offset);
        let top_abs_line = grid.screen_to_abs(0);
        #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
        let visible_images = {
            let screen = if self.modes.alt_screen {
                ScreenKind::Alternate
            } else {
                ScreenKind::Primary
            };
            Cow::Owned(
                self.image_store
                    .visible_placements(screen, top_abs_line, rows),
            )
        };
        RenderSnapshot {
            cols,
            rows,
            visible_rows,
            cursor,
            modes: &self.modes,
            palette: &self.palette,
            palette_overrides: self.palette_overrides,
            extended_palette: &self.extended_palette,
            special_colors: self.special_colors.as_array(),
            palette_epoch: self.palette_epoch,
            selection: self.selection.as_ref(),
            display_offset,
            scrollback_len: grid.history_len(),
            is_alt_screen: self.modes.alt_screen,
            is_synchronized_output: false,
            damage_epoch: grid.damage_epoch(),
            top_abs_line,
            extras: SnapshotExtras::Live(grid.extras()),
            #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
            visible_images,
            #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
            image_store: &self.image_store,
        }
    }

    /// Returns all inline image placements on the active screen.
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    #[must_use]
    pub fn image_placements(&self) -> &[ImagePlacement] {
        let screen = if self.modes.alt_screen {
            ScreenKind::Alternate
        } else {
            ScreenKind::Primary
        };
        self.image_store.placements(screen)
    }

    /// Looks up decoded pixel data for inline image `id`.
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    #[must_use]
    pub fn image_data(&self, id: u32) -> Option<&StoredImage> {
        self.image_store.get_image(id)
    }

    /// Configures the Kitty file-medium (`t=f`/`t=t`) access policy at runtime.
    #[cfg(feature = "kitty-graphics")]
    pub fn set_kitty_file_medium_policy(&mut self, policy: KittyFileMediumPolicy) {
        self.config.kitty_file_medium_policy = policy;
    }

    /// Returns the active Kitty file-medium (`t=f`/`t=t`) access policy.
    #[cfg(feature = "kitty-graphics")]
    #[must_use]
    pub const fn kitty_file_medium_policy(&self) -> KittyFileMediumPolicy {
        self.config.kitty_file_medium_policy
    }

    /// Installs or clears a custom [`KittyShmReader`] for `t=s` shared-memory image transfers.
    #[cfg(feature = "kitty-graphics")]
    pub fn set_kitty_shm_reader(&mut self, reader: Option<Box<dyn KittyShmReader + Send + Sync>>) {
        self.kitty_shm_reader.0 = reader;
    }

    /// Advances all playing Kitty graphics animations to `now_secs`, marking dirty rows if any
    /// displayed animation frame changed.
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    pub fn tick_animations(&mut self, now_secs: f64) -> bool {
        let changed = self.image_store.tick_animations(now_secs);
        if changed {
            self.active_grid_mut().bump_damage_epoch();
            self.active_grid_mut().mark_all_dirty();
        }
        changed
    }

    /// Returns the earliest timestamp (`now_secs`) when any active animation needs its next frame.
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    #[must_use]
    pub fn next_animation_deadline(&self) -> Option<f64> {
        self.image_store.next_animation_deadline()
    }

    /// Encodes a grid-relative [`MouseInputEvent`] according to active mouse modes, using
    /// [`Self::cell_pixel_size`] as fallback when `DEC ? 1016` SGR-Pixels is active.
    #[must_use]
    pub fn encode_mouse_event(&self, ev: MouseInputEvent) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(20);
        if self.encode_mouse_event_into(ev, &mut out) {
            Some(out)
        } else {
            None
        }
    }

    /// Appends VT mouse reporting bytes for `ev` into `out` without allocating, returning `true`
    /// if bytes were written. Uses [`Self::cell_pixel_size`] when `DEC ? 1016` is active.
    pub fn encode_mouse_event_into(&self, ev: MouseInputEvent, out: &mut Vec<u8>) -> bool {
        mouse::encode_mouse_event_with_cell_px_into(
            &self.modes,
            ev,
            None,
            self.effective_cell_pixel_size(),
            out,
        )
    }

    /// Encodes a [`MouseInputEvent`] with pixel coordinates `(pixel_x, pixel_y)` for
    /// `DEC ? 1016` SGR-Pixels mode, falling back to grid encoding for other modes.
    #[must_use]
    pub fn encode_mouse_event_pixels(
        &self,
        ev: MouseInputEvent,
        pixel_x: u32,
        pixel_y: u32,
    ) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(24);
        if self.encode_mouse_event_pixels_into(ev, pixel_x, pixel_y, &mut out) {
            Some(out)
        } else {
            None
        }
    }

    /// Appends VT mouse reporting bytes for `ev` with explicit `(pixel_x, pixel_y)` into `out`
    /// without allocating, returning `true` if bytes were written.
    pub fn encode_mouse_event_pixels_into(
        &self,
        ev: MouseInputEvent,
        pixel_x: u32,
        pixel_y: u32,
        out: &mut Vec<u8>,
    ) -> bool {
        mouse::encode_mouse_event_with_cell_px_into(
            &self.modes,
            ev,
            Some((pixel_x, pixel_y)),
            self.effective_cell_pixel_size(),
            out,
        )
    }

    /// Formats pasted text for transmission according to DEC `?2004` bracketed paste mode.
    #[must_use]
    pub fn format_paste(&self, text: &str) -> Vec<u8> {
        format_paste_bytes(&self.modes, text)
    }

    /// Appends bracketed-paste-formatted and sanitized `text` into `out` without extra allocations.
    pub fn format_paste_into(&self, text: &str, out: &mut Vec<u8>) {
        format_paste_bytes_into(&self.modes, text, out);
    }

    /// Returns focus report bytes (`\x1b[I` / `\x1b[O`) if DEC `?1004` focus reporting is active.
    #[must_use]
    pub const fn focus_report(&self, focused: bool) -> Option<&'static [u8]> {
        focus_event_bytes(&self.modes, focused)
    }

    /// Returns the currently configured xterm `modifyOtherKeys` level (`0..=2`).
    #[must_use]
    pub const fn modify_other_keys_level(&self) -> u8 {
        self.modes.modify_other_keys
    }

    /// Enqueues an `OSC 52` clipboard read response after the host fulfills
    /// [`TerminalEvent::ClipboardReadRequested`] or [`TerminalEvent::ClipboardReadPrompt`].
    pub fn provide_clipboard_read(&mut self, selection: ClipboardSelection, text: &str) {
        let sel_char = match selection {
            ClipboardSelection::Clipboard => 'c',
            ClipboardSelection::Primary => 'p',
            ClipboardSelection::Secondary => 's',
            ClipboardSelection::Select => 'q',
        };
        let encoded = encode_base64(text.as_bytes());
        let reply = format!("\x1b]52;{sel_char};{encoded}\x1b\\");
        self.outgoing.extend_from_slice(reply.as_bytes());
    }

    // -------------------------------------------------------------------------
    // Feature-gated public APIs (search, hyperlinks, shell-integration,
    // kitty-keyboard, serde, metrics)
    // -------------------------------------------------------------------------

    /// Searches the active terminal grid (including primary scrollback) for `query` and stores
    /// the resulting matches for navigation via [`Self::next_search_match`] and
    /// [`Self::prev_search_match`].
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::InvalidRegex`] without mutating terminal or existing search state
    /// if `query.is_regex` is `true` and the pattern fails to compile.
    #[cfg(feature = "search")]
    pub fn search(&mut self, query: &SearchQuery) -> Result<Vec<SearchMatch>, SearchError> {
        let results = {
            let grid = if self.modes.alt_screen {
                &self.alternate
            } else {
                &self.primary
            };
            self.search_state.execute(grid, query)?
        };
        self.incremental_search_active = false;
        self.take_search_dirty_abs(self.modes.alt_screen);
        Ok(results)
    }

    /// Incrementally updates active search matches for mutations since the last search/refresh.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::InvalidRegex`] if the stored regex query fails to recompile.
    #[cfg(feature = "search")]
    pub fn refresh_search(&mut self) -> Result<bool, SearchError> {
        let dirty_from = self.take_search_dirty_abs(self.modes.alt_screen);
        let grid = if self.modes.alt_screen {
            &self.alternate
        } else {
            &self.primary
        };
        let res = self.search_state.refresh(grid, dirty_from);
        if !matches!(res, Ok(false)) {
            self.incremental_search_active = false;
        }
        res
    }

    /// Starts an incremental chunked search for `query` over the active grid.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::InvalidRegex`] without mutating terminal or existing search state
    /// if `query.is_regex` is `true` and the pattern fails to compile.
    #[cfg(feature = "search")]
    pub fn begin_incremental_search(
        &mut self,
        query: &SearchQuery,
    ) -> Result<SearchStepProgress, SearchError> {
        let prog = {
            let grid = if self.modes.alt_screen {
                &self.alternate
            } else {
                &self.primary
            };
            self.search_state.begin_incremental(grid, query)?
        };
        if prog.complete {
            self.incremental_search_active = false;
            self.take_search_dirty_abs(self.modes.alt_screen);
        } else {
            self.incremental_search_active = true;
        }
        Ok(prog)
    }

    /// Advances an active chunked incremental search by up to `max_lines` physical lines.
    #[cfg(feature = "search")]
    pub fn step_incremental_search(&mut self, max_lines: usize) -> SearchStepProgress {
        let prog = {
            let grid = if self.modes.alt_screen {
                &self.alternate
            } else {
                &self.primary
            };
            self.search_state.step_incremental(grid, max_lines)
        };
        if self.incremental_search_active && prog.complete {
            self.incremental_search_active = false;
            self.take_search_dirty_abs(self.modes.alt_screen);
        }
        prog
    }

    /// Advances to the next search match (wrapping to the first match), scrolls it into view
    /// on the primary screen, and returns it.
    #[cfg(feature = "search")]
    pub fn next_search_match(&mut self) -> Option<&SearchMatch> {
        let target_line = {
            let grid = if self.modes.alt_screen {
                &self.alternate
            } else {
                &self.primary
            };
            self.search_state.step_next(grid)?.line
        };
        self.scroll_match_into_view(target_line);
        self.search_state.active_match(self.grid())
    }

    /// Steps backward to the previous search match (wrapping to the last match), scrolls it into
    /// view on the primary screen, and returns it.
    #[cfg(feature = "search")]
    pub fn prev_search_match(&mut self) -> Option<&SearchMatch> {
        let target_line = {
            let grid = if self.modes.alt_screen {
                &self.alternate
            } else {
                &self.primary
            };
            self.search_state.step_prev(grid)?.line
        };
        self.scroll_match_into_view(target_line);
        self.search_state.active_match(self.grid())
    }

    /// Returns the currently focused search match, if still valid for the active screen content.
    #[cfg(feature = "search")]
    #[must_use]
    pub fn active_search_match(&self) -> Option<&SearchMatch> {
        self.search_state.active_match(self.grid())
    }

    /// Returns the 0-based index of the active search match, if any.
    #[cfg(feature = "search")]
    #[must_use]
    pub fn active_search_match_index(&self) -> Option<usize> {
        self.search_state.active_index(self.grid())
    }

    /// Returns all currently cached search matches, or an empty slice if stale.
    #[cfg(feature = "search")]
    #[must_use]
    pub fn search_matches(&self) -> &[SearchMatch] {
        self.search_state.matches(self.grid())
    }

    /// Returns cached search matches intersecting 0-based retained `line` in `O(log M)` time.
    #[cfg(feature = "search")]
    #[must_use]
    pub fn search_matches_on_line(&self, line: usize) -> &[SearchMatch] {
        self.search_state.matches_on_line(self.grid(), line)
    }

    /// Returns cached search matches intersecting `abs_line` in `O(log M)` time.
    #[cfg(feature = "search")]
    #[must_use]
    pub fn search_matches_on_abs_line(&self, abs_line: AbsLine) -> &[SearchMatch] {
        self.search_state.matches_on_abs_line(self.grid(), abs_line)
    }

    /// Returns cached search matches intersecting `[start_line, end_line_excl)` in `O(log M)` time.
    #[cfg(feature = "search")]
    #[must_use]
    pub fn search_matches_in_range(
        &self,
        start_line: usize,
        end_line_excl: usize,
    ) -> &[SearchMatch] {
        self.search_state
            .matches_in_range(self.grid(), start_line, end_line_excl)
    }

    /// Clears any active search matches and focus state.
    #[cfg(feature = "search")]
    pub fn clear_search(&mut self) {
        self.search_state.clear();
        self.incremental_search_active = false;
    }

    /// Resolves an explicit `OSC 8` or implicitly detected URL at visible `(col, row)`,
    /// respecting `display_offset()`.
    #[cfg(feature = "hyperlinks")]
    #[must_use]
    pub fn hyperlink_at(&self, col: u16, row: u16) -> Option<&HyperlinkInfo> {
        let grid = self.grid();
        let r = grid.visible_row(usize::from(row))?;
        let mut c_idx = usize::from(col);
        let cells = r.cells();
        let cell = *cells.get(c_idx)?;
        if cell.is_wrap_spacer() {
            return None;
        }
        if cell.is_wide_spacer() {
            if c_idx == 0 || !cells[c_idx - 1].is_wide_lead() {
                return None;
            }
            c_idx -= 1;
        }
        let resolved_cell = cells[c_idx];
        if resolved_cell.extra_id != 0
            && let Some(ext) = grid.cell_extra(resolved_cell.extra_id)
            && let Some(hid) = ext.hyperlink_id
        {
            return self.hyperlink_pool.get(hid);
        }
        let store = if self.modes.alt_screen {
            &self.alt_implicit_links
        } else {
            &self.primary_implicit_links
        };
        let abs_line = grid.screen_to_abs(usize::from(row));
        store.find_at(abs_line, u16::try_from(c_idx).unwrap_or(u16::MAX))
    }

    /// Activates any hyperlink at visible `(col, row)`, emitting
    /// [`TerminalEvent::HyperlinkActivated`] and returning `true` if a link was found.
    #[cfg(feature = "hyperlinks")]
    pub fn activate_hyperlink_at(&mut self, col: u16, row: u16) -> bool {
        let Some(info) = self.hyperlink_at(col, row).cloned() else {
            return false;
        };
        self.events.push(TerminalEvent::HyperlinkActivated {
            uri: info.uri,
            id: info.id,
        });
        true
    }

    /// Resolves the hyperlink at visible `(col, row)` together with all visible viewport
    /// [`HyperlinkSpan`] segments belonging to that link (for hover underline highlights).
    #[cfg(feature = "hyperlinks")]
    #[must_use]
    pub fn hyperlink_visible_spans_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<(&HyperlinkInfo, Vec<HyperlinkSpan>)> {
        let grid = self.grid();
        let r = grid.visible_row(usize::from(row))?;
        let mut c_idx = usize::from(col);
        let cells = r.cells();
        let cell = *cells.get(c_idx)?;
        if cell.is_wrap_spacer() {
            return None;
        }
        if cell.is_wide_spacer() {
            if c_idx == 0 || !cells[c_idx - 1].is_wide_lead() {
                return None;
            }
            c_idx -= 1;
        }
        let resolved_cell = cells[c_idx];
        if resolved_cell.extra_id != 0
            && let Some(ext) = grid.cell_extra(resolved_cell.extra_id)
            && let Some(hid) = ext.hyperlink_id
            && let Some(info) = self.hyperlink_pool.get(hid)
        {
            let spans = explicit_visible_spans(
                grid,
                hid,
                info.id.is_some(),
                row,
                u16::try_from(c_idx).unwrap_or(u16::MAX),
            );
            return Some((info, spans));
        }

        let store = if self.modes.alt_screen {
            &self.alt_implicit_links
        } else {
            &self.primary_implicit_links
        };
        let abs_line = grid.screen_to_abs(usize::from(row));
        store.visible_spans_at(grid, abs_line, u16::try_from(c_idx).unwrap_or(u16::MAX))
    }

    /// Returns a read-only view of retained `OSC 133` semantic [`ShellZone`](crate::ShellZone)s
    /// on the primary screen.
    #[cfg(feature = "shell-integration")]
    #[must_use]
    pub fn shell_zones(&self) -> ShellZonesView<'_> {
        self.shell_integration.zones_view(&self.primary)
    }

    /// Scrolls the primary viewport to the `delta`-th previous (`delta < 0`) or next
    /// (`delta > 0`) `OSC 133` prompt zone, returning `true` if `display_offset()` changed.
    #[cfg(feature = "shell-integration")]
    pub fn jump_to_prompt(&mut self, delta: i32) -> bool {
        if self.modes.alt_screen {
            return false;
        }
        self.shell_integration
            .jump_to_prompt(&mut self.primary, delta)
    }

    /// Extracts the plain text of the `OSC 133` command output zone associated with 0-based
    /// retained `line` (`0..total_lines()`), if any.
    #[cfg(feature = "shell-integration")]
    #[must_use]
    pub fn command_output_at(&self, line: usize) -> Option<String> {
        self.shell_integration
            .command_output_at(&self.primary, line)
    }

    /// Returns the currently active Kitty Keyboard Protocol enhancement flags for the active
    /// screen (`primary` or `alternate`).
    #[cfg(feature = "kitty-keyboard")]
    #[must_use]
    pub fn kitty_keyboard_flags(&self) -> KittyKeyboardFlags {
        self.kitty_keyboard.flags(self.modes.alt_screen)
    }

    /// Returns the active xterm `modifyOtherKeys` enum level (`kitty-keyboard` feature).
    #[cfg(feature = "kitty-keyboard")]
    #[must_use]
    pub const fn modify_other_keys(&self) -> ModifyOtherKeys {
        match ModifyOtherKeys::from_pv(self.modes.modify_other_keys as u16) {
            Some(level) => level,
            None => ModifyOtherKeys::Disabled,
        }
    }

    /// Encodes `ev` with xterm `modifyOtherKeys` into `out` when active and Kitty keyboard flags
    /// are empty, returning `true` if bytes were written.
    #[cfg(feature = "kitty-keyboard")]
    pub fn encode_modify_other_keys_into(&self, ev: &KittyKeyEvent<'_>, out: &mut Vec<u8>) -> bool {
        if !self.kitty_keyboard_flags().is_empty() {
            return false;
        }
        encode_modify_other_keys_into(self.modify_other_keys(), ev, out)
    }

    /// Encodes `ev` into `out` honoring strict Kitty keyboard precedence over `modifyOtherKeys`.
    #[cfg(feature = "kitty-keyboard")]
    pub fn encode_key_event_into(&self, ev: &KittyKeyEvent<'_>, out: &mut Vec<u8>) -> bool {
        let flags = self.kitty_keyboard_flags();
        if !flags.is_empty() {
            return crate::features::kitty_keyboard::encode_kitty_key_into(flags, ev, out);
        }
        let mok = self.modify_other_keys();
        if mok != ModifyOtherKeys::Disabled && encode_modify_other_keys_into(mok, ev, out) {
            return true;
        }
        crate::features::kitty_keyboard::encode_kitty_key_into(KittyKeyboardFlags::empty(), ev, out)
    }

    /// Exports a serializable full-state [`TerminalSnapshot`] (`serde` feature).
    #[cfg(feature = "serde")]
    #[must_use]
    pub fn export_snapshot(&self) -> TerminalSnapshot {
        #[expect(clippy::cast_possible_truncation, reason = "grid initialized from u16")]
        let cols = self.primary.cols() as u16;
        #[expect(clippy::cast_possible_truncation, reason = "grid initialized from u16")]
        let rows = self.primary.viewport_rows() as u16;
        let mut extended_palette = Vec::new();
        for (idx, entry) in self.extended_palette.iter().enumerate() {
            if let Some(rgb) = entry {
                #[expect(clippy::cast_possible_truncation, reason = "16 + 239 <= 255")]
                let color_idx = (16 + idx) as u8;
                extended_palette.push((color_idx, *rgb));
            }
        }
        #[cfg(feature = "kitty-keyboard")]
        let (kitty_primary_stack, kitty_alt_stack) = self.kitty_keyboard.export_stacks();

        TerminalSnapshot {
            cols,
            rows,
            primary: self.primary.export_snapshot(),
            alternate: self.alternate.export_snapshot(),
            modes: self.modes,
            cursor: self.cursor,
            alt_cursor_backup: self.alt_cursor_backup,
            primary_saved_cursor: self.primary_saved_cursor,
            alt_saved_cursor: self.alt_saved_cursor,
            tabs: self.tabs.clone(),
            scroll_top: self.scroll_top,
            scroll_bottom: self.scroll_bottom,
            scroll_left: self.scroll_left,
            scroll_right: self.scroll_right,
            palette: self.palette.clone(),
            palette_overrides: self.palette_overrides,
            extended_palette,
            special_colors: self.special_colors,
            title: self.title.clone(),
            icon_name: self.icon_name.clone(),
            cwd: self.cwd.clone(),
            document_uri: self.document_uri.clone(),
            hyperlink_pool: self.hyperlink_pool.clone(),
            selection: self.selection,
            #[cfg(feature = "shell-integration")]
            shell_integration: Some(self.shell_integration.snapshot_clone()),
            #[cfg(feature = "kitty-keyboard")]
            kitty_primary_stack,
            #[cfg(feature = "kitty-keyboard")]
            kitty_alt_stack,
        }
    }

    /// Restores a [`TerminalState`] from a [`TerminalSnapshot`] and `config`, defensively sanitizing
    /// dimensions, cursor bounds, scroll margins, hyperlink pools, and sparse `CellExtra` ownership.
    #[cfg(feature = "serde")]
    #[must_use]
    pub fn from_snapshot(snapshot: TerminalSnapshot, config: CoreConfig) -> Self {
        let mut hyperlink_pool = snapshot.hyperlink_pool;
        hyperlink_pool.sanitize();

        let primary = Grid::from_snapshot(
            ScreenKind::Primary,
            snapshot.primary,
            config.scrollback_limit,
            &hyperlink_pool,
        );
        let mut alternate = Grid::from_snapshot(
            ScreenKind::Alternate,
            snapshot.alternate,
            0,
            &hyperlink_pool,
        );
        let actual_cols = primary.cols();
        let actual_rows = primary.viewport_rows();
        if alternate.cols() != actual_cols || alternate.viewport_rows() != actual_rows {
            alternate.resize(actual_cols, actual_rows, ReflowAnchors::default());
        }

        let mut modes = snapshot.modes;
        modes.sync_output = false;
        if modes.modify_other_keys > 2 {
            modes.modify_other_keys = 0;
        }

        let clamp_cursor = |mut cur: Cursor| -> Cursor {
            cur.row = cur.row.min(actual_rows - 1);
            cur.col = cur.col.min(actual_cols - 1);
            cur.pending_wrap &= cur.col == actual_cols - 1;
            cur.attrs.flags &= CellFlags::WRITE_ATTR_MASK;
            cur.charsets.gl = cur.charsets.gl.min(3);
            cur.charsets.gr = cur.charsets.gr.min(3);
            cur.charsets.single_shift = cur.charsets.single_shift.filter(|&s| s == 2 || s == 3);
            if cur
                .attrs
                .hyperlink_id
                .is_some_and(|hid| hyperlink_pool.get(hid).is_none())
            {
                cur.attrs.hyperlink_id = None;
            }
            cur
        };
        let clamp_saved = |mut saved: SavedCursor| -> SavedCursor {
            saved.row = saved.row.min(actual_rows - 1);
            saved.col = saved.col.min(actual_cols - 1);
            saved.pending_wrap &= saved.col == actual_cols - 1;
            saved.attrs.flags &= CellFlags::WRITE_ATTR_MASK;
            saved.charsets.gl = saved.charsets.gl.min(3);
            saved.charsets.gr = saved.charsets.gr.min(3);
            saved.charsets.single_shift = saved.charsets.single_shift.filter(|&s| s == 2 || s == 3);
            if saved
                .attrs
                .hyperlink_id
                .is_some_and(|hid| hyperlink_pool.get(hid).is_none())
            {
                saved.attrs.hyperlink_id = None;
            }
            saved
        };

        let cursor = clamp_cursor(snapshot.cursor);
        let alt_cursor_backup = snapshot.alt_cursor_backup.map(clamp_cursor);
        let primary_saved_cursor = snapshot.primary_saved_cursor.map(clamp_saved);
        let alt_saved_cursor = snapshot.alt_saved_cursor.map(clamp_saved);

        let mut tabs = snapshot.tabs;
        tabs.resize(actual_cols);

        let (scroll_top, scroll_bottom) = if snapshot.scroll_top < snapshot.scroll_bottom
            && snapshot.scroll_bottom < actual_rows
        {
            (snapshot.scroll_top, snapshot.scroll_bottom)
        } else {
            (0, actual_rows - 1)
        };

        let (scroll_left, scroll_right) = if modes.left_right_margin_mode
            && snapshot.scroll_left < snapshot.scroll_right
            && snapshot.scroll_right < actual_cols
        {
            (snapshot.scroll_left, snapshot.scroll_right)
        } else {
            (0, actual_cols - 1)
        };

        let mut extended_palette = [None; 240];
        for (idx, rgb) in snapshot.extended_palette {
            if idx >= 16 {
                extended_palette[(idx - 16) as usize] = Some(rgb);
            }
        }

        let active = if modes.alt_screen {
            &alternate
        } else {
            &primary
        };
        let oldest = active.oldest_abs_line();
        let newest = active.newest_abs_line();
        let selection = snapshot.selection.filter(|sel| {
            [sel.anchor, sel.head, sel.origin_start, sel.origin_end]
                .into_iter()
                .all(|pt| pt.line >= oldest && pt.line <= newest)
        });

        #[cfg(feature = "shell-integration")]
        let shell_integration = {
            let mut si = snapshot.shell_integration.unwrap_or_else(|| {
                ShellIntegrationState::new(config.scrollback_limit, actual_rows)
            });
            si.sanitize_snapshot(&primary);
            si.set_viewport_rows(config.scrollback_limit, actual_rows);
            si
        };

        #[cfg(feature = "kitty-keyboard")]
        let kitty_keyboard = {
            let mut ks = KittyKeyboardState::default();
            ks.restore_stacks(snapshot.kitty_primary_stack, snapshot.kitty_alt_stack);
            ks
        };

        let term = Self {
            config,
            primary,
            alternate,
            modes,
            cursor,
            alt_cursor_backup,
            primary_saved_cursor,
            alt_saved_cursor,
            tabs,
            scroll_top,
            scroll_bottom,
            scroll_left,
            scroll_right,
            pixel_width: 0,
            pixel_height: 0,
            cell_pixel_width: 0,
            cell_pixel_height: 0,
            window_pos: (0, 0),
            screen_char_size: None,
            window_iconified: false,
            palette: snapshot.palette,
            palette_overrides: snapshot.palette_overrides,
            extended_palette,
            special_colors: snapshot.special_colors,
            palette_epoch: 0,
            title: snapshot.title,
            icon_name: snapshot.icon_name,
            title_stack: Vec::new(),
            cwd: snapshot.cwd,
            document_uri: snapshot.document_uri,
            hyperlink_pool,
            selection,
            outgoing: Vec::new(),
            events: Vec::new(),
            dcs_state: DcsState::default(),
            mouse_shape_stack: Vec::new(),
            osc99_pending: VecDeque::new(),
            sgr_stack: SgrStack::default(),
            sync_started_at: None,
            last_time_secs: None,
            latched_frame: LatchedFrame::default(),
            last_written: None,
            last_graphic: None,
            grapheme_scratch: String::with_capacity(16),
            #[cfg(feature = "search")]
            search_state: SearchState::default(),
            #[cfg(feature = "search")]
            incremental_search_active: false,
            #[cfg(feature = "search")]
            search_dirty_abs_pri: None,
            #[cfg(feature = "search")]
            search_dirty_abs_alt: None,
            #[cfg(feature = "hyperlinks")]
            primary_implicit_links: ImplicitLinkStore::default(),
            #[cfg(feature = "hyperlinks")]
            alt_implicit_links: ImplicitLinkStore::default(),
            #[cfg(feature = "hyperlinks")]
            osc8_inserts_since_gc: 0,
            #[cfg(feature = "hyperlinks")]
            hyperlink_gc_threshold: 256,
            #[cfg(feature = "hyperlinks")]
            hyperlink_gc_bits: Vec::new(),
            #[cfg(feature = "shell-integration")]
            shell_integration,
            #[cfg(feature = "kitty-keyboard")]
            kitty_keyboard,
            #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
            image_store: ImageStore::default(),
            #[cfg(feature = "kitty-graphics")]
            kitty_shm_reader: ShmReaderSlot::default(),
            #[cfg(feature = "sixel")]
            active_sixel: None,
            #[cfg(feature = "sixel")]
            sixel_shared_palette: Vec::new(),
            #[cfg(feature = "metrics")]
            metrics: TerminalMetrics::new(),
        };

        #[cfg(feature = "hyperlinks")]
        let term = {
            let mut term = term;
            term.refresh_implicit_hyperlinks();
            term
        };
        term
    }

    /// Returns monotonic parser/render/reflow [`TerminalMetrics`] (`metrics` feature).
    #[cfg(feature = "metrics")]
    #[must_use]
    pub const fn metrics(&self) -> &TerminalMetrics {
        &self.metrics
    }

    /// Records a rendered frame with `dirty_rows` updated rows (`metrics` feature).
    #[cfg(feature = "metrics")]
    pub fn record_frame_rendered(&mut self, dirty_rows: u64) {
        self.metrics.record_frame(dirty_rows);
    }

    // -------------------------------------------------------------------------
    // Internal helpers
    // -------------------------------------------------------------------------

    #[cfg(feature = "search")]
    fn take_search_dirty_abs(&mut self, alt_screen: bool) -> Option<AbsLine> {
        let (acc, grid) = if alt_screen {
            (&mut self.search_dirty_abs_alt, &mut self.alternate)
        } else {
            (&mut self.search_dirty_abs_pri, &mut self.primary)
        };
        let grid_dirty = grid.take_dirty_abs_start();
        match (acc.take(), grid_dirty) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    #[cfg(feature = "search")]
    fn scroll_match_into_view(&mut self, line: usize) {
        if self.modes.alt_screen {
            return;
        }
        let vrows = self.primary.viewport_rows();
        let history_len = self.primary.history_len();
        let top = history_len.saturating_sub(self.primary.display_offset());
        if line < top || line >= top + vrows {
            let desired_top = line.saturating_sub(vrows / 2).min(history_len);
            let target_offset = history_len.saturating_sub(desired_top);
            self.primary.set_display_offset(target_offset);
        }
    }

    #[cfg(feature = "hyperlinks")]
    fn refresh_implicit_hyperlinks(&mut self) {
        let d_pri = self.primary.take_dirty_abs_start();
        #[cfg(feature = "search")]
        if let Some(d) = d_pri {
            self.search_dirty_abs_pri = Some(match self.search_dirty_abs_pri {
                Some(prev) => prev.min(d),
                None => d,
            });
        }
        self.primary_implicit_links
            .refresh_dirty(&self.primary, d_pri);

        let d_alt = self.alternate.take_dirty_abs_start();
        #[cfg(feature = "search")]
        if let Some(d) = d_alt {
            self.search_dirty_abs_alt = Some(match self.search_dirty_abs_alt {
                Some(prev) => prev.min(d),
                None => d,
            });
        }
        self.alt_implicit_links
            .refresh_dirty(&self.alternate, d_alt);
    }

    #[cfg(feature = "hyperlinks")]
    fn mark_live_hyperlink_bit(bits: &mut [u64], hid: crate::cell::HyperlinkId) {
        let idx = hid.index();
        if let Some(word) = bits.get_mut(idx / 64) {
            *word |= 1u64 << (idx % 64);
        }
    }

    #[cfg(feature = "hyperlinks")]
    fn maybe_gc_hyperlink_pool(&mut self) {
        if self.hyperlink_pool.len() <= self.hyperlink_gc_threshold
            || self.osc8_inserts_since_gc < 64
        {
            return;
        }
        let words = self.hyperlink_pool.slots_len().div_ceil(64);
        self.hyperlink_gc_bits.clear();
        self.hyperlink_gc_bits.resize(words, 0);

        self.primary
            .extras()
            .collect_live_hyperlink_ids(&mut self.hyperlink_gc_bits);
        self.alternate
            .extras()
            .collect_live_hyperlink_ids(&mut self.hyperlink_gc_bits);
        if let Some(hid) = self.cursor.attrs.hyperlink_id {
            Self::mark_live_hyperlink_bit(&mut self.hyperlink_gc_bits, hid);
        }
        if let Some(ref cur) = self.alt_cursor_backup
            && let Some(hid) = cur.attrs.hyperlink_id
        {
            Self::mark_live_hyperlink_bit(&mut self.hyperlink_gc_bits, hid);
        }
        if let Some(ref saved) = self.primary_saved_cursor
            && let Some(hid) = saved.attrs.hyperlink_id
        {
            Self::mark_live_hyperlink_bit(&mut self.hyperlink_gc_bits, hid);
        }
        if let Some(ref saved) = self.alt_saved_cursor
            && let Some(hid) = saved.attrs.hyperlink_id
        {
            Self::mark_live_hyperlink_bit(&mut self.hyperlink_gc_bits, hid);
        }
        if self.latched_frame.active {
            for (_, ext) in &self.latched_frame.extras {
                if let Some(hid) = ext.hyperlink_id {
                    Self::mark_live_hyperlink_bit(&mut self.hyperlink_gc_bits, hid);
                }
            }
        }
        self.hyperlink_pool
            .gc_unreferenced_bitset(&self.hyperlink_gc_bits);
        self.hyperlink_gc_threshold = (self.hyperlink_pool.len().saturating_mul(2)).max(256);
        self.osc8_inserts_since_gc = 0;
        #[cfg(feature = "metrics")]
        self.metrics.record_hyperlink_gc();
    }

    #[expect(clippy::cast_possible_truncation, reason = "viewport fits in u16")]
    const fn live_cursor_snapshot(&self, display_offset: usize) -> CursorSnapshot {
        CursorSnapshot {
            row: self.cursor.row as u16,
            col: self.cursor.col as u16,
            visible: self.modes.cursor_visible && display_offset == 0,
            style: self.cursor.style,
        }
    }

    const fn active_grid(&self) -> &Grid {
        if self.modes.alt_screen {
            &self.alternate
        } else {
            &self.primary
        }
    }

    const fn active_grid_mut(&mut self) -> &mut Grid {
        if self.modes.alt_screen {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    fn viewport_to_grid_point(&self, screen_y: usize, col: usize) -> GridPoint {
        let grid = self.grid();
        let clamped_y = screen_y.min(grid.viewport_rows().saturating_sub(1));
        let clamped_col = col.min(grid.cols().saturating_sub(1));
        GridPoint::new(grid.screen_to_abs(clamped_y), clamped_col)
    }

    fn enter_sync_mode(&mut self) {
        if self.modes.sync_output {
            return;
        }
        let (cols, rows) = self.size();
        let display_offset = self.display_offset();
        let cursor = self.live_cursor_snapshot(display_offset);
        let top_abs_line;
        let scrollback_len;
        let damage_epoch;
        {
            let grid = if self.modes.alt_screen {
                &self.alternate
            } else {
                &self.primary
            };
            grid.clone_visible_snapshot_into(
                &mut self.latched_frame.visible_rows,
                &mut self.latched_frame.extras,
            );
            top_abs_line = grid.screen_to_abs(0);
            scrollback_len = grid.history_len();
            damage_epoch = grid.damage_epoch();
        }
        self.latched_frame.active = true;
        self.latched_frame.cols = cols;
        self.latched_frame.rows = rows;
        self.latched_frame.cursor = cursor;
        self.latched_frame.modes = self.modes;
        self.latched_frame.palette.clone_from(&self.palette);
        self.latched_frame.palette_overrides = self.palette_overrides;
        self.latched_frame.extended_palette = self.extended_palette;
        self.latched_frame.special_colors = self.special_colors.as_array();
        self.latched_frame.palette_epoch = self.palette_epoch;
        self.latched_frame.selection = self.selection;
        self.latched_frame.display_offset = display_offset;
        self.latched_frame.scrollback_len = scrollback_len;
        self.latched_frame.is_alt_screen = self.modes.alt_screen;
        self.latched_frame.damage_epoch = damage_epoch;
        self.latched_frame.top_abs_line = top_abs_line;
        #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
        {
            let screen = if self.modes.alt_screen {
                ScreenKind::Alternate
            } else {
                ScreenKind::Primary
            };
            self.latched_frame.visible_images =
                self.image_store
                    .visible_placements(screen, top_abs_line, rows);
        }
        self.modes.sync_output = true;
        self.sync_started_at = self.last_time_secs;
    }

    fn exit_sync_mode(&mut self) {
        if !self.modes.sync_output && !self.latched_frame.active {
            return;
        }
        self.modes.sync_output = false;
        self.sync_started_at = None;
        self.latched_frame.reset();
        self.active_grid_mut().bump_damage_epoch();
        self.active_grid_mut().mark_all_dirty();
    }

    fn save_cursor(&mut self) {
        let saved = SavedCursor {
            row: self.cursor.row,
            col: self.cursor.col,
            attrs: self.cursor.attrs,
            origin_mode: self.modes.origin_mode,
            pending_wrap: self.cursor.pending_wrap,
            charsets: self.cursor.charsets,
        };
        if self.modes.alt_screen {
            self.alt_saved_cursor = Some(saved);
        } else {
            self.primary_saved_cursor = Some(saved);
        }
    }

    fn restore_cursor(&mut self) {
        let saved = if self.modes.alt_screen {
            self.alt_saved_cursor
        } else {
            self.primary_saved_cursor
        };
        let rows = self.active_grid().viewport_rows();
        let cols = self.active_grid().cols();

        if let Some(s) = saved {
            self.modes.origin_mode = s.origin_mode;
            self.cursor.attrs = s.attrs;
            self.cursor.charsets = s.charsets;
            self.cursor.row = if self.modes.origin_mode {
                s.row.clamp(self.scroll_top, self.scroll_bottom)
            } else {
                s.row.min(rows - 1)
            };
            self.cursor.col = if self.modes.origin_mode {
                s.col.clamp(self.scroll_left, self.scroll_right)
            } else {
                s.col.min(cols - 1)
            };
            self.cursor.pending_wrap = s.pending_wrap && self.cursor.col == cols - 1;
        } else {
            self.cursor.attrs.reset_all();
            self.cursor.charsets = CharsetSlots::default();
            self.cursor.row = if self.modes.origin_mode {
                self.scroll_top
            } else {
                0
            };
            self.cursor.col = if self.modes.origin_mode {
                self.scroll_left
            } else {
                0
            };
            self.cursor.pending_wrap = false;
        }
        self.last_written = None;
    }

    fn enter_alt_screen(&mut self, clear_alt: bool, save_primary: bool) {
        if self.modes.alt_screen {
            return;
        }
        if save_primary {
            self.save_cursor();
        }
        self.alt_cursor_backup = Some(self.cursor);
        self.modes.alt_screen = true;
        self.selection = None;
        if clear_alt {
            self.alternate.clear_visible(PackedColor::Default);
            #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
            self.image_store.clear_screen(ScreenKind::Alternate);
            #[cfg(feature = "kitty-keyboard")]
            self.kitty_keyboard.reset_alt();
            self.cursor.row = 0;
            self.cursor.col = 0;
            self.cursor.pending_wrap = false;
        }
        self.scroll_top = 0;
        self.scroll_bottom = self.alternate.viewport_rows() - 1;
        self.scroll_left = 0;
        self.scroll_right = self.alternate.cols() - 1;
        self.last_written = None;
        self.last_graphic = None;
    }

    fn exit_alt_screen(&mut self, clear_alt: bool, restore_primary: bool) {
        if !self.modes.alt_screen {
            return;
        }
        if clear_alt {
            self.alternate.clear_visible(PackedColor::Default);
            #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
            self.image_store.clear_screen(ScreenKind::Alternate);
        }
        self.modes.alt_screen = false;
        self.selection = None;
        let rows = self.primary.viewport_rows();
        let cols = self.primary.cols();
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.scroll_left = 0;
        self.scroll_right = cols - 1;

        if let Some(backup) = self.alt_cursor_backup.take() {
            self.cursor = backup;
            self.cursor.row = self.cursor.row.min(rows - 1);
            self.cursor.col = self.cursor.col.min(cols - 1);
            self.cursor.pending_wrap = self.cursor.pending_wrap && self.cursor.col == cols - 1;
        }
        if restore_primary {
            self.restore_cursor();
        }
        self.primary.mark_all_dirty();
        self.last_written = None;
        self.last_graphic = None;
    }

    fn has_horizontal_margins(&self) -> bool {
        let cols = self.active_grid().cols();
        self.scroll_left > 0 || self.scroll_right + 1 < cols
    }

    fn sync_images_after_scroll_up(
        &mut self,
        top: usize,
        bottom: usize,
        count: usize,
        push_to_history: bool,
    ) {
        #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
        {
            if bottom < top {
                return;
            }
            let rows = self.active_grid().viewport_rows();
            let is_history_scroll =
                push_to_history && !self.modes.alt_screen && top == 0 && bottom + 1 == rows;
            if is_history_scroll {
                self.image_store
                    .prune_before(ScreenKind::Primary, self.primary.oldest_abs_line());
            } else {
                let len = bottom - top + 1;
                let shift = count.min(len);
                if shift > 0 {
                    let screen = if self.modes.alt_screen {
                        ScreenKind::Alternate
                    } else {
                        ScreenKind::Primary
                    };
                    let top_abs = self.active_grid().live_viewport_to_abs(top);
                    let bottom_abs = self.active_grid().live_viewport_to_abs(bottom);
                    let delta = -isize::try_from(shift).unwrap_or(isize::MAX);
                    self.image_store
                        .shift_placements_in_region(screen, top_abs, bottom_abs, delta, true);
                }
            }
        }
        #[cfg(not(any(feature = "kitty-graphics", feature = "sixel")))]
        let _unused = (top, bottom, count, push_to_history);
    }

    fn sync_images_after_scroll_down(&mut self, top: usize, bottom: usize, count: usize) {
        #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
        {
            if bottom < top {
                return;
            }
            let len = bottom - top + 1;
            let shift = count.min(len);
            if shift > 0 {
                let screen = if self.modes.alt_screen {
                    ScreenKind::Alternate
                } else {
                    ScreenKind::Primary
                };
                let top_abs = self.active_grid().live_viewport_to_abs(top);
                let bottom_abs = self.active_grid().live_viewport_to_abs(bottom);
                let delta = isize::try_from(shift).unwrap_or(isize::MAX);
                self.image_store
                    .shift_placements_in_region(screen, top_abs, bottom_abs, delta, true);
            }
        }
        #[cfg(not(any(feature = "kitty-graphics", feature = "sixel")))]
        let _unused = (top, bottom, count);
    }

    fn clear_active_viewport_placements(&mut self) {
        #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
        {
            let screen = if self.modes.alt_screen {
                ScreenKind::Alternate
            } else {
                ScreenKind::Primary
            };
            let top_abs = self.active_grid().live_viewport_to_abs(0);
            let vp_rows = u16::try_from(self.active_grid().viewport_rows()).unwrap_or(u16::MAX);
            self.image_store
                .clear_viewport_placements(screen, top_abs, vp_rows, true);
        }
    }

    fn notify_shell_rows_mutated(&mut self, start_row: usize, end_row: usize) {
        #[cfg(feature = "shell-integration")]
        self.shell_integration
            .on_primary_rows_mutated(&self.primary, start_row, end_row);
        #[cfg(not(feature = "shell-integration"))]
        let _unused = (start_row, end_row);
    }

    fn scroll_up_in_margins(
        &mut self,
        top: usize,
        bottom: usize,
        count: usize,
        push_to_history: bool,
    ) {
        let bg = self.cursor.attrs.bg;
        if self.has_horizontal_margins() {
            let left = self.scroll_left;
            let right = self.scroll_right;
            self.active_grid_mut()
                .scroll_up_box(top, bottom, left, right, count, bg);
            #[cfg(feature = "shell-integration")]
            if !self.modes.alt_screen {
                self.shell_integration
                    .on_primary_rows_mutated(&self.primary, top, bottom);
            }
        } else {
            #[cfg(feature = "shell-integration")]
            let rows = self.active_grid().viewport_rows();
            self.active_grid_mut()
                .scroll_up_region(top, bottom, count, bg, push_to_history);
            self.sync_images_after_scroll_up(top, bottom, count, push_to_history);
            #[cfg(feature = "shell-integration")]
            if !self.modes.alt_screen {
                if push_to_history {
                    self.shell_integration
                        .on_primary_scroll_up(&self.primary, top, bottom, rows);
                } else {
                    self.shell_integration
                        .on_primary_rows_mutated(&self.primary, top, bottom);
                }
            }
            if push_to_history {
                self.shift_selection_after_scroll_if_full(top, bottom);
            }
        }
    }

    fn scroll_down_in_margins(&mut self, top: usize, bottom: usize, count: usize) {
        let bg = self.cursor.attrs.bg;
        if self.has_horizontal_margins() {
            let left = self.scroll_left;
            let right = self.scroll_right;
            self.active_grid_mut()
                .scroll_down_box(top, bottom, left, right, count, bg);
        } else {
            self.active_grid_mut()
                .scroll_down_region(top, bottom, count, bg);
            self.sync_images_after_scroll_down(top, bottom, count);
        }
        #[cfg(feature = "shell-integration")]
        if !self.modes.alt_screen {
            self.shell_integration
                .on_primary_rows_mutated(&self.primary, top, bottom);
        }
    }

    fn linefeed_internal(&mut self, apply_lnm: bool) {
        self.cursor.pending_wrap = false;
        if apply_lnm && self.modes.linefeed_newline {
            self.cursor.col = if self.cursor.col >= self.scroll_left {
                self.scroll_left
            } else {
                0
            };
        }
        let rows = self.active_grid().viewport_rows();
        if self.cursor.row == self.scroll_bottom {
            let top = self.scroll_top;
            let bottom = self.scroll_bottom;
            self.scroll_up_in_margins(top, bottom, 1, true);
        } else if self.cursor.row + 1 < rows {
            self.cursor.row += 1;
        }
        self.last_written = None;
    }

    fn shift_selection_after_scroll_if_full(&mut self, top: usize, bottom: usize) {
        let vrows = self.active_grid().viewport_rows();
        if top != 0 || bottom != vrows - 1 || self.modes.alt_screen {
            return;
        }
        let oldest_line = self.primary.oldest_abs_line();
        if let Some(sel) = &mut self.selection {
            if sel.anchor.line < oldest_line && sel.head.line < oldest_line {
                self.selection = None;
            } else {
                if sel.origin_end.line < oldest_line {
                    sel.origin_evicted = true;
                    sel.origin_cropped_bottom = false;
                }
                let is_block = sel.mode == SelectionMode::Block;
                let clamp_evicted = |pt: &mut GridPoint, past_eol: &mut bool| {
                    if pt.line < oldest_line {
                        pt.line = oldest_line;
                        *past_eol = false;
                        if !is_block {
                            pt.col = 0;
                        }
                    }
                };
                clamp_evicted(&mut sel.anchor, &mut sel.anchor_past_eol);
                clamp_evicted(&mut sel.head, &mut sel.head_past_eol);
                clamp_evicted(&mut sel.origin_start, &mut sel.origin_start_past_eol);
                clamp_evicted(&mut sel.origin_end, &mut sel.origin_end_past_eol);
            }
        }
    }

    fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_top {
            let top = self.scroll_top;
            let bottom = self.scroll_bottom;
            self.scroll_down_in_margins(top, bottom, 1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
        self.last_written = None;
    }

    fn print_scalar(&mut self, raw_ch: char) {
        let ch = self.cursor.charsets.translate(raw_ch);
        #[cfg(feature = "kitty-graphics")]
        let ch_width = if ch == KITTY_PLACEHOLDER_CHAR {
            1
        } else {
            UnicodeWidthChar::width(ch).unwrap_or(0)
        };
        #[cfg(not(feature = "kitty-graphics"))]
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        #[cfg(feature = "kitty-graphics")]
        let is_kitty_diacritic = diacritic_index(ch).is_some();
        #[cfg(not(feature = "kitty-graphics"))]
        let is_kitty_diacritic = false;
        let is_ri = ('\u{1F1E6}'..='\u{1F1FF}').contains(&ch);
        let is_skin_tone = ('\u{1F3FB}'..='\u{1F3FF}').contains(&ch);

        if let Some(lw) = self.last_written
            && (ch_width == 0
                || is_kitty_diacritic
                || lw.ended_with_zwj
                || is_skin_tone
                || (is_ri && lw.is_unpaired_ri))
        {
            let cols = self.active_grid().cols();
            let rows = self.active_grid().viewport_rows();
            let Some(prev_cell) = self
                .active_grid()
                .viewport_row(lw.row)
                .and_then(|r| r.cells().get(lw.col).copied())
            else {
                if ch_width == 0 {
                    return;
                }
                self.write_new_cell(ch, ch_width, is_ri);
                return;
            };

            #[cfg(feature = "kitty-graphics")]
            if is_kitty_diacritic && prev_cell.codepoint == KITTY_PLACEHOLDER_CHAR {
                let too_many = prev_cell.extra_id != 0
                    && self
                        .active_grid()
                        .cell_extra(prev_cell.extra_id)
                        .is_some_and(|ext| ext.combining.len() >= MAX_COMBINING_PER_CELL);
                if !too_many {
                    self.active_grid_mut()
                        .append_combining_scalar(lw.row, lw.col, ch);
                    if let Some(ref mut lg) = self.last_graphic {
                        lg.combining.push(ch);
                    }
                    self.last_written = Some(LastWrittenCell {
                        row: lw.row,
                        col: lw.col,
                        ended_with_zwj: false,
                        is_unpaired_ri: false,
                    });
                }
                return;
            }
            if is_kitty_diacritic
                && ch_width > 0
                && !lw.ended_with_zwj
                && !is_skin_tone
                && !(is_ri && lw.is_unpaired_ri)
            {
                self.write_new_cell(ch, ch_width, is_ri);
                return;
            }

            let mut too_many_combining = false;
            let mut scratch = std::mem::take(&mut self.grapheme_scratch);
            scratch.clear();
            if prev_cell.extra_id != 0
                && let Some(ext) = self.active_grid().cell_extra(prev_cell.extra_id)
            {
                if ext.combining.len() >= MAX_COMBINING_PER_CELL {
                    too_many_combining = true;
                } else {
                    ext.write_grapheme(prev_cell.codepoint, &mut scratch);
                }
            } else {
                scratch.push(prev_cell.codepoint);
            }
            if too_many_combining {
                self.grapheme_scratch = scratch;
                return;
            }
            scratch.push(ch);

            let is_single_grapheme =
                UnicodeSegmentation::graphemes(scratch.as_str(), true).count() == 1;
            let new_w = if is_single_grapheme {
                UnicodeWidthStr::width(scratch.as_str()).clamp(1, 2)
            } else {
                1
            };
            self.grapheme_scratch = scratch;

            if is_single_grapheme {
                let was_wide = prev_cell.is_wide_lead();
                let can_wrap_wide =
                    self.modes.auto_wrap && (lw.row == self.scroll_bottom || lw.row + 1 < rows);
                if !was_wide && new_w == 2 && cols >= 2 && lw.col == cols - 1 && !can_wrap_wide {
                    return;
                }

                let bg = self.cursor.attrs.bg;
                self.active_grid_mut()
                    .append_combining_scalar(lw.row, lw.col, ch);
                if let Some(ref mut lg) = self.last_graphic {
                    lg.combining.push(ch);
                }

                let mut new_lw_row = lw.row;
                let mut new_lw_col = lw.col;

                if !was_wide && new_w == 2 && cols >= 2 {
                    if lw.col + 1 < cols {
                        self.active_grid_mut()
                            .promote_cell_to_wide(lw.row, lw.col, bg);
                        #[cfg(feature = "shell-integration")]
                        if !self.modes.alt_screen {
                            self.shell_integration
                                .on_primary_row_mutated(&self.primary, lw.row);
                        }
                        if self.cursor.row == lw.row && self.cursor.col == lw.col + 1 {
                            if lw.col + 2 < cols {
                                self.cursor.col = lw.col + 2;
                                self.cursor.pending_wrap = false;
                            } else {
                                self.cursor.col = cols - 1;
                                self.cursor.pending_wrap = self.modes.auto_wrap;
                            }
                        }
                    } else if lw.col == cols - 1 && self.modes.auto_wrap {
                        let target_row = if lw.row == self.scroll_bottom {
                            let top = self.scroll_top;
                            let bottom = self.scroll_bottom;
                            if top == bottom {
                                if let Some((lead, extra)) = self
                                    .active_grid_mut()
                                    .take_cell_as_wrap_spacer(lw.row, lw.col)
                                {
                                    let spacer = Cell::new(
                                        ' ',
                                        lead.fg,
                                        lead.bg,
                                        CellFlags::WIDE_SPACER
                                            | (lead.flags & CellFlags::WRITE_ATTR_MASK),
                                    );
                                    self.active_grid_mut()
                                        .scroll_up_region(top, bottom, 1, bg, true);
                                    self.sync_images_after_scroll_up(top, bottom, 1, true);
                                    #[cfg(feature = "shell-integration")]
                                    if !self.modes.alt_screen {
                                        self.shell_integration.on_primary_scroll_up(
                                            &self.primary,
                                            top,
                                            bottom,
                                            rows,
                                        );
                                    }
                                    self.shift_selection_after_scroll_if_full(top, bottom);
                                    self.active_grid_mut()
                                        .write_wide_cell(lw.row, 0, lead, spacer, extra, bg);
                                    #[cfg(feature = "shell-integration")]
                                    if !self.modes.alt_screen {
                                        self.shell_integration
                                            .on_primary_row_mutated(&self.primary, lw.row);
                                    }
                                }
                                new_lw_row = lw.row;
                                new_lw_col = 0;
                                self.cursor.row = lw.row;
                                if cols == 2 {
                                    self.cursor.col = 1;
                                    self.cursor.pending_wrap = true;
                                } else {
                                    self.cursor.col = 2;
                                    self.cursor.pending_wrap = false;
                                }
                                self.last_written = Some(LastWrittenCell {
                                    row: new_lw_row,
                                    col: new_lw_col,
                                    ended_with_zwj: ch == '\u{200D}',
                                    is_unpaired_ri: false,
                                });
                                return;
                            }
                            self.active_grid_mut()
                                .scroll_up_region(top, bottom, 1, bg, true);
                            self.sync_images_after_scroll_up(top, bottom, 1, true);
                            #[cfg(feature = "shell-integration")]
                            if !self.modes.alt_screen {
                                self.shell_integration.on_primary_scroll_up(
                                    &self.primary,
                                    top,
                                    bottom,
                                    rows,
                                );
                            }
                            self.shift_selection_after_scroll_if_full(top, bottom);
                            lw.row
                        } else {
                            lw.row + 1
                        };
                        let src_row = if lw.row == self.scroll_bottom {
                            lw.row - 1
                        } else {
                            lw.row
                        };
                        self.active_grid_mut()
                            .move_cell_to_wide_wrap(src_row, lw.col, target_row, bg);
                        #[cfg(feature = "shell-integration")]
                        if !self.modes.alt_screen {
                            self.shell_integration
                                .on_primary_row_mutated(&self.primary, target_row);
                        }
                        new_lw_row = target_row;
                        new_lw_col = 0;
                        self.cursor.row = target_row;
                        if cols == 2 {
                            self.cursor.col = 1;
                            self.cursor.pending_wrap = true;
                        } else {
                            self.cursor.col = 2;
                            self.cursor.pending_wrap = false;
                        }
                    }
                } else if was_wide && new_w == 1 && lw.col + 1 < cols {
                    self.active_grid_mut()
                        .demote_wide_to_narrow(lw.row, lw.col, bg);
                    #[cfg(feature = "shell-integration")]
                    if !self.modes.alt_screen {
                        self.shell_integration
                            .on_primary_row_mutated(&self.primary, lw.row);
                    }
                    if self.cursor.row == lw.row {
                        if self.cursor.col == lw.col + 2 {
                            self.cursor.col = lw.col + 1;
                            self.cursor.pending_wrap = false;
                        } else if self.cursor.col == lw.col + 1 && self.cursor.pending_wrap {
                            self.cursor.pending_wrap = false;
                        }
                    }
                }

                self.last_written = Some(LastWrittenCell {
                    row: new_lw_row,
                    col: new_lw_col,
                    ended_with_zwj: ch == '\u{200D}',
                    is_unpaired_ri: false,
                });
                return;
            }
        }

        if ch_width == 0 {
            return;
        }
        self.write_new_cell(ch, ch_width, is_ri);
    }

    fn write_new_cell(&mut self, ch: char, raw_width: usize, is_ri: bool) {
        let cols = self.active_grid().cols();
        let right_margin = if self.cursor.col <= self.scroll_right {
            self.scroll_right
        } else {
            cols - 1
        };
        let left_margin = self.scroll_left;
        let w = if cols >= 2 && raw_width >= 2 { 2 } else { 1 };
        let bg = self.cursor.attrs.bg;

        if self.cursor.pending_wrap {
            if self.modes.auto_wrap {
                let row = self.cursor.row;
                if right_margin == cols - 1 {
                    self.active_grid_mut().set_row_soft_wrapped(row, true);
                }
                self.linefeed_internal(false);
                self.cursor.col = left_margin;
            }
            self.cursor.pending_wrap = false;
        }

        if w == 2 && self.cursor.col + 1 > right_margin {
            if self.modes.auto_wrap {
                let row = self.cursor.row;
                if right_margin == cols - 1 {
                    self.active_grid_mut().set_wrap_spacer(row, bg);
                }
                self.linefeed_internal(false);
                self.cursor.col = left_margin;
            } else {
                self.cursor.col = right_margin.saturating_sub(1);
            }
        }

        if self.modes.insert_mode {
            let r = self.cursor.row;
            let c = self.cursor.col;
            if right_margin + 1 < cols && c <= right_margin {
                self.active_grid_mut()
                    .insert_cells_in_margin(r, c, right_margin, w, bg);
            } else {
                self.active_grid_mut().insert_cells(r, c, w, bg);
            }
            #[cfg(feature = "shell-integration")]
            if !self.modes.alt_screen {
                self.shell_integration
                    .on_primary_row_mutated(&self.primary, r);
            }
        }

        let extra = if self.cursor.attrs.underline_color.is_some()
            || self.cursor.attrs.hyperlink_id.is_some()
        {
            Some(CellExtra {
                combining: Vec::new(),
                underline_color: self.cursor.attrs.underline_color,
                hyperlink_id: self.cursor.attrs.hyperlink_id,
                text_sizing: None,
            })
        } else {
            None
        };

        let sgr_flags = self.cursor.attrs.flags & CellFlags::WRITE_ATTR_MASK;
        let row = self.cursor.row;
        let col = self.cursor.col;
        if w == 2 && col + 1 < cols {
            let lead = Cell::new(
                ch,
                self.cursor.attrs.fg,
                bg,
                sgr_flags | CellFlags::WIDE_LEAD,
            );
            let spacer = Cell::new(
                ' ',
                self.cursor.attrs.fg,
                bg,
                sgr_flags | CellFlags::WIDE_SPACER,
            );
            self.active_grid_mut()
                .write_wide_cell(row, col, lead, spacer, extra, bg);
        } else {
            let cell = Cell::new(ch, self.cursor.attrs.fg, bg, sgr_flags);
            self.active_grid_mut().write_cell(row, col, cell, extra, bg);
        }
        #[cfg(feature = "shell-integration")]
        if !self.modes.alt_screen {
            self.shell_integration
                .on_primary_row_mutated(&self.primary, row);
        }

        self.last_written = Some(LastWrittenCell {
            row,
            col,
            ended_with_zwj: false,
            is_unpaired_ri: is_ri,
        });
        self.last_graphic = Some(LastGraphic {
            base: ch,
            combining: Vec::new(),
        });

        if col + w <= right_margin {
            self.cursor.col = col + w;
        } else {
            self.cursor.col = right_margin;
            if self.modes.auto_wrap {
                self.cursor.pending_wrap = true;
            }
        }
    }

    fn set_mode_dec(&mut self, mode: u16, enable: bool) {
        match mode {
            1 => self.modes.app_cursor_keys = enable,
            3 => {
                self.modes.column_132 = enable;
                let bg = self.cursor.attrs.bg;
                let rows = self.active_grid().viewport_rows();
                self.active_grid_mut().clear_visible(bg);
                self.clear_active_viewport_placements();
                #[cfg(feature = "shell-integration")]
                if !self.modes.alt_screen && rows > 0 {
                    self.shell_integration
                        .on_primary_rows_mutated(&self.primary, 0, rows - 1);
                }
                self.scroll_top = 0;
                self.scroll_bottom = rows - 1;
                self.scroll_left = 0;
                self.scroll_right = self.active_grid().cols() - 1;
                self.cursor.row = 0;
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            5 if self.modes.reverse_video != enable => {
                self.modes.reverse_video = enable;
                self.active_grid_mut().mark_all_dirty();
            }
            6 => {
                self.modes.origin_mode = enable;
                self.cursor.row = if enable { self.scroll_top } else { 0 };
                self.cursor.col = if enable { self.scroll_left } else { 0 };
                self.cursor.pending_wrap = false;
            }
            7 => {
                self.modes.auto_wrap = enable;
                if !enable {
                    self.cursor.pending_wrap = false;
                }
            }
            9 => {
                self.modes.mouse_protocol = if enable {
                    MouseProtocol::X10
                } else {
                    MouseProtocol::None
                };
            }
            12 => {
                self.modes.cursor_blink = enable;
                self.cursor.style.blink = enable;
            }
            25 => self.modes.cursor_visible = enable,
            80 => self.modes.sixel_display_mode = enable,
            47 => {
                if enable {
                    self.enter_alt_screen(false, false);
                } else {
                    self.exit_alt_screen(false, false);
                }
            }
            69 => {
                self.modes.left_right_margin_mode = enable;
                if !enable {
                    self.scroll_left = 0;
                    self.scroll_right = self.active_grid().cols() - 1;
                }
            }
            1000 => {
                self.modes.mouse_protocol = if enable {
                    MouseProtocol::Normal
                } else {
                    MouseProtocol::None
                };
            }
            1002 => {
                self.modes.mouse_protocol = if enable {
                    MouseProtocol::ButtonEvent
                } else {
                    MouseProtocol::None
                };
            }
            1003 => {
                self.modes.mouse_protocol = if enable {
                    MouseProtocol::AnyEvent
                } else {
                    MouseProtocol::None
                };
            }
            1004 => self.modes.focus_reporting = enable,
            1005 => {
                self.modes.mouse_encoding = if enable {
                    MouseEncoding::Utf8
                } else {
                    MouseEncoding::Default
                };
            }
            1006 => {
                self.modes.mouse_encoding = if enable {
                    MouseEncoding::Sgr
                } else {
                    MouseEncoding::Default
                };
            }
            1007 => self.modes.alt_scroll = enable,
            1016 => {
                self.modes.mouse_encoding = if enable {
                    MouseEncoding::SgrPixels
                } else {
                    MouseEncoding::Default
                };
            }
            1047 => {
                if enable {
                    self.enter_alt_screen(true, false);
                } else {
                    self.exit_alt_screen(true, false);
                }
            }
            1048 => {
                if enable {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            1049 => {
                if enable {
                    self.enter_alt_screen(true, true);
                } else {
                    self.exit_alt_screen(false, true);
                }
            }
            1070 => self.modes.sixel_private_color_registers = enable,
            2004 => self.modes.bracketed_paste = enable,
            2026 => {
                if enable {
                    self.enter_sync_mode();
                } else {
                    self.exit_sync_mode();
                }
            }
            2027 => self.modes.grapheme_clustering = enable,
            2031 => self.modes.color_scheme_updates = enable,
            8452 => self.modes.sixel_cursor_right = enable,
            _ => {}
        }
    }

    fn set_mode_ansi(&mut self, mode: u16, enable: bool) {
        match mode {
            4 => self.modes.insert_mode = enable,
            20 => self.modes.linefeed_newline = enable,
            _ => {}
        }
    }

    fn soft_reset(&mut self) {
        let was_reverse = self.modes.reverse_video;
        self.modes.reset_soft(self.config.default_cursor_blink);
        self.exit_sync_mode();
        if was_reverse {
            self.active_grid_mut().mark_all_dirty();
        }
        let rows = self.active_grid().viewport_rows();
        let cols = self.active_grid().cols();
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.scroll_left = 0;
        self.scroll_right = cols - 1;
        self.cursor.attrs.reset_all();
        self.cursor.charsets = CharsetSlots::default();
        self.sgr_stack.clear();
        self.cursor.style = CursorStyle {
            shape: self.config.default_cursor_shape,
            blink: self.config.default_cursor_blink,
        };
        self.cursor.pending_wrap = false;
        self.primary_saved_cursor = None;
        self.alt_saved_cursor = None;
        #[cfg(feature = "sixel")]
        self.sixel_shared_palette.clear();
        self.last_written = None;
        self.last_graphic = None;
    }

    fn resolve_rect_1based(
        &self,
        params: &CsiParams,
        base_idx: usize,
        rows: usize,
        cols: usize,
    ) -> Option<(usize, usize, usize, usize)> {
        let (min_r, max_r, min_c, max_c) = if self.modes.origin_mode {
            (
                self.scroll_top,
                self.scroll_bottom,
                self.scroll_left,
                self.scroll_right,
            )
        } else {
            (0, rows.saturating_sub(1), 0, cols.saturating_sub(1))
        };
        if rows == 0 || cols == 0 || min_r > max_r || min_c > max_c {
            return None;
        }
        let rel_rows = u16::try_from(max_r - min_r + 1).unwrap_or(u16::MAX);
        let rel_cols = u16::try_from(max_c - min_c + 1).unwrap_or(u16::MAX);
        let pt = usize::from(params.get_nonzero_or(base_idx, 1));
        let pl = usize::from(params.get_nonzero_or(base_idx + 1, 1));
        let pb = usize::from(params.get_nonzero_or(base_idx + 2, rel_rows));
        let pr = usize::from(params.get_nonzero_or(base_idx + 3, rel_cols));
        let top = min_r.saturating_add(pt - 1);
        let left = min_c.saturating_add(pl - 1);
        if top > max_r || left > max_c {
            return None;
        }
        let bottom = min_r.saturating_add(pb - 1).min(max_r);
        let right = min_c.saturating_add(pr - 1).min(max_c);
        if top > bottom || left > right {
            return None;
        }
        Some((top, left, bottom, right))
    }

    fn apply_text_sizing(&mut self, sizing: TextSizing, text: &str) {
        if text.is_empty() {
            return;
        }
        let cols = self.active_grid().cols();
        let rows = self.active_grid().viewport_rows();
        if cols == 0 || rows == 0 {
            return;
        }
        let s = usize::from(sizing.scale.clamp(1, 7));
        let bg = self.cursor.attrs.bg;
        let fg = self.cursor.attrs.fg;
        let write_flags = self.cursor.attrs.flags & CellFlags::WRITE_ATTR_MASK;
        let spacer_flags = CellFlags::SIZED_SPACER | (write_flags & CellFlags::PROTECTED);
        let right_margin = if self.cursor.col <= self.scroll_right {
            self.scroll_right
        } else {
            cols - 1
        };
        let left_margin = self.scroll_left;

        if self.cursor.pending_wrap {
            if self.modes.auto_wrap {
                let row = self.cursor.row;
                if right_margin == cols - 1 {
                    self.active_grid_mut().set_row_soft_wrapped(row, true);
                }
                self.linefeed_internal(false);
                self.cursor.col = left_margin;
            }
            self.cursor.pending_wrap = false;
        }

        if sizing.width > 0 {
            let box_cols = usize::from(sizing.width).saturating_mul(s).max(1);
            if self.modes.auto_wrap
                && self.cursor.col > left_margin
                && self.cursor.col + box_cols > right_margin + 1
                && left_margin + box_cols <= right_margin + 1
            {
                let row = self.cursor.row;
                if right_margin == cols - 1 {
                    self.active_grid_mut().set_row_soft_wrapped(row, true);
                }
                self.linefeed_internal(false);
                self.cursor.col = left_margin;
            }
            let row = self.cursor.row;
            let start_col = self.cursor.col;
            let first_g = UnicodeSegmentation::graphemes(text, true)
                .next()
                .unwrap_or(" ");
            let mut chars = first_g.chars();
            let base_ch = chars.next().unwrap_or(' ');
            let combining: Vec<char> = chars.take(MAX_COMBINING_PER_CELL).collect();
            let extra = Some(CellExtra {
                combining,
                underline_color: self.cursor.attrs.underline_color,
                hyperlink_id: self.cursor.attrs.hyperlink_id,
                text_sizing: Some(sizing),
            });
            self.active_grid_mut().write_cell(
                row,
                start_col,
                Cell::new(base_ch, fg, bg, write_flags),
                extra,
                bg,
            );
            let col_end = (start_col + box_cols).min(right_margin + 1).min(cols);
            let row_end = (row + s).min(rows);
            for r in row..row_end {
                for c in start_col..col_end {
                    if r == row && c == start_col {
                        continue;
                    }
                    self.active_grid_mut().write_cell(
                        r,
                        c,
                        Cell::new(' ', fg, bg, spacer_flags),
                        None,
                        bg,
                    );
                }
            }
            #[cfg(feature = "shell-integration")]
            if !self.modes.alt_screen && row_end > row {
                self.shell_integration
                    .on_primary_rows_mutated(&self.primary, row, row_end - 1);
            }
            let next_col = start_col + box_cols;
            if next_col <= right_margin {
                self.cursor.col = next_col;
                self.cursor.pending_wrap = false;
            } else {
                self.cursor.col = right_margin;
                self.cursor.pending_wrap = self.modes.auto_wrap;
            }
            return;
        }

        for grapheme in UnicodeSegmentation::graphemes(text, true) {
            let gw = UnicodeWidthStr::width(grapheme).clamp(1, 2);
            let glyph_cols = gw.saturating_mul(s).max(1);
            if self.modes.auto_wrap
                && self.cursor.col > left_margin
                && self.cursor.col + glyph_cols > right_margin + 1
                && left_margin + glyph_cols <= right_margin + 1
            {
                let row = self.cursor.row;
                if right_margin == cols - 1 {
                    self.active_grid_mut().set_row_soft_wrapped(row, true);
                }
                self.linefeed_internal(false);
                self.cursor.col = left_margin;
                self.cursor.pending_wrap = false;
            }
            let row = self.cursor.row;
            let start_col = self.cursor.col;
            let mut chars = grapheme.chars();
            let base_ch = chars.next().unwrap_or(' ');
            let combining: Vec<char> = chars.take(MAX_COMBINING_PER_CELL).collect();
            let extra = Some(CellExtra {
                combining,
                underline_color: self.cursor.attrs.underline_color,
                hyperlink_id: self.cursor.attrs.hyperlink_id,
                text_sizing: Some(sizing),
            });
            self.active_grid_mut().write_cell(
                row,
                start_col,
                Cell::new(base_ch, fg, bg, write_flags),
                extra,
                bg,
            );
            let col_end = (start_col + glyph_cols).min(right_margin + 1).min(cols);
            let row_end = (row + s).min(rows);
            for r in row..row_end {
                for c in start_col..col_end {
                    if r == row && c == start_col {
                        continue;
                    }
                    self.active_grid_mut().write_cell(
                        r,
                        c,
                        Cell::new(' ', fg, bg, spacer_flags),
                        None,
                        bg,
                    );
                }
            }
            #[cfg(feature = "shell-integration")]
            if !self.modes.alt_screen && row_end > row {
                self.shell_integration
                    .on_primary_rows_mutated(&self.primary, row, row_end - 1);
            }
            let next_col = start_col + glyph_cols;
            if next_col <= right_margin {
                self.cursor.col = next_col;
                self.cursor.pending_wrap = false;
            } else {
                self.cursor.col = right_margin;
                self.cursor.pending_wrap = self.modes.auto_wrap;
            }
        }
    }

    fn handle_xtwinops(&mut self, params: &CsiParams, cols: usize, rows: usize) {
        const MAX_TITLE_STACK: usize = 16;
        match params.get_or(0, 0) {
            11 => {
                let code = if self.window_iconified { 2 } else { 1 };
                let mut reply = String::with_capacity(8);
                if write!(reply, "\x1b[{code}t").is_err() {
                    // `String`'s `fmt::Write` impl is infallible.
                }
                self.outgoing.extend_from_slice(reply.as_bytes());
            }
            13 => {
                let (x, y) = self.window_pos;
                let mut reply = String::with_capacity(24);
                if write!(reply, "\x1b[3;{x};{y}t").is_err() {
                    // `String`'s `fmt::Write` impl is infallible.
                }
                self.outgoing.extend_from_slice(reply.as_bytes());
            }
            14 => {
                let (cw, ch) = self.effective_cell_pixel_size();
                let (w, h) = if self.pixel_width > 0 && self.pixel_height > 0 {
                    (
                        usize::from(self.pixel_width),
                        usize::from(self.pixel_height),
                    )
                } else {
                    (cols * usize::from(cw), rows * usize::from(ch))
                };
                let mut reply = String::with_capacity(24);
                if write!(reply, "\x1b[4;{h};{w}t").is_err() {
                    // `String`'s `fmt::Write` impl is infallible.
                }
                self.outgoing.extend_from_slice(reply.as_bytes());
            }
            16 => {
                let (cw, ch) = self.effective_cell_pixel_size();
                let mut reply = String::with_capacity(24);
                if write!(reply, "\x1b[6;{ch};{cw}t").is_err() {
                    // `String`'s `fmt::Write` impl is infallible.
                }
                self.outgoing.extend_from_slice(reply.as_bytes());
            }
            18 => {
                let mut reply = String::with_capacity(24);
                if write!(reply, "\x1b[8;{rows};{cols}t").is_err() {
                    // `String`'s `fmt::Write` impl is infallible.
                }
                self.outgoing.extend_from_slice(reply.as_bytes());
            }
            19 => {
                let (sw, sh) = self
                    .screen_char_size
                    .map_or((cols, rows), |(w, h)| (usize::from(w), usize::from(h)));
                let mut reply = String::with_capacity(24);
                if write!(reply, "\x1b[9;{sh};{sw}t").is_err() {
                    // `String`'s `fmt::Write` impl is infallible.
                }
                self.outgoing.extend_from_slice(reply.as_bytes());
            }
            20 => {
                self.outgoing.extend_from_slice(b"\x1b]L");
                for ch in self.icon_name.chars() {
                    if !ch.is_control() {
                        let mut buf = [0u8; 4];
                        let s = ch.encode_utf8(&mut buf);
                        self.outgoing.extend_from_slice(s.as_bytes());
                    }
                }
                self.outgoing.extend_from_slice(b"\x1b\\");
            }
            21 => {
                self.outgoing.extend_from_slice(b"\x1b]l");
                for ch in self.title.chars() {
                    if !ch.is_control() {
                        let mut buf = [0u8; 4];
                        let s = ch.encode_utf8(&mut buf);
                        self.outgoing.extend_from_slice(s.as_bytes());
                    }
                }
                self.outgoing.extend_from_slice(b"\x1b\\");
            }
            22 => {
                let which = params.get_or(1, 0);
                let icon = if matches!(which, 0 | 1) {
                    Some(self.icon_name.clone())
                } else {
                    None
                };
                let title = if matches!(which, 0 | 2) {
                    Some(self.title.clone())
                } else {
                    None
                };
                if icon.is_some() || title.is_some() {
                    if self.title_stack.len() >= MAX_TITLE_STACK {
                        self.title_stack.remove(0);
                    }
                    self.title_stack.push((icon, title));
                }
            }
            23 => {
                let which = params.get_or(1, 0);
                if let Some((icon, title)) = self.title_stack.pop() {
                    if matches!(which, 0 | 1)
                        && let Some(prev_icon) = icon
                        && self.icon_name != prev_icon
                    {
                        self.icon_name.clone_from(&prev_icon);
                        self.events.push(TerminalEvent::IconNameChanged(prev_icon));
                    }
                    if matches!(which, 0 | 2)
                        && let Some(prev_title) = title
                        && self.title != prev_title
                    {
                        self.title.clone_from(&prev_title);
                        self.events.push(TerminalEvent::TitleChanged(prev_title));
                    }
                }
            }
            _ => {}
        }
    }

    fn full_reset(&mut self) {
        self.exit_sync_mode();
        self.modes = Modes::with_default_blink(self.config.default_cursor_blink);
        self.primary.clear_all();
        self.alternate.clear_all();
        let cols = self.primary.cols();
        let rows = self.primary.viewport_rows();
        self.tabs.reset(cols);
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.scroll_left = 0;
        self.scroll_right = cols - 1;
        self.cursor = Cursor {
            style: CursorStyle {
                shape: self.config.default_cursor_shape,
                blink: self.config.default_cursor_blink,
            },
            ..Cursor::default()
        };
        self.alt_cursor_backup = None;
        self.primary_saved_cursor = None;
        self.alt_saved_cursor = None;
        self.sgr_stack.clear();
        self.palette = self.config.theme.scheme.clone();
        self.palette_overrides = PaletteOverrides::default();
        self.extended_palette.fill(None);
        if self.special_colors != SpecialColors::default() {
            self.special_colors = SpecialColors::default();
        }
        self.palette_epoch = self.palette_epoch.saturating_add(1);
        self.title.clear();
        self.icon_name.clear();
        self.title_stack.clear();
        self.mouse_shape_stack.clear();
        self.osc99_pending.clear();
        self.cwd = None;
        self.document_uri = None;
        self.hyperlink_pool.clear();
        #[cfg(feature = "hyperlinks")]
        {
            self.primary_implicit_links.clear();
            self.alt_implicit_links.clear();
            self.osc8_inserts_since_gc = 0;
        }
        #[cfg(feature = "shell-integration")]
        self.shell_integration.reset();
        #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
        self.image_store.clear_all();
        #[cfg(feature = "sixel")]
        {
            self.active_sixel = None;
            self.sixel_shared_palette.clear();
        }
        #[cfg(feature = "kitty-keyboard")]
        self.kitty_keyboard.reset_all();
        self.selection = None;
        self.last_written = None;
        self.last_graphic = None;
    }
}

fn parse_charset(code: u8) -> Option<Charset> {
    match code {
        b'B' => Some(Charset::Ascii),
        b'0' => Some(Charset::DecSpecialGraphics),
        b'A' => Some(Charset::Uk),
        _ => None,
    }
}

impl VtHandler for TerminalState {
    fn print(&mut self, ch: char) {
        self.print_scalar(ch);
    }

    fn print_ascii_run(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.modes.insert_mode
            || self.cursor.charsets.active() != Charset::Ascii
            || self.cursor.charsets.has_single_shift()
            || self.cursor.attrs.underline_color.is_some()
            || self.cursor.attrs.hyperlink_id.is_some()
            || self.has_horizontal_margins()
        {
            for &b in bytes {
                self.print_scalar(char::from(b));
            }
            return;
        }

        let cols = self.active_grid().cols();
        let fg = self.cursor.attrs.fg;
        let bg = self.cursor.attrs.bg;
        let flags = self.cursor.attrs.flags & CellFlags::WRITE_ATTR_MASK;
        let mut rem = bytes;

        while !rem.is_empty() {
            if self.cursor.pending_wrap {
                if self.modes.auto_wrap {
                    let row = self.cursor.row;
                    self.active_grid_mut().set_row_soft_wrapped(row, true);
                    self.linefeed_internal(false);
                    self.cursor.col = 0;
                    self.cursor.pending_wrap = false;
                } else {
                    // Overwrite last column repeatedly when DECAWM is disabled.
                    self.cursor.pending_wrap = false;
                }
            }

            if !self.modes.auto_wrap && self.cursor.col == cols - 1 && rem.len() > 1 {
                let last_byte = rem[rem.len() - 1];
                let row = self.cursor.row;
                #[cfg(feature = "shell-integration")]
                if !self.modes.alt_screen
                    && self.shell_integration.row_has_live_eol(&self.primary, row)
                    && fg.is_default()
                    && bg.is_default()
                    && flags.is_empty()
                    && rem[..rem.len() - 1].contains(&b' ')
                {
                    let blank = Cell::new(' ', fg, bg, flags);
                    self.primary.write_cell(row, cols - 1, blank, None, bg);
                    self.shell_integration
                        .on_primary_row_mutated(&self.primary, row);
                }
                self.active_grid_mut().write_ascii_slice(
                    row,
                    cols - 1,
                    &[last_byte],
                    fg,
                    bg,
                    flags,
                );
                #[cfg(feature = "shell-integration")]
                if !self.modes.alt_screen {
                    self.shell_integration
                        .on_primary_row_mutated(&self.primary, row);
                }
                self.last_written = Some(LastWrittenCell {
                    row,
                    col: cols - 1,
                    ended_with_zwj: false,
                    is_unpaired_ri: false,
                });
                break;
            }

            let avail = cols - self.cursor.col;
            let take = rem.len().min(avail);
            let row = self.cursor.row;
            let start_col = self.cursor.col;
            #[cfg(feature = "shell-integration")]
            if !self.modes.alt_screen && self.shell_integration.row_has_live_eol(&self.primary, row)
            {
                let has_default_space = fg.is_default()
                    && bg.is_default()
                    && flags.is_empty()
                    && rem[..take].contains(&b' ');
                let hits_wide = self.primary.viewport_row(row).is_some_and(|r| {
                    r.cells()[start_col..start_col + take]
                        .iter()
                        .any(|c| c.is_wide_lead() || c.is_wide_spacer())
                });
                if has_default_space || hits_wide {
                    for &b in &rem[..take] {
                        self.print_scalar(char::from(b));
                    }
                    rem = &rem[take..];
                    continue;
                }
            }
            self.active_grid_mut()
                .write_ascii_slice(row, start_col, &rem[..take], fg, bg, flags);
            #[cfg(feature = "shell-integration")]
            if !self.modes.alt_screen {
                self.shell_integration
                    .on_primary_row_mutated(&self.primary, row);
            }
            let written_col = start_col + take - 1;
            self.last_written = Some(LastWrittenCell {
                row,
                col: written_col,
                ended_with_zwj: false,
                is_unpaired_ri: false,
            });

            if start_col + take < cols {
                self.cursor.col = start_col + take;
            } else {
                self.cursor.col = cols - 1;
                if self.modes.auto_wrap {
                    self.cursor.pending_wrap = true;
                }
            }

            rem = &rem[take..];
        }

        if let Some(&last_b) = bytes.last() {
            self.last_graphic = Some(LastGraphic {
                base: char::from(last_b),
                combining: Vec::new(),
            });
        }
    }

    fn print_utf8_run(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.modes.insert_mode
            || self.cursor.charsets.active() != Charset::Ascii
            || self.cursor.charsets.has_single_shift()
            || self.cursor.attrs.underline_color.is_some()
            || self.cursor.attrs.hyperlink_id.is_some()
            || self.has_horizontal_margins()
        {
            for ch in text.chars() {
                self.print_scalar(ch);
            }
            return;
        }

        let cols = self.active_grid().cols();
        let fg = self.cursor.attrs.fg;
        let bg = self.cursor.attrs.bg;
        let write_flags = self.cursor.attrs.flags & CellFlags::WRITE_ATTR_MASK;
        let mut last_scalar_ch: Option<char> = None;

        for ch in text.chars() {
            #[cfg(feature = "kitty-graphics")]
            let ch_width = if ch == KITTY_PLACEHOLDER_CHAR {
                1
            } else {
                UnicodeWidthChar::width(ch).unwrap_or(0)
            };
            #[cfg(not(feature = "kitty-graphics"))]
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            #[cfg(feature = "kitty-graphics")]
            let is_kitty_diacritic = diacritic_index(ch).is_some();
            #[cfg(not(feature = "kitty-graphics"))]
            let is_kitty_diacritic = false;
            let is_ri = ('\u{1F1E6}'..='\u{1F1FF}').contains(&ch);
            let is_skin_tone = ('\u{1F3FB}'..='\u{1F3FF}').contains(&ch);
            let needs_scalar = ch_width == 0
                || is_kitty_diacritic
                || is_ri
                || is_skin_tone
                || self.last_written.is_some_and(|lw| lw.ended_with_zwj);
            if needs_scalar {
                self.print_scalar(ch);
                last_scalar_ch = None;
                continue;
            }

            let w = if cols >= 2 && ch_width >= 2 { 2 } else { 1 };
            if self.cursor.pending_wrap {
                if self.modes.auto_wrap {
                    let row = self.cursor.row;
                    self.active_grid_mut().set_row_soft_wrapped(row, true);
                    self.linefeed_internal(false);
                    self.cursor.col = 0;
                }
                self.cursor.pending_wrap = false;
            }

            if w == 2 && self.cursor.col + 1 >= cols {
                if self.modes.auto_wrap {
                    let row = self.cursor.row;
                    self.active_grid_mut().set_wrap_spacer(row, bg);
                    self.linefeed_internal(false);
                    self.cursor.col = 0;
                } else {
                    self.cursor.col = cols.saturating_sub(2);
                }
            }

            let row = self.cursor.row;
            let col = self.cursor.col;
            #[cfg(feature = "shell-integration")]
            if !self.modes.alt_screen && self.shell_integration.row_has_live_eol(&self.primary, row)
            {
                self.print_scalar(ch);
                last_scalar_ch = None;
                continue;
            }

            if w == 2 && col + 1 < cols {
                let lead = Cell::new(ch, fg, bg, write_flags | CellFlags::WIDE_LEAD);
                let spacer = Cell::new(' ', fg, bg, write_flags | CellFlags::WIDE_SPACER);
                self.active_grid_mut()
                    .write_wide_cell(row, col, lead, spacer, None, bg);
            } else {
                let cell = Cell::new(ch, fg, bg, write_flags);
                self.active_grid_mut().write_cell(row, col, cell, None, bg);
            }
            #[cfg(feature = "shell-integration")]
            if !self.modes.alt_screen {
                self.shell_integration
                    .on_primary_row_mutated(&self.primary, row);
            }

            self.last_written = Some(LastWrittenCell {
                row,
                col,
                ended_with_zwj: false,
                is_unpaired_ri: false,
            });
            last_scalar_ch = Some(ch);

            if col + w < cols {
                self.cursor.col = col + w;
            } else {
                self.cursor.col = cols - 1;
                if self.modes.auto_wrap {
                    self.cursor.pending_wrap = true;
                }
            }
        }

        if let Some(ch) = last_scalar_ch {
            self.last_graphic = Some(LastGraphic {
                base: ch,
                combining: Vec::new(),
            });
        }
    }

    fn execute(&mut self, byte: u8) {
        if !matches!(byte, 0x00 | 0x7F | 0x8E | 0x8F) {
            self.cursor.charsets.clear_single_shift();
        }
        self.last_written = None;
        self.last_graphic = None;
        match byte {
            0x07 => self.events.push(TerminalEvent::Bell),
            0x08 => {
                self.cursor.pending_wrap = false;
                let min_col = if self.cursor.col >= self.scroll_left {
                    self.scroll_left
                } else {
                    0
                };
                self.cursor.col = self.cursor.col.saturating_sub(1).max(min_col);
            }
            0x09 => {
                self.cursor.pending_wrap = false;
                let max_col = if self.cursor.col <= self.scroll_right {
                    self.scroll_right
                } else {
                    self.active_grid().cols().saturating_sub(1)
                };
                self.cursor.col = self.tabs.next(self.cursor.col, 1).min(max_col);
            }
            0x0A..=0x0C => self.linefeed_internal(true),
            0x0D => {
                self.cursor.pending_wrap = false;
                self.cursor.col = if self.cursor.col >= self.scroll_left {
                    self.scroll_left
                } else {
                    0
                };
            }
            0x0E => self.cursor.charsets.set_gl(1),
            0x0F => self.cursor.charsets.set_gl(0),
            0x84 => self.linefeed_internal(false),
            0x85 => {
                self.cursor.pending_wrap = false;
                self.cursor.col = if self.cursor.col >= self.scroll_left {
                    self.scroll_left
                } else {
                    0
                };
                self.linefeed_internal(false);
            }
            0x88 => self.tabs.set(self.cursor.col),
            0x8D => self.reverse_index(),
            0x8E => self.cursor.charsets.set_single_shift(2),
            0x8F => self.cursor.charsets.set_single_shift(3),
            _ => {}
        }
    }

    fn hook(&mut self, params: &CsiParams, intermediates: &[u8], action: u8) {
        self.cursor.charsets.clear_single_shift();
        self.last_written = None;
        self.last_graphic = None;
        #[cfg(feature = "sixel")]
        if action == b'q' && intermediates.is_empty() {
            let decoder = if self.modes.sixel_private_color_registers {
                SixelDecoder::new(params)
            } else {
                let palette = std::mem::take(&mut self.sixel_shared_palette);
                SixelDecoder::with_shared_palette(params, palette)
            };
            self.active_sixel = Some(decoder);
            return;
        }
        self.dcs_state.start(params, intermediates, action);
    }

    fn put(&mut self, byte: u8) {
        #[cfg(feature = "sixel")]
        if let Some(dec) = &mut self.active_sixel {
            dec.put(byte);
            return;
        }
        self.dcs_state.push(byte);
    }

    fn put_slice(&mut self, bytes: &[u8]) {
        #[cfg(feature = "sixel")]
        if let Some(dec) = &mut self.active_sixel {
            for &b in bytes {
                dec.put(b);
            }
            return;
        }
        self.dcs_state.push_slice(bytes);
    }

    fn unhook(&mut self) {
        #[cfg(feature = "sixel")]
        if let Some(dec) = self.active_sixel.take() {
            match dec.finish_sixel_or_palette() {
                Ok(img) => {
                    if !self.modes.sixel_private_color_registers {
                        self.sixel_shared_palette = img.palette;
                    }
                    let screen = if self.modes.alt_screen {
                        ScreenKind::Alternate
                    } else {
                        ScreenKind::Primary
                    };
                    let id = self.image_store.allocate_id();
                    let w = img.width;
                    let h = img.height;
                    let pan = u64::from(img.pan.max(1));
                    let pad = u64::from(img.pad.max(1));
                    let stored = StoredImage {
                        id,
                        width: w,
                        height: h,
                        format: crate::features::inline_images::ImageFormat::Rgba8,
                        rgba: img.rgba,
                        revision: 0,
                    };
                    if self.image_store.insert_image(stored) {
                        let (place_row, place_col) = if self.modes.sixel_display_mode {
                            (0, 0)
                        } else {
                            (self.cursor.row, self.cursor.col)
                        };
                        let abs_line = self.grid().live_viewport_to_abs(place_row);
                        let (cw, ch) = self.effective_cell_pixel_size();
                        let cols =
                            u16::try_from(w.div_ceil(u32::from(cw.max(1)))).unwrap_or(u16::MAX);
                        let rows = u16::try_from(
                            (u64::from(h) * pan).div_ceil(pad * u64::from(ch.max(1))),
                        )
                        .unwrap_or(u16::MAX);
                        self.image_store.add_placement(
                            screen,
                            ImagePlacement {
                                image_id: id,
                                placement_id: 0,
                                abs_line,
                                col: u16::try_from(place_col).unwrap_or(u16::MAX),
                                cols,
                                rows,
                                src_x: 0,
                                src_y: 0,
                                src_w: 0,
                                src_h: 0,
                                z_index: 0,
                                is_virtual: false,
                                sub_x: 0,
                                sub_y: 0,
                                parent_image_id: 0,
                                parent_placement_id: 0,
                                offset_cols: 0,
                                offset_rows: 0,
                            },
                        );
                        if !self.modes.sixel_display_mode {
                            if self.modes.sixel_cursor_right {
                                for _ in 0..rows.saturating_sub(1) {
                                    self.linefeed_internal(false);
                                }
                                let grid_cols = self.active_grid().cols();
                                self.cursor.col = place_col
                                    .saturating_add(usize::from(cols))
                                    .min(grid_cols.saturating_sub(1));
                                self.cursor.pending_wrap = false;
                            } else {
                                for _ in 0..rows {
                                    self.linefeed_internal(false);
                                }
                                self.cursor.col = 0;
                                self.cursor.pending_wrap = false;
                            }
                        }
                        self.active_grid_mut().mark_all_dirty();
                    }
                }
                Err(palette) => {
                    if !self.modes.sixel_private_color_registers {
                        self.sixel_shared_palette = palette;
                    }
                }
            }
            return;
        }
        let Some((kind, payload)) = self.dcs_state.take() else {
            return;
        };
        match kind {
            DcsKind::Xtgettcap => dcs::handle_xtgettcap(&payload, &mut self.outgoing),
            DcsKind::Decrqss => {
                let state = DecrqssState {
                    attrs: &self.cursor.attrs,
                    scroll_top: self.scroll_top,
                    scroll_bottom: self.scroll_bottom,
                    scroll_left: self.scroll_left,
                    scroll_right: self.scroll_right,
                    left_right_margin_mode: self.modes.left_right_margin_mode,
                    cursor_style: self.cursor.style,
                };
                dcs::handle_decrqss(&payload, &state, &mut self.outgoing);
            }
        }
    }

    fn apc_dispatch(&mut self, data: &[u8]) {
        self.cursor.charsets.clear_single_shift();
        self.last_written = None;
        self.last_graphic = None;
        #[cfg(feature = "kitty-graphics")]
        {
            let screen = if self.modes.alt_screen {
                ScreenKind::Alternate
            } else {
                ScreenKind::Primary
            };
            let grid = self.grid();
            let viewport_top_abs_line = grid.live_viewport_to_abs(0);
            let viewport_rows = u16::try_from(grid.viewport_rows()).unwrap_or(u16::MAX);
            let cursor_abs_line = grid.live_viewport_to_abs(self.cursor.row);
            let cursor_col = u16::try_from(self.cursor.col).unwrap_or(u16::MAX);
            let cell_px = self.effective_cell_pixel_size();
            let policy = KittyTransportPolicy {
                file_policy: self.config.kitty_file_medium_policy,
                shm_reader: self
                    .kitty_shm_reader
                    .0
                    .as_deref()
                    .map(|r| r as &dyn KittyShmReader),
                ..KittyTransportPolicy::default()
            };
            let outcome = handle_kitty_apc_with_context(
                &mut self.image_store,
                data,
                KittyApcContext {
                    screen,
                    viewport_top_abs_line,
                    viewport_rows,
                    cursor_abs_line,
                    cursor_col,
                    cell_px,
                    policy,
                },
                &mut self.outgoing,
            );
            if outcome.move_cursor {
                let grid_cols = self.active_grid().cols();
                let new_col = self
                    .cursor
                    .col
                    .saturating_add(usize::from(outcome.advance_cols));
                for _ in 0..outcome.advance_rows.saturating_sub(1) {
                    self.linefeed_internal(false);
                }
                if new_col >= grid_cols {
                    self.linefeed_internal(false);
                    self.cursor.col = 0;
                } else {
                    self.cursor.col = new_col;
                }
                self.cursor.pending_wrap = false;
            }
            if outcome.modified {
                self.active_grid_mut().mark_all_dirty();
            }
        }
        #[cfg(not(feature = "kitty-graphics"))]
        let _unused_data = data;
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], terminator: OscTerminator) {
        #[cfg(feature = "metrics")]
        self.metrics.record_osc();
        self.cursor.charsets.clear_single_shift();
        self.last_written = None;
        self.last_graphic = None;
        if params.first().copied() == Some(b"133") {
            #[cfg(feature = "shell-integration")]
            if !self.modes.alt_screen {
                self.shell_integration.handle_osc_133(
                    params,
                    &self.primary,
                    self.cursor.row,
                    self.cursor.col,
                    self.cursor.pending_wrap,
                    &mut self.events,
                );
            }
            return;
        }
        if params.first().copied() == Some(b"66") {
            if let Some((sizing, text)) = osc::parse_osc66(params) {
                self.apply_text_sizing(sizing, &text);
            }
            return;
        }
        let mut palette_changed = false;
        #[cfg(feature = "hyperlinks")]
        let mut hyperlink_inserted = false;
        let osc52_read_policy = self.osc52_read_policy();
        let mut ctx = OscContext {
            palette: &mut self.palette,
            default_palette: &self.config.theme.scheme,
            palette_overrides: &mut self.palette_overrides,
            extended_palette: &mut self.extended_palette,
            special_colors: &mut self.special_colors,
            title: &mut self.title,
            icon_name: &mut self.icon_name,
            cwd: &mut self.cwd,
            document_uri: &mut self.document_uri,
            osc52_read_policy,
            outgoing: &mut self.outgoing,
            events: &mut self.events,
            palette_changed: &mut palette_changed,
            mouse_shape_stack: &mut self.mouse_shape_stack,
            osc99_pending: &mut self.osc99_pending,
            #[cfg(feature = "hyperlinks")]
            hyperlink_pool: &mut self.hyperlink_pool,
            #[cfg(feature = "hyperlinks")]
            active_hyperlink: &mut self.cursor.attrs.hyperlink_id,
            #[cfg(feature = "hyperlinks")]
            hyperlink_inserted: &mut hyperlink_inserted,
        };
        osc::handle_osc(params, terminator, &mut ctx);
        if palette_changed {
            self.palette_epoch = self.palette_epoch.saturating_add(1);
            self.events.push(TerminalEvent::PaletteChanged);
            self.active_grid_mut().mark_all_dirty();
        }
        #[cfg(feature = "hyperlinks")]
        if hyperlink_inserted {
            self.osc8_inserts_since_gc += 1;
            self.maybe_gc_hyperlink_pool();
        }
    }

    fn csi_dispatch(&mut self, params: &CsiParams, intermediates: &[u8], action: u8) {
        #[cfg(feature = "metrics")]
        self.metrics.record_csi();
        self.cursor.charsets.clear_single_shift();
        self.last_written = None;
        if !(intermediates.is_empty() && action == b'b') {
            self.last_graphic = None;
        }

        let rows = self.active_grid().viewport_rows();
        let cols = self.active_grid().cols();
        let bg = self.cursor.attrs.bg;

        match intermediates {
            [] => match action {
                b'A' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let min_row = if self.cursor.row >= self.scroll_top {
                        self.scroll_top
                    } else {
                        0
                    };
                    self.cursor.row = self.cursor.row.saturating_sub(n).max(min_row);
                    self.cursor.pending_wrap = false;
                }
                b'B' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let max_row = if self.cursor.row <= self.scroll_bottom {
                        self.scroll_bottom
                    } else {
                        rows - 1
                    };
                    self.cursor.row = (self.cursor.row + n).min(max_row);
                    self.cursor.pending_wrap = false;
                }
                b'C' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let max_col = if self.cursor.col <= self.scroll_right {
                        self.scroll_right
                    } else {
                        cols - 1
                    };
                    self.cursor.col = (self.cursor.col + n).min(max_col);
                    self.cursor.pending_wrap = false;
                }
                b'D' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let min_col = if self.cursor.col >= self.scroll_left {
                        self.scroll_left
                    } else {
                        0
                    };
                    self.cursor.col = self.cursor.col.saturating_sub(n).max(min_col);
                    self.cursor.pending_wrap = false;
                }
                b'E' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let max_row = if self.cursor.row <= self.scroll_bottom {
                        self.scroll_bottom
                    } else {
                        rows - 1
                    };
                    self.cursor.row = (self.cursor.row + n).min(max_row);
                    self.cursor.col = if self.cursor.col >= self.scroll_left {
                        self.scroll_left
                    } else {
                        0
                    };
                    self.cursor.pending_wrap = false;
                }
                b'F' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let min_row = if self.cursor.row >= self.scroll_top {
                        self.scroll_top
                    } else {
                        0
                    };
                    self.cursor.row = self.cursor.row.saturating_sub(n).max(min_row);
                    self.cursor.col = if self.cursor.col >= self.scroll_left {
                        self.scroll_left
                    } else {
                        0
                    };
                    self.cursor.pending_wrap = false;
                }
                b'G' | b'`' => {
                    let c = usize::from(params.get_nonzero_or(0, 1));
                    self.cursor.col = if self.modes.origin_mode {
                        (self.scroll_left + c - 1).min(self.scroll_right)
                    } else {
                        (c - 1).min(cols - 1)
                    };
                    self.cursor.pending_wrap = false;
                }
                b'd' => {
                    let r = usize::from(params.get_nonzero_or(0, 1));
                    self.cursor.row = if self.modes.origin_mode {
                        (self.scroll_top + r - 1).min(self.scroll_bottom)
                    } else {
                        (r - 1).min(rows - 1)
                    };
                    self.cursor.pending_wrap = false;
                }
                b'H' | b'f' => {
                    let r = usize::from(params.get_nonzero_or(0, 1));
                    let c = usize::from(params.get_nonzero_or(1, 1));
                    self.cursor.row = if self.modes.origin_mode {
                        (self.scroll_top + r - 1).min(self.scroll_bottom)
                    } else {
                        (r - 1).min(rows - 1)
                    };
                    self.cursor.col = if self.modes.origin_mode {
                        (self.scroll_left + c - 1).min(self.scroll_right)
                    } else {
                        (c - 1).min(cols - 1)
                    };
                    self.cursor.pending_wrap = false;
                }
                b'@' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let r = self.cursor.row;
                    let c = self.cursor.col;
                    if self.has_horizontal_margins() && c <= self.scroll_right {
                        let right = self.scroll_right;
                        self.active_grid_mut()
                            .insert_cells_in_margin(r, c, right, n, bg);
                    } else {
                        self.active_grid_mut().insert_cells(r, c, n, bg);
                    }
                    #[cfg(feature = "shell-integration")]
                    if !self.modes.alt_screen {
                        self.shell_integration
                            .on_primary_row_mutated(&self.primary, r);
                    }
                    self.cursor.pending_wrap = false;
                }
                b'P' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let r = self.cursor.row;
                    let c = self.cursor.col;
                    if self.has_horizontal_margins() && c <= self.scroll_right {
                        let right = self.scroll_right;
                        self.active_grid_mut()
                            .delete_cells_in_margin(r, c, right, n, bg);
                    } else {
                        self.active_grid_mut().delete_cells(r, c, n, bg);
                    }
                    #[cfg(feature = "shell-integration")]
                    if !self.modes.alt_screen {
                        self.shell_integration
                            .on_primary_row_mutated(&self.primary, r);
                    }
                    self.cursor.pending_wrap = false;
                }
                b'L' if self.cursor.row >= self.scroll_top
                    && self.cursor.row <= self.scroll_bottom
                    && (!self.has_horizontal_margins()
                        || (self.cursor.col >= self.scroll_left
                            && self.cursor.col <= self.scroll_right)) =>
                {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let r = self.cursor.row;
                    let bottom = self.scroll_bottom;
                    self.scroll_down_in_margins(r, bottom, n);
                    self.cursor.col = if self.has_horizontal_margins() {
                        self.scroll_left
                    } else {
                        0
                    };
                    self.cursor.pending_wrap = false;
                }
                b'M' if self.cursor.row >= self.scroll_top
                    && self.cursor.row <= self.scroll_bottom
                    && (!self.has_horizontal_margins()
                        || (self.cursor.col >= self.scroll_left
                            && self.cursor.col <= self.scroll_right)) =>
                {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let r = self.cursor.row;
                    let bottom = self.scroll_bottom;
                    self.scroll_up_in_margins(r, bottom, n, false);
                    self.cursor.col = if self.has_horizontal_margins() {
                        self.scroll_left
                    } else {
                        0
                    };
                    self.cursor.pending_wrap = false;
                }
                b'K' => {
                    let mode = params.get_or(0, 0);
                    let r = self.cursor.row;
                    let c = self.cursor.col;
                    self.active_grid_mut().erase_in_line(r, c, mode, bg);
                    #[cfg(feature = "shell-integration")]
                    if !self.modes.alt_screen {
                        self.shell_integration
                            .on_primary_row_mutated(&self.primary, r);
                    }
                    self.cursor.pending_wrap = false;
                }
                b'J' => {
                    let mode = params.get_or(0, 0);
                    let r = self.cursor.row;
                    let c = self.cursor.col;
                    self.active_grid_mut().erase_in_display(r, c, mode, bg);
                    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
                    match mode {
                        2 => self.clear_active_viewport_placements(),
                        3 if !self.modes.alt_screen => {
                            let top = self.primary.live_viewport_to_abs(0);
                            self.image_store.prune_before(ScreenKind::Primary, top);
                        }
                        _ => {}
                    }
                    if !self.modes.alt_screen {
                        match mode {
                            0 if rows > 0 => self.notify_shell_rows_mutated(r, rows - 1),
                            1 if rows > 0 => self.notify_shell_rows_mutated(0, r.min(rows - 1)),
                            2 if rows > 0 => self.notify_shell_rows_mutated(0, rows - 1),
                            3 => {
                                self.selection = None;
                                #[cfg(feature = "shell-integration")]
                                self.shell_integration
                                    .sync_after_eviction_if_needed(&self.primary);
                            }
                            _ => {}
                        }
                    }
                    self.cursor.pending_wrap = false;
                }
                b'X' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let r = self.cursor.row;
                    let c = self.cursor.col;
                    self.active_grid_mut().erase_chars(r, c, n, bg);
                    #[cfg(feature = "shell-integration")]
                    if !self.modes.alt_screen {
                        self.shell_integration
                            .on_primary_row_mutated(&self.primary, r);
                    }
                    self.cursor.pending_wrap = false;
                }
                b'S' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let top = self.scroll_top;
                    let bottom = self.scroll_bottom;
                    self.scroll_up_in_margins(top, bottom, n, true);
                }
                b'T' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let top = self.scroll_top;
                    let bottom = self.scroll_bottom;
                    self.scroll_down_in_margins(top, bottom, n);
                }
                b'I' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let max_col = if self.cursor.col <= self.scroll_right {
                        self.scroll_right
                    } else {
                        cols.saturating_sub(1)
                    };
                    self.cursor.col = self.tabs.next(self.cursor.col, n).min(max_col);
                    self.cursor.pending_wrap = false;
                }
                b'Z' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let min_col = if self.cursor.col >= self.scroll_left {
                        self.scroll_left
                    } else {
                        0
                    };
                    self.cursor.col = self.tabs.prev(self.cursor.col, n).max(min_col);
                    self.cursor.pending_wrap = false;
                }
                b'g' => match params.get_or(0, 0) {
                    0 => self.tabs.clear(self.cursor.col),
                    3 => self.tabs.clear_all(),
                    _ => {}
                },
                b'r' => {
                    let top = usize::from(params.get_nonzero_or(0, 1));
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "viewport_rows fits in u16"
                    )]
                    let bottom_default = rows as u16;
                    let bottom = usize::from(params.get_nonzero_or(1, bottom_default)).min(rows);
                    if top < bottom {
                        self.scroll_top = top - 1;
                        self.scroll_bottom = bottom - 1;
                        self.cursor.row = if self.modes.origin_mode {
                            self.scroll_top
                        } else {
                            0
                        };
                        self.cursor.col = if self.modes.origin_mode {
                            self.scroll_left
                        } else {
                            0
                        };
                        self.cursor.pending_wrap = false;
                    }
                }
                b'b' => {
                    let n = usize::from(params.get_nonzero_or(0, 1)).min(cols * rows);
                    if let Some(lg) = self.last_graphic.clone() {
                        for _ in 0..n {
                            self.print_scalar(lg.base);
                            for &comb in &lg.combining {
                                self.print_scalar(comb);
                            }
                        }
                    }
                }
                b'm' => sgr::apply_sgr(params, &mut self.cursor.attrs),
                b'h' => {
                    for sub in params {
                        if let Some(&m) = sub.first() {
                            self.set_mode_ansi(m, true);
                        }
                    }
                }
                b'l' => {
                    for sub in params {
                        if let Some(&m) = sub.first() {
                            self.set_mode_ansi(m, false);
                        }
                    }
                }
                b's' => {
                    if self.modes.left_right_margin_mode {
                        let left = usize::from(params.get_nonzero_or(0, 1));
                        #[expect(clippy::cast_possible_truncation, reason = "cols fits in u16")]
                        let right_default = cols as u16;
                        let right = usize::from(params.get_nonzero_or(1, right_default)).min(cols);
                        if left < right {
                            self.scroll_left = left - 1;
                            self.scroll_right = right - 1;
                            self.cursor.row = if self.modes.origin_mode {
                                self.scroll_top
                            } else {
                                0
                            };
                            self.cursor.col = if self.modes.origin_mode {
                                self.scroll_left
                            } else {
                                0
                            };
                            self.cursor.pending_wrap = false;
                        }
                    } else if params.is_empty() {
                        self.save_cursor();
                    }
                }
                b't' => self.handle_xtwinops(params, cols, rows),
                b'u' => self.restore_cursor(),
                b'c' if params.get_or(0, 0) == 0 => {
                    self.outgoing.extend_from_slice(b"\x1b[?62;4;22c");
                }
                b'n' => match params.get_or(0, 0) {
                    5 => self.outgoing.extend_from_slice(b"\x1b[0n"),
                    6 => {
                        let r = if self.modes.origin_mode {
                            self.cursor.row.saturating_sub(self.scroll_top) + 1
                        } else {
                            self.cursor.row + 1
                        };
                        let c = if self.modes.origin_mode {
                            self.cursor.col.saturating_sub(self.scroll_left) + 1
                        } else {
                            self.cursor.col + 1
                        };
                        let mut reply = String::with_capacity(16);
                        if write!(reply, "\x1b[{r};{c}R").is_err() {
                            // `String`'s `fmt::Write` impl is infallible.
                        }
                        self.outgoing.extend_from_slice(reply.as_bytes());
                    }
                    _ => {}
                },
                _ => {}
            },
            b"?" => match action {
                b'h' => {
                    for sub in params {
                        if let Some(&m) = sub.first() {
                            self.set_mode_dec(m, true);
                        }
                    }
                }
                b'l' => {
                    for sub in params {
                        if let Some(&m) = sub.first() {
                            self.set_mode_dec(m, false);
                        }
                    }
                }
                b'J' => {
                    let mode = params.get_or(0, 0);
                    if mode <= 3 {
                        let r = self.cursor.row;
                        let c = self.cursor.col;
                        self.active_grid_mut()
                            .erase_in_display_selective(r, c, mode, bg);
                        #[cfg(feature = "shell-integration")]
                        if !self.modes.alt_screen && rows > 0 {
                            match mode {
                                0 => self.shell_integration.on_primary_rows_mutated(
                                    &self.primary,
                                    r,
                                    rows - 1,
                                ),
                                1 => self.shell_integration.on_primary_rows_mutated(
                                    &self.primary,
                                    0,
                                    r.min(rows - 1),
                                ),
                                2 | 3 => self.shell_integration.on_primary_rows_mutated(
                                    &self.primary,
                                    0,
                                    rows - 1,
                                ),
                                _ => {}
                            }
                        }
                        self.cursor.pending_wrap = false;
                    }
                }
                b'K' => {
                    let mode = params.get_or(0, 0);
                    if mode <= 2 {
                        let r = self.cursor.row;
                        let c = self.cursor.col;
                        self.active_grid_mut()
                            .erase_in_line_selective(r, c, mode, bg);
                        #[cfg(feature = "shell-integration")]
                        if !self.modes.alt_screen {
                            self.shell_integration
                                .on_primary_row_mutated(&self.primary, r);
                        }
                        self.cursor.pending_wrap = false;
                    }
                }
                b'm' if params.get(0) == Some(4) => {
                    let level = self.modes.modify_other_keys;
                    let mut reply = String::with_capacity(12);
                    if write!(reply, "\x1b[>4;{level}m").is_err() {
                        // `String`'s `fmt::Write` impl is infallible.
                    }
                    self.outgoing.extend_from_slice(reply.as_bytes());
                }
                b'n' => match params.get_or(0, 0) {
                    6 => {
                        let r = if self.modes.origin_mode {
                            self.cursor.row.saturating_sub(self.scroll_top) + 1
                        } else {
                            self.cursor.row + 1
                        };
                        let c = if self.modes.origin_mode {
                            self.cursor.col.saturating_sub(self.scroll_left) + 1
                        } else {
                            self.cursor.col + 1
                        };
                        let mut reply = String::with_capacity(18);
                        if write!(reply, "\x1b[?{r};{c}R").is_err() {
                            // `String`'s `fmt::Write` impl is infallible.
                        }
                        self.outgoing.extend_from_slice(reply.as_bytes());
                    }
                    996 => {
                        let code = self.color_scheme_dsr_code();
                        let mut reply = String::with_capacity(12);
                        if write!(reply, "\x1b[?997;{code}n").is_err() {
                            // `String`'s `fmt::Write` impl is infallible.
                        }
                        self.outgoing.extend_from_slice(reply.as_bytes());
                    }
                    _ => {}
                },
                #[cfg(feature = "kitty-keyboard")]
                b'u' if params.is_empty() => {
                    let bits = self.kitty_keyboard.flags(self.modes.alt_screen).bits();
                    let mut reply = String::with_capacity(8);
                    if write!(reply, "\x1b[?{bits}u").is_err() {
                        // `String`'s `fmt::Write` impl is infallible.
                    }
                    self.outgoing.extend_from_slice(reply.as_bytes());
                }
                _ => {}
            },
            b"!" if action == b'p' => self.soft_reset(),
            b" " => match action {
                b'q' => {
                    let code = params.get_or(0, 0);
                    let style = match code {
                        0 => CursorStyle {
                            shape: self.config.default_cursor_shape,
                            blink: self.config.default_cursor_blink,
                        },
                        1 => CursorStyle {
                            shape: CursorShape::Block,
                            blink: true,
                        },
                        2 => CursorStyle {
                            shape: CursorShape::Block,
                            blink: false,
                        },
                        3 => CursorStyle {
                            shape: CursorShape::Underline,
                            blink: true,
                        },
                        4 => CursorStyle {
                            shape: CursorShape::Underline,
                            blink: false,
                        },
                        5 => CursorStyle {
                            shape: CursorShape::Bar,
                            blink: true,
                        },
                        6 => CursorStyle {
                            shape: CursorShape::Bar,
                            blink: false,
                        },
                        _ => return,
                    };
                    self.cursor.style = style;
                    self.modes.cursor_blink = style.blink;
                    self.events.push(TerminalEvent::CursorStyleChanged(style));
                }
                b'@' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let top = self.scroll_top;
                    let bottom = self.scroll_bottom;
                    let left = self.scroll_left;
                    let right = self.scroll_right;
                    self.active_grid_mut()
                        .shift_left_box(top, bottom, left, right, n, bg);
                    #[cfg(feature = "shell-integration")]
                    if !self.modes.alt_screen {
                        self.shell_integration
                            .on_primary_rows_mutated(&self.primary, top, bottom);
                    }
                    self.cursor.pending_wrap = false;
                }
                b'A' => {
                    let n = usize::from(params.get_nonzero_or(0, 1));
                    let top = self.scroll_top;
                    let bottom = self.scroll_bottom;
                    let left = self.scroll_left;
                    let right = self.scroll_right;
                    self.active_grid_mut()
                        .shift_right_box(top, bottom, left, right, n, bg);
                    #[cfg(feature = "shell-integration")]
                    if !self.modes.alt_screen {
                        self.shell_integration
                            .on_primary_rows_mutated(&self.primary, top, bottom);
                    }
                    self.cursor.pending_wrap = false;
                }
                _ => {}
            },
            b"\"" if action == b'q' => match params.get_or(0, 0) {
                0 | 2 => self.cursor.attrs.set_protected(false),
                1 => self.cursor.attrs.set_protected(true),
                _ => {}
            },
            b"#" => match action {
                b'{' | b'p' => self.sgr_stack.push(&self.cursor.attrs, params),
                b'}' | b'q' => self.sgr_stack.pop(&mut self.cursor.attrs),
                _ => {}
            },
            b">" => match action {
                b'c' if params.get_or(0, 0) == 0 => {
                    self.outgoing.extend_from_slice(b"\x1b[>0;100;0c");
                }
                b'm' if params.get(0) == Some(4) => {
                    let level = params.get_or(1, 0);
                    if level <= 2 {
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "level <= 2 fits in u8"
                        )]
                        {
                            self.modes.modify_other_keys = level as u8;
                        }
                    }
                }
                b'n' if params.get(0) == Some(4) => {
                    self.modes.modify_other_keys = 0;
                }
                b'q' if params.is_empty() || params.get_or(0, 0) == 0 => {
                    let reply = format!("\x1bP>|egui_xterm {}\x1b\\", env!("CARGO_PKG_VERSION"));
                    self.outgoing.extend_from_slice(reply.as_bytes());
                }
                #[cfg(feature = "kitty-keyboard")]
                b'u' => {
                    let flags =
                        KittyKeyboardFlags::from_bits_truncate((params.get_or(0, 0) & 0x1F) as u8);
                    let mode = params.get_or(1, 1);
                    self.kitty_keyboard.push(self.modes.alt_screen, flags, mode);
                }
                _ => {}
            },
            #[cfg(feature = "kitty-keyboard")]
            b"<" if action == b'u' => {
                let count = usize::from(params.get_nonzero_or(0, 1));
                self.kitty_keyboard.pop(self.modes.alt_screen, count);
            }
            #[cfg(feature = "kitty-keyboard")]
            b"=" if action == b'u' => {
                let flags =
                    KittyKeyboardFlags::from_bits_truncate((params.get_or(0, 0) & 0x1F) as u8);
                let mode = params.get_or(1, 1);
                self.kitty_keyboard.set(self.modes.alt_screen, flags, mode);
            }
            b"$" => match action {
                b'p' => {
                    if let Some(m) = params.get(0) {
                        let pm = self.modes.decrqm_ansi(m);
                        let mut reply = String::with_capacity(16);
                        if write!(reply, "\x1b[{m};{pm}$y").is_err() {
                            // `String`'s `fmt::Write` impl is infallible.
                        }
                        self.outgoing.extend_from_slice(reply.as_bytes());
                    }
                }
                b'v' => {
                    if params.get_nonzero_or(4, 1) == 1
                        && params.get_nonzero_or(7, 1) == 1
                        && let Some((st, sl, sb, sr)) =
                            self.resolve_rect_1based(params, 0, rows, cols)
                    {
                        let (min_r, max_r, min_c, max_c) = if self.modes.origin_mode {
                            (
                                self.scroll_top,
                                self.scroll_bottom,
                                self.scroll_left,
                                self.scroll_right,
                            )
                        } else {
                            (0, rows.saturating_sub(1), 0, cols.saturating_sub(1))
                        };
                        let dst_top =
                            min_r.saturating_add(usize::from(params.get_nonzero_or(5, 1)) - 1);
                        let dst_left =
                            min_c.saturating_add(usize::from(params.get_nonzero_or(6, 1)) - 1);
                        if dst_top <= max_r && dst_left <= max_c {
                            let copy_bottom = st + (sb - st).min(max_r - dst_top);
                            let copy_right = sl + (sr - sl).min(max_c - dst_left);
                            self.active_grid_mut().copy_rect(
                                st,
                                sl,
                                copy_bottom,
                                copy_right,
                                dst_top,
                                dst_left,
                                bg,
                            );
                            #[cfg(feature = "shell-integration")]
                            if !self.modes.alt_screen {
                                self.shell_integration.on_primary_rows_mutated(
                                    &self.primary,
                                    dst_top,
                                    dst_top + (copy_bottom - st),
                                );
                            }
                        }
                    }
                }
                b'z' => {
                    if let Some((t, l, b, r)) = self.resolve_rect_1based(params, 0, rows, cols) {
                        self.active_grid_mut()
                            .erase_rect(t, l, b, r, bg, EraseProtection::All);
                        #[cfg(feature = "shell-integration")]
                        if !self.modes.alt_screen {
                            self.shell_integration
                                .on_primary_rows_mutated(&self.primary, t, b);
                        }
                    }
                }
                b'{' => {
                    if let Some((t, l, b, r)) = self.resolve_rect_1based(params, 0, rows, cols) {
                        self.active_grid_mut().erase_rect(
                            t,
                            l,
                            b,
                            r,
                            bg,
                            EraseProtection::UnprotectedOnly,
                        );
                        #[cfg(feature = "shell-integration")]
                        if !self.modes.alt_screen {
                            self.shell_integration
                                .on_primary_rows_mutated(&self.primary, t, b);
                        }
                    }
                }
                b'x' => {
                    let pch = if params.is_empty() {
                        32
                    } else {
                        params.get_or(0, 0)
                    };
                    if let Ok(byte) = u8::try_from(pch)
                        && matches!(byte, 32..=126 | 160..=255)
                        && let Some((t, l, b, r)) = self.resolve_rect_1based(params, 1, rows, cols)
                    {
                        let ch = char::from(byte);
                        let fg = self.cursor.attrs.fg;
                        let write_flags = self.cursor.attrs.flags & CellFlags::WRITE_ATTR_MASK;
                        self.active_grid_mut()
                            .fill_rect(t, l, b, r, ch, fg, bg, write_flags);
                        #[cfg(feature = "shell-integration")]
                        if !self.modes.alt_screen {
                            self.shell_integration
                                .on_primary_rows_mutated(&self.primary, t, b);
                        }
                    }
                }
                _ => {}
            },
            b"?$" if action == b'p' => {
                if let Some(m) = params.get(0) {
                    let pm = self.modes.decrqm_dec(m);
                    let mut reply = String::with_capacity(18);
                    if write!(reply, "\x1b[?{m};{pm}$y").is_err() {
                        // `String`'s `fmt::Write` impl is infallible.
                    }
                    self.outgoing.extend_from_slice(reply.as_bytes());
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], action: u8) {
        if !(intermediates.is_empty() && matches!(action, b'N' | b'O')) {
            self.cursor.charsets.clear_single_shift();
        }
        self.last_written = None;
        self.last_graphic = None;
        match (intermediates, action) {
            ([], b'6') => {
                self.cursor.pending_wrap = false;
                if self.cursor.col == self.scroll_left
                    && self.cursor.row >= self.scroll_top
                    && self.cursor.row <= self.scroll_bottom
                {
                    let top = self.scroll_top;
                    let bottom = self.scroll_bottom;
                    let left = self.scroll_left;
                    let right = self.scroll_right;
                    let bg = self.cursor.attrs.bg;
                    self.active_grid_mut()
                        .shift_right_box(top, bottom, left, right, 1, bg);
                    #[cfg(feature = "shell-integration")]
                    if !self.modes.alt_screen {
                        self.shell_integration
                            .on_primary_rows_mutated(&self.primary, top, bottom);
                    }
                } else if self.cursor.col > 0 {
                    self.cursor.col -= 1;
                }
            }
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'9') => {
                self.cursor.pending_wrap = false;
                let cols = self.active_grid().cols();
                if self.cursor.col == self.scroll_right
                    && self.cursor.row >= self.scroll_top
                    && self.cursor.row <= self.scroll_bottom
                {
                    let top = self.scroll_top;
                    let bottom = self.scroll_bottom;
                    let left = self.scroll_left;
                    let right = self.scroll_right;
                    let bg = self.cursor.attrs.bg;
                    self.active_grid_mut()
                        .shift_left_box(top, bottom, left, right, 1, bg);
                    #[cfg(feature = "shell-integration")]
                    if !self.modes.alt_screen {
                        self.shell_integration
                            .on_primary_rows_mutated(&self.primary, top, bottom);
                    }
                } else if self.cursor.col + 1 < cols {
                    self.cursor.col += 1;
                }
            }
            ([], b'D') => self.linefeed_internal(false),
            ([], b'E') => {
                self.cursor.pending_wrap = false;
                self.cursor.col = if self.cursor.col >= self.scroll_left {
                    self.scroll_left
                } else {
                    0
                };
                self.linefeed_internal(false);
            }
            ([], b'H') => self.tabs.set(self.cursor.col),
            ([], b'M') => self.reverse_index(),
            ([], b'N') => self.cursor.charsets.set_single_shift(2),
            ([], b'O') => self.cursor.charsets.set_single_shift(3),
            ([], b'n') => self.cursor.charsets.set_gl(2),
            ([], b'o') => self.cursor.charsets.set_gl(3),
            ([], b'~') => self.cursor.charsets.set_gr(1),
            ([], b'}') => self.cursor.charsets.set_gr(2),
            ([], b'|') => self.cursor.charsets.set_gr(3),
            ([], b'=') => self.modes.app_keypad = true,
            ([], b'>') => self.modes.app_keypad = false,
            ([], b'c') => self.full_reset(),
            (b"#", b'8') => {
                self.active_grid_mut().fill_alignment_pattern();
                let rows = self.active_grid().viewport_rows();
                let cols = self.active_grid().cols();
                #[cfg(feature = "shell-integration")]
                if !self.modes.alt_screen && rows > 0 {
                    self.shell_integration
                        .on_primary_rows_mutated(&self.primary, 0, rows - 1);
                }
                self.scroll_top = 0;
                self.scroll_bottom = rows.saturating_sub(1);
                self.scroll_left = 0;
                self.scroll_right = cols.saturating_sub(1);
                self.modes.origin_mode = false;
                self.cursor.row = 0;
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            (b"(", code) => {
                if let Some(cs) = parse_charset(code) {
                    self.cursor.charsets.g0 = cs;
                }
            }
            (b")" | b"-", code) => {
                if let Some(cs) = parse_charset(code) {
                    self.cursor.charsets.g1 = cs;
                }
            }
            (b"*" | b".", code) => {
                if let Some(cs) = parse_charset(code) {
                    self.cursor.charsets.g2 = cs;
                }
            }
            (b"+" | b"/", code) => {
                if let Some(cs) = parse_charset(code) {
                    self.cursor.charsets.g3 = cs;
                }
            }
            // Includes 7-bit ST (`ESC \`) following OSC or DCS payloads, which is a no-op terminator.
            _ => {}
        }
    }
}

impl Terminal {
    /// Checks whether an active DEC Mode 2026 latch has exceeded `sync_output_timeout_secs` at
    /// `now_secs`, releasing the frozen snapshot if expired. Returns `true` if flushed.
    pub fn check_sync_timeout(&mut self, now_secs: f64) -> bool {
        self.state.check_sync_timeout(now_secs)
    }

    /// Returns the armed DEC Mode 2026 watchdog deadline on the same clock as
    /// [`Terminal::advance_with_time`], if it has a finite deadline.
    #[must_use]
    pub fn sync_output_deadline(&self) -> Option<f64> {
        self.state.sync_output_deadline()
    }

    /// Resizes the terminal to `(cols.max(1), rows.max(1))`, reflowing primary scrollback and
    /// remapping cursor and selection anchors.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.state.resize(cols, rows);
    }

    /// Sets the viewport pixel dimensions reported by `XTWINOPS 14t`/`16t` and Sixel/Kitty.
    pub fn set_pixel_size(&mut self, width: u16, height: u16) {
        self.state.set_pixel_size(width, height);
    }

    /// Returns the last configured viewport pixel dimensions `(width, height)`.
    #[must_use]
    pub const fn pixel_size(&self) -> (u16, u16) {
        self.state.pixel_size()
    }

    /// Sets the explicit cell pixel dimensions `(width_px, height_px)` used by `XTWINOPS 16t`,
    /// Sixel, Kitty graphics, and `DEC ?1016` pixel mouse fallback.
    pub fn set_cell_pixel_size(&mut self, width_px: u16, height_px: u16) {
        self.state.set_cell_pixel_size(width_px, height_px);
    }

    /// Returns the effective cell pixel size `(width_px, height_px)`.
    #[must_use]
    pub fn cell_pixel_size(&self) -> (u16, u16) {
        self.state.cell_pixel_size()
    }

    /// Sets the window pixel position `(x, y)` reported by `XTWINOPS 13t`.
    pub fn set_window_pos(&mut self, x: i32, y: i32) {
        self.state.set_window_pos(x, y);
    }

    /// Sets the host screen character size `(cols, rows)` reported by `XTWINOPS 19t`.
    pub fn set_screen_char_size(&mut self, size: Option<(u16, u16)>) {
        self.state.set_screen_char_size(size);
    }

    /// Sets whether the host window is iconified (reported by `XTWINOPS 11t`).
    pub fn set_window_iconified(&mut self, iconified: bool) {
        self.state.set_window_iconified(iconified);
    }

    /// Returns `(cols, rows)` of the active grid.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        self.state.size()
    }

    /// Scrolls the primary viewport by `delta_lines` (`> 0` scrolls up into history, `< 0` down).
    pub fn scroll_display(&mut self, delta_lines: i32) {
        self.state.scroll_display(delta_lines);
    }

    /// Snaps the primary viewport back to the live bottom (`display_offset = 0`).
    pub fn scroll_to_bottom(&mut self) {
        self.state.scroll_to_bottom();
    }

    /// Returns the active scrollback offset (`0` when viewing live bottom or on alt screen).
    #[must_use]
    pub fn display_offset(&self) -> usize {
        self.state.display_offset()
    }

    /// Drains pending response bytes (DSR, DA, OSC queries, etc.) to write back to the PTY.
    pub fn drain_outgoing(&mut self) -> Vec<u8> {
        self.state.drain_outgoing()
    }

    /// Drains queued [`TerminalEvent`] notifications produced since the last drain.
    pub fn drain_events(&mut self) -> Vec<TerminalEvent> {
        self.state.drain_events()
    }

    /// Starts a selection at viewport coordinates `(viewport_row, col)`.
    pub fn selection_start(&mut self, viewport_row: usize, col: usize, kind: SelectionKind) {
        self.state.selection_start(viewport_row, col, kind);
    }

    /// Updates the active selection head to viewport coordinates `(viewport_row, col)`.
    pub fn selection_update(&mut self, viewport_row: usize, col: usize) {
        self.state.selection_update(viewport_row, col);
    }

    /// Marks the active selection as finished (keeps it highlighted until cleared or scrolled off).
    pub fn selection_finish(&mut self) {
        self.state.selection_finish();
    }

    /// Clears the current selection.
    pub fn selection_clear(&mut self) {
        self.state.selection_clear();
    }

    /// Selects the word containing `(viewport_row, col)`.
    pub fn selection_word(&mut self, viewport_row: usize, col: usize) {
        self.state.selection_word(viewport_row, col);
    }

    /// Selects the full logical line containing `(viewport_row, col)`.
    pub fn selection_line(&mut self, viewport_row: usize, col: usize) {
        self.state.selection_line(viewport_row, col);
    }

    /// Starts a rectangular block selection at `(viewport_row, col)`.
    pub fn selection_block(&mut self, viewport_row: usize, col: usize) {
        self.state.selection_block(viewport_row, col);
    }

    /// Returns the active [`Selection`], if any.
    #[must_use]
    pub const fn selection(&self) -> Option<&Selection> {
        self.state.selection()
    }

    /// Extracts selected text from the active grid without modifying terminal state.
    #[must_use]
    pub fn selection_text(&self) -> Option<String> {
        self.state.selection_text()
    }

    /// Returns `true` if there is a selection containing non-empty text, without allocating.
    #[must_use]
    pub fn has_non_empty_selection(&self) -> bool {
        self.state.has_non_empty_selection()
    }

    /// Returns `true` if `(viewport_row, col)` falls inside the active selection.
    #[must_use]
    pub fn is_cell_selected(&self, viewport_row: usize, col: usize) -> bool {
        self.state.is_cell_selected(viewport_row, col)
    }

    /// Returns the active screen [`Grid`] (alternate screen when active, otherwise primary).
    #[must_use]
    pub const fn grid(&self) -> &Grid {
        self.state.grid()
    }

    /// Returns the primary [`Grid`] regardless of whether the alternate screen is active.
    #[must_use]
    pub const fn primary_grid(&self) -> &Grid {
        self.state.primary_grid()
    }

    /// Returns the current terminal [`Modes`].
    #[must_use]
    pub const fn modes(&self) -> &Modes {
        self.state.modes()
    }

    /// Returns the live [`Cursor`] state.
    #[must_use]
    pub const fn cursor(&self) -> &Cursor {
        self.state.cursor()
    }

    /// Returns the active 16-color + fg/bg/cursor [`ColorScheme`].
    #[must_use]
    pub const fn palette(&self) -> &ColorScheme {
        self.state.palette()
    }

    /// Returns the monotonic palette epoch bumped whenever palette/special colors change.
    #[must_use]
    pub const fn palette_epoch(&self) -> u64 {
        self.state.palette_epoch()
    }

    /// Returns the `OSC 5` / `105` dynamic SGR special color registers.
    #[must_use]
    pub const fn special_colors(&self) -> SpecialColors {
        self.state.special_colors()
    }

    /// Returns the configured base [`Theme`].
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        self.state.theme()
    }

    /// Applies a new base [`Theme`], preserving any active `OSC` palette overrides.
    pub fn set_theme(&mut self, theme: Theme) {
        self.state.set_theme(theme);
    }

    /// Returns which `OSC` overrides are currently active over the base theme.
    #[must_use]
    pub const fn palette_overrides(&self) -> &PaletteOverrides {
        self.state.palette_overrides()
    }

    /// Returns any `OSC 4` overrides for palette indices `16..=255`.
    #[must_use]
    pub const fn extended_palette(&self) -> &[Option<[u8; 3]>; 240] {
        self.state.extended_palette()
    }

    /// Returns the current window title (`OSC 0`/`OSC 2`).
    #[must_use]
    pub fn title(&self) -> &str {
        self.state.title()
    }

    /// Returns the current icon name (`OSC 0`/`OSC 1`).
    #[must_use]
    pub fn icon_name(&self) -> &str {
        self.state.icon_name()
    }

    /// Returns the last reported current working directory (`OSC 7`).
    #[must_use]
    pub fn cwd(&self) -> Option<&str> {
        self.state.cwd()
    }

    /// Returns the last reported current document URI (`OSC 6`).
    #[must_use]
    pub fn document_uri(&self) -> Option<&str> {
        self.state.document_uri()
    }

    /// Returns the effective `OSC 52` clipboard read policy.
    #[must_use]
    pub const fn osc52_read_policy(&self) -> Osc52ReadPolicy {
        self.state.osc52_read_policy()
    }

    /// Updates the `OSC 52` clipboard read policy.
    pub fn set_osc52_read_policy(&mut self, policy: Osc52ReadPolicy) {
        self.state.set_osc52_read_policy(policy);
    }

    /// Returns the active damage epoch (frozen while DEC 2026 sync latch is active).
    #[must_use]
    pub const fn damage_epoch(&self) -> u64 {
        self.state.damage_epoch()
    }

    /// Returns `true` when the alternate screen buffer is active.
    #[must_use]
    pub const fn is_alt_screen(&self) -> bool {
        self.state.is_alt_screen()
    }

    /// Returns `true` when DEC Mode 2026 synchronized output latch is active.
    #[must_use]
    pub const fn is_synchronized_output(&self) -> bool {
        self.state.is_synchronized_output()
    }

    /// Returns the number of scrollback history lines on the active screen.
    #[must_use]
    pub fn scrollback_len(&self) -> usize {
        self.state.scrollback_len()
    }

    /// Returns the total retained lines (`scrollback_len + viewport_rows`) on the active screen.
    #[must_use]
    pub fn total_lines(&self) -> usize {
        self.state.total_lines()
    }

    /// Looks up sparse [`CellExtra`] data by 1-based `extra_id` from the active snapshot/grid.
    #[must_use]
    pub fn cell_extra(&self, extra_id: u32) -> Option<&CellExtra> {
        self.state.cell_extra(extra_id)
    }

    /// Returns the interned `OSC 8` [`HyperlinkPool`].
    #[must_use]
    pub const fn hyperlink_pool(&self) -> &HyperlinkPool {
        self.state.hyperlink_pool()
    }

    /// Returns live cursor position `(col, row)` in 0-based viewport coordinates.
    #[must_use]
    pub const fn cursor_pos(&self) -> (u16, u16) {
        self.state.cursor_pos()
    }

    /// Returns the active [`CursorShape`].
    #[must_use]
    pub const fn cursor_shape(&self) -> CursorShape {
        self.state.cursor_shape()
    }

    /// Returns `true` if cursor blinking is currently enabled.
    #[must_use]
    pub const fn cursor_blink(&self) -> bool {
        self.state.cursor_blink()
    }

    /// Returns `true` if the cursor is visible (`DECTCEM`).
    #[must_use]
    pub const fn cursor_visible(&self) -> bool {
        self.state.cursor_visible()
    }

    /// Extracts trimmed plain text for visible viewport `row`.
    #[must_use]
    pub fn visible_line_text(&self, row: u16) -> Option<String> {
        self.state.visible_line_text(row)
    }

    /// Extracts trimmed plain text for every row in the visible viewport.
    #[must_use]
    pub fn visible_lines(&self) -> Vec<String> {
        self.state.visible_lines()
    }

    /// Extracts trimmed plain text for retained scrollback `index` (`0` = oldest retained line).
    #[must_use]
    pub fn scrollback_line_text(&self, index: usize) -> Option<String> {
        self.state.scrollback_line_text(index)
    }

    /// Concatenates all retained lines with `\n`, joining soft-wrapped logical lines seamlessly.
    #[must_use]
    pub fn full_text(&self) -> String {
        self.state.full_text()
    }

    /// Extracts trimmed plain text for visible `row` from [`Self::active_snapshot`].
    #[must_use]
    pub fn render_snapshot_line_text(&self, row: u16) -> Option<String> {
        self.state.render_snapshot_line_text(row)
    }

    /// Inspects a single visible cell at `(col, row)` with resolved grapheme and hyperlink info.
    #[must_use]
    pub fn cell_at(&self, col: u16, row: u16) -> Option<CellSnapshot> {
        self.state.cell_at(col, row)
    }

    /// Returns a zero-allocation borrowed [`RenderSnapshot`] for the current frame.
    #[must_use]
    pub fn active_snapshot(&self) -> RenderSnapshot<'_> {
        self.state.active_snapshot()
    }

    /// Returns active inline image placements (`kitty-graphics` or `sixel` feature).
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    #[must_use]
    pub fn image_placements(&self) -> &[ImagePlacement] {
        self.state.image_placements()
    }

    /// Returns stored RGBA/RGB image data by `id` (`kitty-graphics` or `sixel` feature).
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    #[must_use]
    pub fn image_data(&self, id: u32) -> Option<&StoredImage> {
        self.state.image_data(id)
    }

    /// Configures the Kitty file-medium (`t=f`/`t=t`) access policy at runtime.
    #[cfg(feature = "kitty-graphics")]
    pub fn set_kitty_file_medium_policy(&mut self, policy: KittyFileMediumPolicy) {
        self.state.set_kitty_file_medium_policy(policy);
    }

    /// Returns the active Kitty file-medium (`t=f`/`t=t`) access policy.
    #[cfg(feature = "kitty-graphics")]
    #[must_use]
    pub const fn kitty_file_medium_policy(&self) -> KittyFileMediumPolicy {
        self.state.kitty_file_medium_policy()
    }

    /// Installs or clears a custom [`KittyShmReader`] for `t=s` shared-memory image transfers.
    #[cfg(feature = "kitty-graphics")]
    pub fn set_kitty_shm_reader(&mut self, reader: Option<Box<dyn KittyShmReader + Send + Sync>>) {
        self.state.set_kitty_shm_reader(reader);
    }

    /// Advances all playing Kitty graphics animations to `now_secs`, marking dirty rows if any
    /// displayed animation frame changed.
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    pub fn tick_animations(&mut self, now_secs: f64) -> bool {
        self.state.tick_animations(now_secs)
    }

    /// Returns the earliest timestamp (`now_secs`) when any active animation needs its next frame.
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    #[must_use]
    pub fn next_animation_deadline(&self) -> Option<f64> {
        self.state.next_animation_deadline()
    }

    /// Encodes a [`MouseInputEvent`] according to the active mouse tracking protocol/encoding.
    #[must_use]
    pub fn encode_mouse_event(&self, ev: MouseInputEvent) -> Option<Vec<u8>> {
        self.state.encode_mouse_event(ev)
    }

    /// Appends VT mouse reporting bytes for `ev` into `out` without allocating.
    pub fn encode_mouse_event_into(&self, ev: MouseInputEvent, out: &mut Vec<u8>) -> bool {
        self.state.encode_mouse_event_into(ev, out)
    }

    /// Encodes a [`MouseInputEvent`] with explicit pixel coordinates for `DEC ?1016` SGR-Pixels.
    #[must_use]
    pub fn encode_mouse_event_pixels(
        &self,
        ev: MouseInputEvent,
        pixel_x: u32,
        pixel_y: u32,
    ) -> Option<Vec<u8>> {
        self.state.encode_mouse_event_pixels(ev, pixel_x, pixel_y)
    }

    /// Appends VT mouse reporting bytes for `ev` with explicit pixel coordinates into `out`.
    pub fn encode_mouse_event_pixels_into(
        &self,
        ev: MouseInputEvent,
        pixel_x: u32,
        pixel_y: u32,
        out: &mut Vec<u8>,
    ) -> bool {
        self.state
            .encode_mouse_event_pixels_into(ev, pixel_x, pixel_y, out)
    }

    /// Formats pasted text for transmission according to DEC `?2004` bracketed paste mode.
    #[must_use]
    pub fn format_paste(&self, text: &str) -> Vec<u8> {
        self.state.format_paste(text)
    }

    /// Appends sanitized paste bytes into `out` without allocating a temporary buffer.
    pub fn format_paste_into(&self, text: &str, out: &mut Vec<u8>) {
        self.state.format_paste_into(text, out);
    }

    /// Returns focus in/out bytes when DEC `?1004` focus reporting is enabled.
    #[must_use]
    pub const fn focus_report(&self, focused: bool) -> Option<&'static [u8]> {
        self.state.focus_report(focused)
    }

    /// Returns the raw numeric `modifyOtherKeys` level (`0`, `1`, or `2`).
    #[must_use]
    pub const fn modify_other_keys_level(&self) -> u8 {
        self.state.modify_other_keys_level()
    }

    /// Supplies clipboard contents in response to [`TerminalEvent::ClipboardReadRequested`]
    /// or [`TerminalEvent::ClipboardReadPrompt`].
    pub fn provide_clipboard_read(&mut self, selection: ClipboardSelection, text: &str) {
        self.state.provide_clipboard_read(selection, text);
    }

    /// Runs a search across the active grid (`search` feature).
    #[cfg(feature = "search")]
    pub fn search(&mut self, query: &SearchQuery) -> Result<Vec<SearchMatch>, SearchError> {
        self.state.search(query)
    }

    /// Incrementally updates active search matches from dirty lines (`search` feature).
    #[cfg(feature = "search")]
    pub fn refresh_search(&mut self) -> Result<bool, SearchError> {
        self.state.refresh_search()
    }

    /// Starts a chunked incremental search across the active grid (`search` feature).
    #[cfg(feature = "search")]
    pub fn begin_incremental_search(
        &mut self,
        query: &SearchQuery,
    ) -> Result<SearchStepProgress, SearchError> {
        self.state.begin_incremental_search(query)
    }

    /// Advances an active chunked incremental search by up to `max_lines` (`search` feature).
    #[cfg(feature = "search")]
    pub fn step_incremental_search(&mut self, max_lines: usize) -> SearchStepProgress {
        self.state.step_incremental_search(max_lines)
    }

    /// Advances to the next search match (`search` feature).
    #[cfg(feature = "search")]
    pub fn next_search_match(&mut self) -> Option<&SearchMatch> {
        self.state.next_search_match()
    }

    /// Steps backward to the previous search match (`search` feature).
    #[cfg(feature = "search")]
    pub fn prev_search_match(&mut self) -> Option<&SearchMatch> {
        self.state.prev_search_match()
    }

    /// Returns the currently focused search match (`search` feature).
    #[cfg(feature = "search")]
    #[must_use]
    pub fn active_search_match(&self) -> Option<&SearchMatch> {
        self.state.active_search_match()
    }

    /// Returns the 0-based index of the active search match (`search` feature).
    #[cfg(feature = "search")]
    #[must_use]
    pub fn active_search_match_index(&self) -> Option<usize> {
        self.state.active_search_match_index()
    }

    /// Returns all currently cached search matches (`search` feature).
    #[cfg(feature = "search")]
    #[must_use]
    pub fn search_matches(&self) -> &[SearchMatch] {
        self.state.search_matches()
    }

    /// Returns cached search matches intersecting retained `line` (`search` feature).
    #[cfg(feature = "search")]
    #[must_use]
    pub fn search_matches_on_line(&self, line: usize) -> &[SearchMatch] {
        self.state.search_matches_on_line(line)
    }

    /// Returns cached search matches intersecting `abs_line` (`search` feature).
    #[cfg(feature = "search")]
    #[must_use]
    pub fn search_matches_on_abs_line(&self, abs_line: AbsLine) -> &[SearchMatch] {
        self.state.search_matches_on_abs_line(abs_line)
    }

    /// Returns cached search matches intersecting `[start_line, end_line_excl)` (`search` feature).
    #[cfg(feature = "search")]
    #[must_use]
    pub fn search_matches_in_range(
        &self,
        start_line: usize,
        end_line_excl: usize,
    ) -> &[SearchMatch] {
        self.state
            .search_matches_in_range(start_line, end_line_excl)
    }

    /// Clears active search matches (`search` feature).
    #[cfg(feature = "search")]
    pub fn clear_search(&mut self) {
        self.state.clear_search();
    }

    /// Resolves any explicit or implicit hyperlink at visible `(col, row)` (`hyperlinks` feature).
    #[cfg(feature = "hyperlinks")]
    #[must_use]
    pub fn hyperlink_at(&self, col: u16, row: u16) -> Option<&HyperlinkInfo> {
        self.state.hyperlink_at(col, row)
    }

    /// Emits [`TerminalEvent::HyperlinkActivated`] for any link at `(col, row)` (`hyperlinks` feature).
    #[cfg(feature = "hyperlinks")]
    pub fn activate_hyperlink_at(&mut self, col: u16, row: u16) -> bool {
        self.state.activate_hyperlink_at(col, row)
    }

    /// Resolves hyperlink and visible viewport spans at `(col, row)` (`hyperlinks` feature).
    #[cfg(feature = "hyperlinks")]
    #[must_use]
    pub fn hyperlink_visible_spans_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<(&HyperlinkInfo, Vec<HyperlinkSpan>)> {
        self.state.hyperlink_visible_spans_at(col, row)
    }

    /// Returns a zero-allocation view over recorded OSC 133 shell zones (`shell-integration` feature).
    #[cfg(feature = "shell-integration")]
    #[must_use]
    pub fn shell_zones(&self) -> ShellZonesView<'_> {
        self.state.shell_zones()
    }

    /// Jumps the primary scrollback offset to the next/previous OSC 133 prompt (`shell-integration` feature).
    #[cfg(feature = "shell-integration")]
    pub fn jump_to_prompt(&mut self, delta: i32) -> bool {
        self.state.jump_to_prompt(delta)
    }

    /// Extracts captured command output text at retained `line` (`shell-integration` feature).
    #[cfg(feature = "shell-integration")]
    #[must_use]
    pub fn command_output_at(&self, line: usize) -> Option<String> {
        self.state.command_output_at(line)
    }

    /// Returns active Kitty keyboard protocol flags (`kitty-keyboard` feature).
    #[cfg(feature = "kitty-keyboard")]
    #[must_use]
    pub fn kitty_keyboard_flags(&self) -> KittyKeyboardFlags {
        self.state.kitty_keyboard_flags()
    }

    /// Returns the active xterm `modifyOtherKeys` enum level (`kitty-keyboard` feature).
    #[cfg(feature = "kitty-keyboard")]
    #[must_use]
    pub const fn modify_other_keys(&self) -> ModifyOtherKeys {
        self.state.modify_other_keys()
    }

    /// Encodes `ev` with xterm `modifyOtherKeys` into `out` when active (`kitty-keyboard` feature).
    #[cfg(feature = "kitty-keyboard")]
    pub fn encode_modify_other_keys_into(&self, ev: &KittyKeyEvent<'_>, out: &mut Vec<u8>) -> bool {
        self.state.encode_modify_other_keys_into(ev, out)
    }

    /// Encodes `ev` into `out` honoring strict Kitty keyboard precedence (`kitty-keyboard` feature).
    #[cfg(feature = "kitty-keyboard")]
    pub fn encode_key_event_into(&self, ev: &KittyKeyEvent<'_>, out: &mut Vec<u8>) -> bool {
        self.state.encode_key_event_into(ev, out)
    }

    /// Exports a serializable full-state [`TerminalSnapshot`] (`serde` feature).
    #[cfg(feature = "serde")]
    #[must_use]
    pub fn export_snapshot(&self) -> TerminalSnapshot {
        self.state.export_snapshot()
    }

    /// Returns monotonic parser/render/reflow [`TerminalMetrics`] (`metrics` feature).
    #[cfg(feature = "metrics")]
    #[must_use]
    pub const fn metrics(&self) -> &TerminalMetrics {
        self.state.metrics()
    }

    /// Records a rendered frame with `dirty_rows` updated rows (`metrics` feature).
    #[cfg(feature = "metrics")]
    pub fn record_frame_rendered(&mut self, dirty_rows: u64) {
        self.state.record_frame_rendered(dirty_rows);
    }
}

fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0usize;
    while i + 3 <= input.len() {
        let triple =
            (u32::from(input[i]) << 16) | (u32::from(input[i + 1]) << 8) | u32::from(input[i + 2]);
        out.push(char::from(TABLE[((triple >> 18) & 0x3F) as usize]));
        out.push(char::from(TABLE[((triple >> 12) & 0x3F) as usize]));
        out.push(char::from(TABLE[((triple >> 6) & 0x3F) as usize]));
        out.push(char::from(TABLE[(triple & 0x3F) as usize]));
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let triple = u32::from(input[i]) << 16;
        out.push(char::from(TABLE[((triple >> 18) & 0x3F) as usize]));
        out.push(char::from(TABLE[((triple >> 12) & 0x3F) as usize]));
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let triple = (u32::from(input[i]) << 16) | (u32::from(input[i + 1]) << 8);
        out.push(char::from(TABLE[((triple >> 18) & 0x3F) as usize]));
        out.push(char::from(TABLE[((triple >> 12) & 0x3F) as usize]));
        out.push(char::from(TABLE[((triple >> 6) & 0x3F) as usize]));
        out.push('=');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_term(cols: u16, rows: u16) -> Terminal {
        Terminal::new(cols, rows, CoreConfig::default())
    }

    #[test]
    fn public_inspection_api_and_cell_snapshot() {
        let mut term = new_term(10, 4);
        assert_eq!(term.size(), (10, 4));
        term.advance("hello中文".as_bytes());
        assert_eq!(term.visible_line_text(0), Some("hello中文".to_owned()));
        assert_eq!(term.visible_lines().len(), 4);
        assert_eq!(term.cursor_pos(), (9, 0));
        assert_eq!(term.cursor_shape(), CursorShape::Block);
        assert!(term.cursor_blink());
        assert!(term.cursor_visible());
        assert!(!term.is_alt_screen());
        assert!(!term.is_synchronized_output());
        assert_eq!(term.full_text(), "hello中文");

        let c_mid = term.cell_at(5, 0).unwrap();
        assert_eq!(c_mid.ch, '中');
        assert_eq!(c_mid.grapheme, "中");
        assert_eq!(c_mid.width, 2);
        assert!(c_mid.is_wide);
        assert!(!c_mid.is_wide_spacer);

        let c_sp = term.cell_at(6, 0).unwrap();
        assert!(!c_sp.is_wide);
        assert!(c_sp.is_wide_spacer);
        assert_eq!(c_sp.width, 0);
        assert!(c_sp.grapheme.is_empty());

        term.grid().debug_verify_extra_invariants();
        term.grid().debug_verify_wide_invariants();
    }

    #[test]
    fn grapheme_combining_vs16_zwj_ri_and_zalgo_cap() {
        let mut term = new_term(12, 4);
        // Combining acute accent + VS16 promotion + ZWJ + Regional Indicator flag.
        term.advance("e\u{0301}\u{2764}\u{FE0F}👩\u{200D}🔬🇺🇸".as_bytes());
        let line = term.visible_line_text(0).unwrap();
        assert_eq!(line, "e\u{0301}\u{2764}\u{FE0F}👩\u{200D}🔬🇺🇸");

        let e_cell = term.cell_at(0, 0).unwrap();
        assert_eq!(e_cell.grapheme, "e\u{0301}");
        assert_eq!(e_cell.width, 1);

        let heart = term.cell_at(1, 0).unwrap();
        assert_eq!(heart.grapheme, "\u{2764}\u{FE0F}");
        assert!(heart.is_wide);
        assert!(term.cell_at(2, 0).unwrap().is_wide_spacer);

        // VS16 promotion at right margin wraps atomically.
        let mut t2 = new_term(5, 3);
        t2.advance("abcd\u{2764}\u{FE0F}".as_bytes());
        assert_eq!(t2.visible_line_text(0).unwrap(), "abcd");
        assert_eq!(t2.visible_line_text(1).unwrap(), "\u{2764}\u{FE0F}");
        t2.grid().debug_verify_wide_invariants();

        // VS15 demotion from wide back to narrow.
        let mut t3 = new_term(10, 2);
        t3.advance("\u{2764}\u{FE0F}\u{FE0E}".as_bytes());
        // After VS16 then VS15, unicode-width stays >=1; test direct demotion invariant.
        t3.grid().debug_verify_wide_invariants();

        // 32-mark Zalgo cap.
        let mut t4 = new_term(10, 2);
        let mut zalgo = String::from("a");
        for _ in 0..64 {
            zalgo.push('\u{0301}');
        }
        t4.advance(zalgo.as_bytes());
        let snap = t4.cell_at(0, 0).unwrap();
        assert_eq!(snap.grapheme.chars().count(), 1 + MAX_COMBINING_PER_CELL);
        t4.grid().debug_verify_extra_invariants();
    }

    #[test]
    fn pending_wrap_decstbm_decom_il_dl_and_alt_screen() {
        let mut term = new_term(5, 5);
        // Pending wrap cleared by CUB 1 before printing.
        term.advance(b"ABCDE\x1b[DZ");
        assert_eq!(term.visible_line_text(0).unwrap(), "ABCZE");
        assert_eq!(term.visible_line_text(1).unwrap(), "");
        // Pending wrap cleared by CR overwrites col 0 without wrapping.
        term.advance(b"!\rX");
        assert_eq!(term.visible_line_text(0).unwrap(), "XBCZ!");
        assert_eq!(term.visible_line_text(1).unwrap(), "");

        // DECSTBM + DECOM clamping.
        term.advance(b"\x1b[2;4r\x1b[?6h\x1b[1;1Hrow1\x1b[99;1Hrow3");
        assert_eq!(term.visible_line_text(1).unwrap(), "row1");
        assert_eq!(term.visible_line_text(3).unwrap(), "row3");

        // IL/DL outside scroll margins is a no-op (cursor remains at col 1, row 0).
        term.advance(b"\x1b[?6l\x1b[1;1H\x1b[2K\x1b[1;2Htop\x1b[1;2H\x1b[2L");
        assert_eq!(term.visible_line_text(0).unwrap(), " top");
        assert_eq!(term.cursor_pos(), (1, 0));

        // IL inside scroll margins resets col to 0.
        term.advance(b"\x1b[2;3H\x1b[1L");
        assert_eq!(term.cursor_pos(), (0, 1));
        assert_eq!(term.visible_line_text(1).unwrap(), "");
        assert_eq!(term.visible_line_text(2).unwrap(), "row1");

        // Independent per-screen DECSC/DECRC with ?1049.
        term.advance(b"\x1b[1;2H\x1b7\x1b[?1049h\x1b[3;2Halt\x1b7");
        assert!(term.is_alt_screen());
        assert_eq!(term.visible_line_text(2).unwrap(), " alt");
        // Redundant ?1049h must not clobber primary saved cursor.
        term.advance(b"\x1b[?1049h\x1b[?1049l");
        assert!(!term.is_alt_screen());
        assert_eq!(term.cursor_pos(), (1, 0));
    }

    #[test]
    fn sgr_colon_semicolon_and_underline_colors() {
        let mut term = new_term(20, 2);
        term.advance(b"\x1b[1;3;21;38:2::10:20:30;48;5;196;58:5:42;53mX\x1b[0mY");
        let x = term.cell_at(0, 0).unwrap();
        assert_eq!(x.ch, 'X');
        assert!(
            x.flags
                .contains(CellFlags::BOLD | CellFlags::ITALIC | CellFlags::OVERLINE)
        );
        assert_eq!(x.underline_style, UnderlineStyle::Double);
        assert_eq!(x.fg, PackedColor::rgb(10, 20, 30));
        assert_eq!(x.bg, PackedColor::indexed(196));
        assert_eq!(x.underline_color, Some(PackedColor::indexed(42)));

        let y = term.cell_at(1, 0).unwrap();
        assert_eq!(y.fg, PackedColor::Default);
        assert_eq!(y.bg, PackedColor::Default);
        assert_eq!(y.underline_style, UnderlineStyle::None);
        assert_eq!(y.underline_color, None);
    }

    #[test]
    fn dec_2026_synchronized_output_and_watchdog() {
        let mut term = new_term(20, 3);
        term.advance_with_time(b"frame0", 1.0);
        assert_eq!(term.render_snapshot_line_text(0).unwrap(), "frame0");

        // Enter BSU at t = 2.0, write partial frame1.
        term.advance_with_time(b"\x1b[?2026h\rframe1", 2.0);
        assert!(term.is_synchronized_output());
        assert_eq!(term.visible_line_text(0).unwrap(), "frame1");
        assert_eq!(term.render_snapshot_line_text(0).unwrap(), "frame0");

        // DECRQM ? 2026 -> 1 (set).
        term.advance_with_time(b"\x1b[?2026$p", 2.1);
        assert_eq!(term.drain_outgoing(), b"\x1b[?2026;1$y");

        // Still latched at +200ms.
        term.advance_with_time(b"", 2.20);
        assert!(term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0).unwrap(), "frame0");

        // Watchdog expires at +250ms.
        term.advance_with_time(b"", 2.26);
        assert!(!term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0).unwrap(), "frame1");

        // Explicit ESU release.
        term.advance_with_time(b"\x1b[?2026h\rframe2", 3.0);
        assert_eq!(term.render_snapshot_line_text(0).unwrap(), "frame1");
        term.advance_with_time(b"\x1b[?2026l", 3.05);
        assert!(!term.is_synchronized_output());
        assert_eq!(term.render_snapshot_line_text(0).unwrap(), "frame2");
    }

    #[test]
    fn osc_title_cwd_palette_and_52_security() {
        let mut term = new_term(20, 3);
        term.advance(b"\x1b]0;my;title\x07");
        term.advance(b"\x1b]7;file://localhost/tmp/hello%20world\x1b\\");
        term.advance(b"\x1b]4;1;rgb:ff/00/80;1;?\x07");
        term.advance(b"\x1b]10;?\x1b\\");
        // OSC 52 read blocked by default; write works.
        term.advance(b"\x1b]52;c;?\x07\x1b]52;c;SGVsbG8=\x07");

        assert_eq!(term.title(), "my;title");
        assert_eq!(term.icon_name(), "my;title");
        assert_eq!(term.cwd(), Some("/tmp/hello world"));

        let out = String::from_utf8(term.drain_outgoing()).unwrap();
        assert!(out.contains("\x1b]4;1;rgb:ffff/0000/8080\x07"));
        assert!(out.contains("\x1b]10;rgb:"));

        let events = term.drain_events();
        assert!(events.contains(&TerminalEvent::TitleChanged("my;title".to_owned())));
        assert!(events.contains(&TerminalEvent::CwdChanged("/tmp/hello world".to_owned())));
        assert!(events.contains(&TerminalEvent::ClipboardWrite("Hello".to_owned())));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, TerminalEvent::ClipboardReadRequested { .. }))
        );

        // Reset palette 104 / 110.
        term.advance(b"\x1b]104\x07\x1b]110\x07");
        assert_eq!(term.palette().ansi[1], ColorScheme::dark().ansi[1]);
    }

    #[test]
    fn selection_stream_word_line_block_and_scrollback() {
        let mut term = new_term(10, 5);
        term.advance("hello world\r\nfoo中bar\r\n12345".as_bytes());
        // Row 0: "hello worl" (soft_wrapped), Row 1: "d", Row 2: "foo中bar", Row 3: "12345".

        // Word selection across soft-wrap ("world").
        term.selection_word(0, 7);
        assert_eq!(term.selection_text(), Some("world".to_owned()));

        // Line selection joins soft-wrapped physical lines without '\n'.
        term.selection_line(0, 2);
        assert_eq!(term.selection_text(), Some("hello world".to_owned()));

        // Wide-char normalization on Stream selection hitting WIDE_SPACER of '中' (row 2 col 4).
        term.selection_start(2, 4, SelectionMode::Stream);
        term.selection_update(2, 5);
        term.selection_finish();
        assert_eq!(term.selection_text(), Some("中b".to_owned()));

        // Block selection.
        term.selection_block(2, 0);
        term.selection_update(3, 2);
        term.selection_finish();
        assert_eq!(term.selection_text(), Some("foo\n123".to_owned()));
    }

    #[test]
    fn mouse_encodings_queries_tabs_rep_and_dec_special_graphics() {
        let mut term = new_term(20, 4);
        // SGR mouse encoding (?1002 + ?1006).
        term.advance(b"\x1b[?1002h\x1b[?1006h");
        let press = term
            .encode_mouse_event(MouseInputEvent {
                kind: MouseEventKind::Press,
                button: Some(MouseButton::Left),
                col: 4,
                row: 9,
                modifiers: MouseModifiers {
                    shift: false,
                    alt: false,
                    ctrl: true,
                },
            })
            .unwrap();
        assert_eq!(press, b"\x1b[<16;5;10M");

        let release = term
            .encode_mouse_event(MouseInputEvent {
                kind: MouseEventKind::Release,
                button: Some(MouseButton::Left),
                col: 4,
                row: 9,
                modifiers: MouseModifiers::default(),
            })
            .unwrap();
        assert_eq!(release, b"\x1b[<0;5;10m");

        // DEC Special Graphics + REP + Queries.
        term.advance(b"\x1b(0lqk\x1b(B\r\n");
        assert_eq!(term.visible_line_text(0).unwrap(), "┌─┐");

        term.advance(b"a\x1b[3b\n\x1b[2b");
        assert_eq!(term.visible_line_text(1).unwrap(), "aaaa");

        term.advance(b"\x1b[c\x1b[>c\x1b[5n\x1b[6n\x1b[?6n\x1b[4$p");
        let replies = String::from_utf8(term.drain_outgoing()).unwrap();
        assert!(replies.contains("\x1b[?62;"));
        assert!(replies.contains("\x1b[>0;100;0c"));
        assert!(replies.contains("\x1b[0n"));
        assert!(replies.contains("\x1b[4;2$y"));

        // Bracketed paste sanitization.
        term.advance(b"\x1b[?2004h");
        assert_eq!(
            term.format_paste("hi\x1b[201~evil"),
            b"\x1b[200~hievil\x1b[201~"
        );
    }

    #[test]
    fn scrollback_lookup_display_offset_pinning_and_ed3() {
        let mut term = new_term(10, 3);
        for i in 0..8 {
            term.advance(format!("line{i}\r\n").as_bytes());
        }
        assert!(term.scrollback_len() >= 5);
        assert_eq!(term.scrollback_line_text(0), Some("line0".to_owned()));

        term.scroll_display(3);
        let pinned_top = term.visible_line_text(0);
        // New output while scrolled up keeps viewport pinned to the same scrollback line.
        term.advance(b"extra\r\n");
        assert_eq!(term.visible_line_text(0), pinned_top);

        // ED 3 clears scrollback only.
        term.advance(b"\x1b[3J");
        assert_eq!(term.scrollback_len(), 0);
        assert_eq!(term.display_offset(), 0);
    }

    #[test]
    fn dch_clears_wrap_spacer_invariants() {
        let mut term = new_term(5, 3);
        term.advance("abcd\u{4E2D}\x1b[1;1H\x1b[1P".as_bytes());
        term.grid().debug_verify_wide_invariants();
        term.grid().debug_verify_extra_invariants();
        assert_eq!(term.visible_line_text(0), Some("bcd".to_owned()));
        assert_eq!(term.visible_line_text(1), Some("中".to_owned()));
    }

    #[test]
    fn reflow_trailing_space_cursor_preservation_and_randomized_seeds() {
        // Repro: "abc " across 80 -> 3 -> 80 + "d" must yield "abc d", not "abd".
        let mut term = new_term(80, 24);
        term.advance(b"abc ");
        term.resize(3, 24);
        assert_eq!(term.cursor_pos(), (1, 1));
        term.resize(80, 24);
        assert_eq!(term.cursor_pos(), (4, 0));
        term.advance(b"d");
        assert_eq!(term.visible_line_text(0), Some("abc d".to_owned()));

        // Through cols = 1 as well.
        let mut t1 = new_term(80, 24);
        t1.advance(b"abc ");
        t1.resize(1, 24);
        t1.resize(80, 24);
        t1.advance(b"d");
        assert_eq!(t1.visible_line_text(0), Some("abc d".to_owned()));

        // Blank line with trailing spaces across 80 -> 3 -> 80.
        let mut t_blank = new_term(80, 24);
        t_blank.advance(b"    ");
        t_blank.resize(3, 24);
        t_blank.resize(80, 24);
        t_blank.advance(b"d");
        assert_eq!(t_blank.visible_line_text(0), Some("    d".to_owned()));

        // Pending wrap with trailing spaces ("abc   " at 3 cols -> 80 cols).
        let mut t_pw = new_term(3, 24);
        t_pw.advance(b"abc   ");
        assert!(t_pw.cursor().pending_wrap);
        t_pw.resize(80, 24);
        assert_eq!(t_pw.cursor_pos(), (6, 0));
        assert!(!t_pw.cursor().pending_wrap);
        t_pw.advance(b"d");
        assert_eq!(t_pw.visible_line_text(0), Some("abc   d".to_owned()));

        // Randomized multi-step reflow seeds (1..=32) preserving cursor_pos & full_text.
        for seed in 1_u64..=32 {
            let mut rng = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let mut next_u32 = || {
                rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                (rng >> 32) as u32
            };
            let mut t = Terminal::new(
                80,
                24,
                CoreConfig {
                    scrollback_limit: 500,
                    ..CoreConfig::default()
                },
            );
            let line_count = 3 + (next_u32() % 6);
            for line_i in 0..line_count {
                let len = 1 + (next_u32() % 25);
                let mut line = String::new();
                for _ in 0..len {
                    match next_u32() % 8 {
                        0 => line.push(' '),
                        1 => line.push('中'),
                        _ => {
                            let ch = char::from(b'a' + u8::try_from(next_u32() % 26).unwrap_or(0));
                            line.push(ch);
                        }
                    }
                }
                t.advance(line.as_bytes());
                if line_i + 1 < line_count {
                    t.advance(b"\r\n");
                }
            }
            let expected_text = t.full_text();
            let expected_cursor = t.cursor_pos();
            for _ in 0..8 {
                let w = u16::try_from(1 + (next_u32() % 80)).unwrap_or(10);
                let h = u16::try_from(4 + (next_u32() % 24)).unwrap_or(10);
                t.resize(w, h);
                t.grid().debug_verify_wide_invariants();
                t.grid().debug_verify_extra_invariants();
            }
            t.resize(80, 24);
            assert_eq!(t.full_text(), expected_text, "seed={seed}: full_text");
            assert_eq!(t.cursor_pos(), expected_cursor, "seed={seed}: cursor_pos");
        }
    }

    #[test]
    fn vs16_wide_promotion_on_1_row_terminal_and_below_scroll_bottom() {
        let mut t2 = new_term(2, 1);
        t2.advance("a\u{2764}\u{FE0F}".as_bytes());
        t2.grid().debug_verify_wide_invariants();
        t2.grid().debug_verify_extra_invariants();
        assert_eq!(t2.visible_line_text(0), Some("\u{2764}\u{FE0F}".to_owned()));
        assert_eq!(t2.full_text(), "a\u{2764}\u{FE0F}");
        assert_eq!(t2.cursor_pos(), (1, 0));
        assert!(t2.cursor().pending_wrap);

        let mut t5 = new_term(5, 1);
        t5.advance("abcd\u{2764}\u{FE0F}".as_bytes());
        t5.grid().debug_verify_wide_invariants();
        t5.grid().debug_verify_extra_invariants();
        assert_eq!(t5.visible_line_text(0), Some("\u{2764}\u{FE0F}".to_owned()));
        assert_eq!(t5.full_text(), "abcd\u{2764}\u{FE0F}");
        assert_eq!(t5.cursor_pos(), (2, 0));

        let mut t_zero_sb = Terminal::new(
            2,
            1,
            CoreConfig {
                scrollback_limit: 0,
                ..CoreConfig::default()
            },
        );
        t_zero_sb.advance("a\u{2764}\u{FE0F}".as_bytes());
        t_zero_sb.grid().debug_verify_wide_invariants();
        t_zero_sb.grid().debug_verify_extra_invariants();
        assert_eq!(
            t_zero_sb.visible_line_text(0),
            Some("\u{2764}\u{FE0F}".to_owned())
        );

        // Below scroll_bottom on the bottom-most row cannot scroll or wrap down: do not store
        // a 2-col grapheme inside a 1-col cell.
        let mut t_below = new_term(5, 4);
        t_below.advance("\x1b[1;2r\x1b[4;1Habcd\u{2764}\u{FE0F}".as_bytes());
        t_below.grid().debug_verify_wide_invariants();
        t_below.grid().debug_verify_extra_invariants();
        let snap = t_below.cell_at(4, 3).unwrap();
        assert_eq!(snap.width, 1);
        assert_eq!(snap.grapheme, "\u{2764}");
    }

    #[test]
    fn sgr_truncated_semicolon_color_does_not_leak_flags() {
        let mut term = new_term(20, 2);
        term.advance(b"\x1b[38;2;10;20mA\x1b[0m");
        term.advance(b"\x1b[38;5mB\x1b[0m");
        term.advance(b"\x1b[48;5mC\x1b[0m");
        term.advance(b"\x1b[58;2;1mD\x1b[0m");
        term.advance(b"\x1b[38;9;1mE\x1b[0m");
        term.advance(b"\x1b[38;2;10;20;30;1mF");

        for col in 0..5 {
            let c = term.cell_at(col, 0).unwrap();
            assert_eq!(c.flags, CellFlags::empty(), "col={col} leaked flags");
            assert_eq!(c.fg, PackedColor::Default, "col={col} leaked fg");
            assert_eq!(c.bg, PackedColor::Default, "col={col} leaked bg");
            assert_eq!(c.underline_color, None, "col={col} leaked underline_color");
        }

        let valid = term.cell_at(5, 0).unwrap();
        assert_eq!(valid.ch, 'F');
        assert_eq!(valid.fg, PackedColor::rgb(10, 20, 30));
        assert!(valid.flags.contains(CellFlags::BOLD));
    }

    #[test]
    fn selection_word_and_line_backwards_drag_and_wide_spacer_extra() {
        let mut term = new_term(20, 4);
        term.advance(b"hello world foo");
        term.selection_start(0, 8, SelectionMode::Word);
        assert_eq!(term.selection_text(), Some("world".to_owned()));
        term.selection_update(0, 2);
        assert_eq!(term.selection_text(), Some("hello world".to_owned()));
        term.selection_update(0, 13);
        assert_eq!(term.selection_text(), Some("world foo".to_owned()));
        term.selection_update(0, 7);
        assert_eq!(term.selection_text(), Some("world".to_owned()));

        // Wrapped logical line backwards drag.
        let mut t_line = new_term(5, 4);
        t_line.advance(b"top\r\n12345678");
        t_line.selection_start(1, 1, SelectionMode::Line);
        assert_eq!(t_line.selection_text(), Some("12345678".to_owned()));
        t_line.selection_update(0, 0);
        assert_eq!(t_line.selection_text(), Some("top\n12345678".to_owned()));
        t_line.selection_update(2, 1);
        assert_eq!(t_line.selection_text(), Some("12345678".to_owned()));

        // Word selection hitting WIDE_SPACER whose WIDE_LEAD has CellExtra combining marks.
        let mut t_wide = new_term(20, 2);
        t_wide.advance("a \u{2764}\u{FE0F}b".as_bytes());
        t_wide.selection_word(0, 3);
        assert_eq!(
            t_wide.selection_text(),
            Some("\u{2764}\u{FE0F}b".to_owned())
        );

        // Word selection backwards drag preserved across resize reflow.
        let mut t_reflow = new_term(20, 4);
        t_reflow.advance(b"alpha beta");
        t_reflow.selection_start(0, 7, SelectionMode::Word);
        t_reflow.resize(6, 4);
        t_reflow.selection_update(0, 1);
        assert_eq!(t_reflow.selection_text(), Some("alpha beta".to_owned()));
    }

    #[test]
    fn enter_alt_screen_duplicate_is_idempotent() {
        let mut term = new_term(20, 5);
        term.advance(b"PRI\x1b[?1049h\x1b[2;4rALT_DATA\x1b[?1049h");
        assert!(term.is_alt_screen());
        assert_eq!(term.visible_line_text(0), Some("ALT_DATA".to_owned()));
        assert_eq!(term.state.scroll_top, 1);
        assert_eq!(term.state.scroll_bottom, 3);
        term.advance(b"\x1b[?1049l");
        assert!(!term.is_alt_screen());
        assert_eq!(term.visible_line_text(0), Some("PRI".to_owned()));

        let mut t1047 = new_term(20, 4);
        t1047.advance(b"\x1b[?1047hALT1047\x1b[?1047h");
        assert_eq!(t1047.visible_line_text(0), Some("ALT1047".to_owned()));

        let mut t47 = new_term(20, 4);
        t47.advance(b"\x1b[?47hALT47\x1b[?47h");
        assert_eq!(t47.visible_line_text(0), Some("ALT47".to_owned()));
    }

    #[test]
    fn dec_2026_watchdog_nan_and_advance_before_time() {
        // NaN must not wipe prior finite last_time_secs.
        let mut t1 = new_term(20, 2);
        t1.advance_with_time(b"a", 1.0);
        t1.advance_with_time(b"\x1b[?2026h\rb", f64::NAN);
        assert!(t1.is_synchronized_output());
        assert_eq!(t1.render_snapshot_line_text(0), Some("a".to_owned()));
        t1.advance_with_time(b"", 1.3);
        assert!(!t1.is_synchronized_output());
        assert_eq!(t1.render_snapshot_line_text(0), Some("b".to_owned()));

        // NaN with no prior clock still arms on first subsequent finite tick.
        let mut t2 = new_term(20, 2);
        t2.advance_with_time(b"\x1b[?2026hb", f64::NAN);
        assert!(t2.is_synchronized_output());
        t2.advance_with_time(b"", 10.0);
        assert!(t2.is_synchronized_output());
        t2.advance_with_time(b"", 10.26);
        assert!(!t2.is_synchronized_output());

        // BSU delivered via advance() arms on first advance_with_time() / check_sync_timeout().
        let mut t3 = new_term(20, 2);
        t3.advance(b"old\x1b[?2026h\rnew");
        assert!(t3.is_synchronized_output());
        assert_eq!(t3.render_snapshot_line_text(0), Some("old".to_owned()));
        t3.advance_with_time(b"", 100.0);
        assert!(t3.is_synchronized_output());
        t3.advance_with_time(b"", 100.25);
        assert!(!t3.is_synchronized_output());
        assert_eq!(t3.render_snapshot_line_text(0), Some("new".to_owned()));
    }

    #[test]
    fn selection_eviction_bounds_and_scroll_clamping() {
        let mut term = Terminal::new(
            10,
            2,
            CoreConfig {
                scrollback_limit: 1,
                ..CoreConfig::default()
            },
        );
        term.advance(b"l0\r\nl1\r\nl2");
        // History has "l0", viewport has "l1", "l2". Select from oldest (row 0 in history) to l2.
        term.scroll_display(1);
        term.selection_start(0, 0, SelectionMode::Stream);
        term.scroll_to_bottom();
        term.selection_update(1, 1);
        term.selection_finish();
        assert_eq!(term.selection_text(), Some("l0\nl1\nl2".to_owned()));

        // Scroll 1 line -> l0 is evicted; anchor clamps to oldest retained line (l1).
        term.advance(b"\r\nl3");
        assert_eq!(term.selection_text(), Some("l1\nl2".to_owned()));

        // Direct extract_text on fully evicted Selection returns None.
        let stale_sel = Selection::new(
            SelectionMode::Stream,
            GridPoint::new(AbsLine(0), 0),
            GridPoint::new(AbsLine(0), 1),
        );
        assert_eq!(selection::extract_text(term.grid(), &stale_sel), None);

        // Scroll 4 more lines -> entire selection evicted and cleared.
        term.advance(b"\r\nl4\r\nl5\r\nl6\r\nl7");
        assert_eq!(term.selection_text(), None);
        assert!(term.selection().is_none());
    }

    #[test]
    fn c1_vt500_eight_bit_execute_and_sequences() {
        let cfg = CoreConfig {
            c1_policy: C1Policy::Vt500EightBit,
            ..CoreConfig::default()
        };

        // 0x84 (IND): move down one line at same column, clearing pending_wrap.
        let mut t_ind = Terminal::new(10, 4, cfg.clone());
        t_ind.advance(b"\x9B1;4HA\x84B");
        assert_eq!(t_ind.cell_at(3, 0).unwrap().ch, 'A');
        assert_eq!(t_ind.cell_at(4, 1).unwrap().ch, 'B');
        // IND from pending_wrap at right margin moves to next row at last col (matching ESC D).
        let mut t_ind_pw = Terminal::new(5, 3, cfg.clone());
        t_ind_pw.advance(b"12345\x84X");
        assert_eq!(t_ind_pw.visible_line_text(0), Some("12345".to_owned()));
        assert_eq!(t_ind_pw.cell_at(4, 1).unwrap().ch, 'X');

        // 0x85 (NEL): move to col 0 on next line, clearing pending_wrap.
        let mut t_nel = Terminal::new(10, 4, cfg.clone());
        t_nel.advance(b"\x9B1;5HA\x85B");
        assert_eq!(t_nel.cell_at(4, 0).unwrap().ch, 'A');
        assert_eq!(t_nel.cell_at(0, 1).unwrap().ch, 'B');
        let mut t_nel_pw = Terminal::new(5, 3, cfg.clone());
        t_nel_pw.advance(b"12345\x85X");
        assert_eq!(t_nel_pw.visible_line_text(0), Some("12345".to_owned()));
        assert_eq!(t_nel_pw.visible_line_text(1), Some("X".to_owned()));

        // 0x88 (HTS): set horizontal tab stop at current column.
        let mut t_hts = Terminal::new(10, 4, cfg.clone());
        t_hts.advance(b"\x9B3g\x9B1;4H\x88\x9B1;1H\tZ");
        assert_eq!(t_hts.cell_at(3, 0).unwrap().ch, 'Z');

        // 0x8D (RI): move up one line at same column (scroll down at scroll_top), clearing pending_wrap.
        let mut t_ri = Terminal::new(10, 4, cfg.clone());
        t_ri.advance(b"\x9B2;4HA\x8DB");
        assert_eq!(t_ri.cell_at(3, 1).unwrap().ch, 'A');
        assert_eq!(t_ri.cell_at(4, 0).unwrap().ch, 'B');
        t_ri.advance(b"\x9B1;2H\x8DC");
        assert_eq!(t_ri.cell_at(1, 0).unwrap().ch, 'C');
        assert_eq!(t_ri.cell_at(4, 1).unwrap().ch, 'B');

        // 0x9B (CSI) & 0x9D/0x9C (OSC/ST).
        let mut t_seq = Terminal::new(10, 4, cfg);
        t_seq.advance(b"\x9D2;c1title\x9C\x9B2;3Hhi");
        assert_eq!(t_seq.title(), "c1title");
        assert_eq!(t_seq.visible_line_text(1), Some("  hi".to_owned()));
    }

    #[test]
    fn block_selection_partial_eviction_preserves_columns() {
        // 1. Scrollback eviction via Terminal::advance.
        let mut term = Terminal::new(
            20,
            4,
            CoreConfig {
                scrollback_limit: 5,
                ..CoreConfig::default()
            },
        );
        for i in 0..9 {
            term.advance(format!("0123456789_{i:04}\r\n").as_bytes());
        }
        term.scroll_display(5);
        term.selection_block(0, 5);
        term.selection_update(2, 8);
        term.selection_finish();
        assert_eq!(term.selection_text(), Some("5678\n5678\n5678".to_owned()));

        // Scroll 1 line -> top selected line is evicted; columns 5..=8 must stay intact.
        term.advance(b"0123456789_0009\r\n");
        assert_eq!(term.selection_text(), Some("5678\n5678".to_owned()));

        // Scroll until entire block selection is evicted -> selection cleared.
        for i in 10..18 {
            term.advance(format!("0123456789_{i:04}\r\n").as_bytes());
        }
        assert_eq!(term.selection_text(), None);
        assert!(term.selection().is_none());

        // 2. Partial eviction via vertical resize shrink (same width fast-path).
        let mut t_same_w = Terminal::new(
            20,
            6,
            CoreConfig {
                scrollback_limit: 2,
                ..CoreConfig::default()
            },
        );
        for i in 0..6 {
            t_same_w.advance(format!("0123456789_{i}").as_bytes());
            if i < 5 {
                t_same_w.advance(b"\r\n");
            }
        }
        t_same_w.selection_block(0, 5);
        t_same_w.selection_update(3, 8);
        t_same_w.selection_finish();
        assert_eq!(
            t_same_w.selection_text(),
            Some("5678\n5678\n5678\n5678".to_owned())
        );
        t_same_w.resize(20, 3);
        assert_eq!(
            t_same_w.selection_text(),
            Some("5678\n5678\n5678".to_owned())
        );

        // 3. Partial eviction via full reflow resize (different width + vertical eviction).
        let mut t_diff_w = Terminal::new(
            20,
            6,
            CoreConfig {
                scrollback_limit: 2,
                ..CoreConfig::default()
            },
        );
        for i in 0..6 {
            t_diff_w.advance(format!("0123456789_{i}").as_bytes());
            if i < 5 {
                t_diff_w.advance(b"\r\n");
            }
        }
        t_diff_w.selection_block(0, 5);
        t_diff_w.selection_update(3, 8);
        t_diff_w.selection_finish();
        t_diff_w.resize(15, 3);
        assert_eq!(
            t_diff_w.selection_text(),
            Some("5678\n5678\n5678".to_owned())
        );
    }

    #[test]
    fn reflow_selection_trailing_whitespace_and_past_eol() {
        // Repro probe6: hard break after trailing blank start ("ab\r\nxyz", Stream (0,5)..(1,2)).
        let mut t6 = new_term(80, 24);
        t6.advance(b"ab\r\nxyz");
        t6.selection_start(0, 5, SelectionMode::Stream);
        t6.selection_update(1, 2);
        t6.selection_finish();
        assert_eq!(t6.selection_text(), Some("\nxyz".to_owned()));
        t6.resize(2, 24);
        assert_eq!(t6.selection_text(), Some("\nxyz".to_owned()));
        assert!(!t6.is_cell_selected(0, 1));
        assert!(t6.is_cell_selected(1, 0));
        t6.resize(1, 24);
        assert_eq!(t6.selection_text(), Some("\nxyz".to_owned()));
        t6.resize(80, 24);
        assert_eq!(t6.selection_text(), Some("\nxyz".to_owned()));

        // Repro probe7: trailing blank selection on same line as Cursor ("abc       ").
        let mut t7 = new_term(80, 24);
        t7.advance(b"abc       ");
        t7.selection_start(0, 7, SelectionMode::Stream);
        t7.selection_update(0, 9);
        t7.selection_finish();
        assert_eq!(t7.selection_text(), Some(String::new()));
        t7.resize(3, 24);
        assert_eq!(t7.selection_text(), Some(String::new()));
        t7.resize(1, 24);
        assert_eq!(t7.selection_text(), Some(String::new()));
        t7.resize(80, 24);
        assert_eq!(t7.selection_text(), Some(String::new()));

        // Repro probe8: Block selection with past-EOL right column across shrink/expand.
        let mut t8 = new_term(20, 5);
        t8.advance(b"0123456789\r\nxy");
        t8.selection_block(0, 5);
        t8.selection_update(1, 15);
        t8.selection_finish();
        assert_eq!(t8.selection_text(), Some("56789\n".to_owned()));
        t8.resize(2, 10);
        // Same-width vertical resize while col=15 is virtual (>2) must also preserve col=15.
        t8.resize(2, 12);
        t8.resize(20, 5);
        assert_eq!(t8.selection_text(), Some("56789\n".to_owned()));

        // Alternate screen resize must not clobber Block columns or panic on past-EOL cols.
        let mut t_alt = new_term(20, 5);
        t_alt.advance(b"\x1b[?1049h0123456789\r\nxy");
        t_alt.selection_block(0, 12);
        t_alt.selection_update(1, 15);
        t_alt.selection_finish();
        assert_eq!(t_alt.selection_text(), Some("\n".to_owned()));
        t_alt.resize(2, 5);
        assert!(!t_alt.is_cell_selected(0, 1));
        assert_eq!(t_alt.selection_text(), Some("\n".to_owned()));
        t_alt.resize(20, 5);
        assert_eq!(t_alt.selection_text(), Some("\n".to_owned()));

        // Randomized multi-step resize seeds (1..=80) verifying Stream & Block selection_text().
        for seed in 1_u64..=80 {
            let mut rng = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(17);
            let mut next_u32 = || {
                rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                (rng >> 32) as u32
            };
            let mut t = Terminal::new(
                80,
                24,
                CoreConfig {
                    scrollback_limit: 500,
                    ..CoreConfig::default()
                },
            );
            let line_count = 3 + (next_u32() % 5);
            for line_i in 0..line_count {
                let len = 1 + (next_u32() % 20);
                let mut line = String::new();
                for _ in 0..len {
                    match next_u32() % 7 {
                        0 => line.push(' '),
                        1 => line.push('中'),
                        2 => line.push_str("\u{2764}\u{FE0F}"),
                        _ => {
                            let ch = char::from(b'a' + u8::try_from(next_u32() % 26).unwrap_or(0));
                            line.push(ch);
                        }
                    }
                }
                t.advance(line.as_bytes());
                if line_i + 1 < line_count {
                    t.advance(b"\r\n");
                }
            }
            let r1 = usize::try_from(next_u32() % line_count).unwrap_or(0);
            let r2 = usize::try_from(next_u32() % line_count).unwrap_or(0);
            let c1 = usize::try_from(next_u32() % 35).unwrap_or(0);
            let c2 = usize::try_from(next_u32() % 35).unwrap_or(0);
            let is_block = seed % 2 == 0;
            if is_block {
                t.selection_block(r1, c1);
            } else {
                t.selection_start(r1, c1, SelectionMode::Stream);
            }
            t.selection_update(r2, c2);
            t.selection_finish();

            let expected_sel = t.selection_text();
            let expected_text = t.full_text();
            let expected_cursor = t.cursor_pos();

            for _ in 0..8 {
                let w = u16::try_from(1 + (next_u32() % 80)).unwrap_or(10);
                let h = u16::try_from(4 + (next_u32() % 24)).unwrap_or(10);
                t.resize(w, h);
                t.grid().debug_verify_wide_invariants();
                t.grid().debug_verify_extra_invariants();
                // Ensure inspection & extraction never panic at any intermediate size.
                drop(t.selection_text());
                let _selected = t.is_cell_selected(0, 0);
            }
            t.resize(80, 24);
            assert_eq!(t.full_text(), expected_text, "seed={seed}: full_text");
            assert_eq!(t.cursor_pos(), expected_cursor, "seed={seed}: cursor_pos");
            assert_eq!(
                t.selection_text(),
                expected_sel,
                "seed={seed}: selection_text (is_block={is_block})"
            );
        }
    }

    #[test]
    fn blank_rows_below_keep_until_row_selection_reflow() {
        // probe11a_1: blank-row Stream selection must not jump onto occupied row 0 on full reflow.
        let mut t1 = new_term(80, 24);
        t1.advance(b"hello");
        t1.selection_start(1, 1, SelectionMode::Stream);
        t1.selection_update(2, 4);
        t1.selection_finish();
        assert_eq!(t1.selection_text(), Some("\n".to_owned()));
        t1.resize(40, 24);
        assert_eq!(t1.selection_text(), Some("\n".to_owned()));
        assert!(!t1.is_cell_selected(0, 1));
        assert!(t1.is_cell_selected(1, 1));
        assert!(t1.is_cell_selected(2, 4));
        // Content wraps to 2 rows (3 cols) -> blank selection shifts below wrapped content, then roundtrips.
        t1.resize(3, 24);
        assert_eq!(t1.selection_text(), Some("\n".to_owned()));
        assert!(!t1.is_cell_selected(0, 1));
        assert!(!t1.is_cell_selected(1, 0));
        t1.resize(80, 24);
        assert_eq!(t1.selection_text(), Some("\n".to_owned()));

        // Shrinking height so blank rows crop down to 1 blank row keeps selection on blank row 1.
        let mut t1_crop = new_term(80, 24);
        t1_crop.advance(b"hello");
        t1_crop.selection_start(1, 1, SelectionMode::Stream);
        t1_crop.selection_update(2, 4);
        t1_crop.selection_finish();
        t1_crop.resize(80, 2);
        assert_eq!(t1_crop.selection_text(), Some(String::new()));
        assert!(!t1_crop.is_cell_selected(0, 1));
        // Shrinking to 1 row leaves zero blank rows -> cropped blank selection clears instead of fabricating "ello".
        t1_crop.resize(80, 1);
        assert_eq!(t1_crop.selection_text(), None);
        assert!(!t1_crop.is_cell_selected(0, 1));

        // probe11a_2: same-width vertical resize must preserve multi-line selection ending on blank row.
        let mut t2 = new_term(80, 24);
        t2.advance(b"hello");
        t2.selection_start(0, 2, SelectionMode::Stream);
        t2.selection_update(2, 4);
        t2.selection_finish();
        assert_eq!(t2.selection_text(), Some("llo\n\n".to_owned()));
        t2.resize(80, 20);
        assert_eq!(t2.selection_text(), Some("llo\n\n".to_owned()));
        assert!(t2.is_cell_selected(0, 2));
        assert!(t2.is_cell_selected(2, 4));
        // Roundtrip through narrower width (3 cols) preserves trailing blank-row count.
        t2.resize(3, 20);
        assert_eq!(t2.selection_text(), Some("llo\n\n".to_owned()));
        t2.resize(80, 24);
        assert_eq!(t2.selection_text(), Some("llo\n\n".to_owned()));

        // Anchor on row 0 + head on blank row 2, shrunk to 1 row -> keeps full remainder of row 0.
        let mut t2_crop = new_term(80, 24);
        t2_crop.advance(b"hello world");
        t2_crop.selection_start(0, 2, SelectionMode::Stream);
        t2_crop.selection_update(2, 4);
        t2_crop.selection_finish();
        t2_crop.resize(80, 1);
        assert_eq!(t2_crop.selection_text(), Some("llo world".to_owned()));

        // probe11a_3: blank-row Block selection must not jump onto occupied row 0 on full reflow.
        let mut t3 = new_term(80, 24);
        t3.advance(b"hello");
        t3.selection_block(1, 1);
        t3.selection_update(2, 4);
        t3.selection_finish();
        assert_eq!(t3.selection_text(), Some("\n".to_owned()));
        t3.resize(40, 24);
        assert_eq!(t3.selection_text(), Some("\n".to_owned()));
        assert!(!t3.is_cell_selected(0, 1));
        assert!(t3.is_cell_selected(1, 1));
        assert!(t3.is_cell_selected(2, 4));
        // Shrinking to 1 row (zero blank rows) clears cropped-off blank Block selection.
        t3.resize(40, 1);
        assert_eq!(t3.selection_text(), None);
        assert!(!t3.is_cell_selected(0, 1));
    }

    #[test]
    fn evicted_word_and_line_origin_drag_preserves_bounds() {
        // probe10b: Word drag across scrollback eviction boundary then back onto (oldest_abs_line, 0).
        let mut term = Terminal::new(
            20,
            4,
            CoreConfig {
                scrollback_limit: 2,
                ..CoreConfig::default()
            },
        );
        for i in 0..5 {
            term.advance(format!("alpha_{i:02} beta_{i:02}\r\n").as_bytes());
        }
        term.selection_start(0, 1, SelectionMode::Word);
        term.selection_update(2, 10);
        term.advance(b"alpha_05 beta_05\r\nalpha_06 beta_06\r\nalpha_07 beta_07\r\n");
        assert_eq!(term.grid().oldest_abs_line(), AbsLine(3));
        term.scroll_display(2);
        term.selection_update(0, 0);
        assert_eq!(term.selection_text(), Some("alpha_03".to_owned()));
        term.selection_update(0, 10);
        assert_eq!(term.selection_text(), Some("alpha_03 beta_03".to_owned()));
        // Resizing after origin_evicted must preserve origin_evicted behavior.
        term.resize(25, 4);
        term.selection_update(0, 0);
        assert_eq!(term.selection_text(), Some("alpha_03".to_owned()));

        // Word drag across resize-induced eviction (6 -> 3 rows with scrollback_limit=1).
        let mut t_resize = Terminal::new(
            20,
            6,
            CoreConfig {
                scrollback_limit: 1,
                ..CoreConfig::default()
            },
        );
        for i in 0..6 {
            t_resize.advance(format!("word_{i:02} tail_{i:02}").as_bytes());
            if i < 5 {
                t_resize.advance(b"\r\n");
            }
        }
        t_resize.selection_start(0, 1, SelectionMode::Word);
        t_resize.selection_update(4, 10);
        t_resize.resize(20, 3);
        assert_eq!(t_resize.grid().oldest_abs_line(), AbsLine(2));
        t_resize.scroll_display(1);
        t_resize.selection_update(0, 0);
        assert_eq!(t_resize.selection_text(), Some("word_02".to_owned()));

        // Line drag across eviction when oldest surviving line is soft-wrapped across 2 physical rows.
        let mut t_line = Terminal::new(
            10,
            4,
            CoreConfig {
                scrollback_limit: 2,
                ..CoreConfig::default()
            },
        );
        t_line.advance(b"l0\r\nl1\r\nl2\r\nwraps_here_row3\r\nl4");
        // Lines 0..=5: l0(0), l1(1), l2(2), wraps_here(3), _row3(4), l4(5); oldest=0, history=2.
        t_line.scroll_display(2);
        t_line.selection_start(2, 0, SelectionMode::Line);
        t_line.scroll_to_bottom();
        t_line.selection_update(3, 0);
        // Advance 3 lines -> oldest becomes 3 ("wraps_here" + "_row3"), origin line 2 is evicted.
        t_line.advance(b"\r\nl5\r\nl6\r\nl7");
        assert_eq!(t_line.grid().oldest_abs_line(), AbsLine(3));
        t_line.scroll_display(2);
        t_line.selection_update(0, 0);
        assert_eq!(t_line.selection_text(), Some("wraps_here_row3".to_owned()));
    }

    #[test]
    fn scrolled_up_selection_consistency_across_resize() {
        for mode in [
            SelectionMode::Stream,
            SelectionMode::Word,
            SelectionMode::Line,
            SelectionMode::Block,
        ] {
            let mut term = Terminal::new(
                20,
                4,
                CoreConfig {
                    scrollback_limit: 20,
                    ..CoreConfig::default()
                },
            );
            for i in 0..10 {
                term.advance(format!("row_{i:02} payload_{i:02}").as_bytes());
                if i < 9 {
                    term.advance(b"\r\n");
                }
            }
            term.scroll_display(4);
            let initial_offset = term.display_offset();
            assert!(initial_offset > 0);

            match mode {
                SelectionMode::Block => term.selection_block(0, 2),
                _ => term.selection_start(0, 2, mode),
            }
            term.selection_update(2, 11);
            term.selection_finish();
            let expected_text = term.selection_text();
            assert!(expected_text.is_some());

            // Shrink width -> reflows scrollback & viewport while preserving display_offset > 0 and selection.
            term.resize(10, 4);
            assert!(term.display_offset() > 0);
            if mode != SelectionMode::Block {
                assert_eq!(
                    term.selection_text(),
                    expected_text,
                    "mode={mode:?} at 10x4"
                );
            }
            let snap = term.active_snapshot();
            for r in 0..4 {
                for c in 0..10 {
                    assert_eq!(
                        term.is_cell_selected(r, c),
                        snap.is_cell_selected(r, c),
                        "mode={mode:?} r={r} c={c}"
                    );
                }
            }

            // Expand back to 20x4 -> exact selection text & scrolled-up highlight consistency.
            term.resize(20, 4);
            assert_eq!(term.display_offset(), initial_offset);
            assert_eq!(
                term.selection_text(),
                expected_text,
                "mode={mode:?} back at 20x4"
            );
        }
    }

    #[test]
    fn blank_row_crop_with_empty_cursor_row_and_empty_terminal() {
        // probe14b & Probe2a: "hello\r\n" leaves empty cursor row 1; Stream on blank rows 2..=3 clears.
        for (w, h) in [(80, 2), (40, 2)] {
            let mut t = new_term(80, 24);
            t.advance(b"hello\r\n");
            t.selection_start(2, 1, SelectionMode::Stream);
            t.selection_update(3, 4);
            t.selection_finish();
            t.resize(w, h);
            assert_eq!(t.selection_text(), None, "Stream {w}x{h}");
            assert!(!t.is_cell_selected(1, 2), "Stream {w}x{h} row 1 col 2");
            t.advance(b"world");
            assert_eq!(t.selection_text(), None, "Stream {w}x{h} after world");
        }

        // probe14c: "hello\r\n" Block on blank rows 2..=3 clears on vertical crop.
        for (w, h) in [(80, 2), (40, 2)] {
            let mut t = new_term(80, 24);
            t.advance(b"hello\r\n");
            t.selection_block(2, 1);
            t.selection_update(3, 4);
            t.selection_finish();
            t.resize(w, h);
            assert_eq!(t.selection_text(), None, "Block {w}x{h}");
            assert!(!t.is_cell_selected(1, 2), "Block {w}x{h} row 1 col 2");
            t.advance(b"world");
            assert_eq!(t.selection_text(), None, "Block {w}x{h} after world");
        }

        // probe14d: "hello\r\n" crossing Stream (0,2)..=(2,4) & backwards (2,4)..=(0,2) keeps full row 1.
        for reverse in [false, true] {
            for (w, h) in [(80, 2), (40, 2)] {
                let mut t = new_term(80, 24);
                t.advance(b"hello\r\n");
                if reverse {
                    t.selection_start(2, 4, SelectionMode::Stream);
                    t.selection_update(0, 2);
                } else {
                    t.selection_start(0, 2, SelectionMode::Stream);
                    t.selection_update(2, 4);
                }
                t.selection_finish();
                t.resize(w, h);
                assert!(
                    t.is_cell_selected(1, 10),
                    "crossing reverse={reverse} {w}x{h}: row 1 col 10 must be selected"
                );
                assert_eq!(t.selection_text(), Some("llo\n".to_owned()));
            }
        }

        // Probe2b: completely empty terminal 80x4 -> 80x1 / 40x1 clears blank-row Stream & Block.
        for is_block in [false, true] {
            for (w, h) in [(80, 1), (40, 1)] {
                let mut t = new_term(80, 4);
                if is_block {
                    t.selection_block(1, 1);
                } else {
                    t.selection_start(1, 1, SelectionMode::Stream);
                }
                t.selection_update(2, 4);
                t.selection_finish();
                t.resize(w, h);
                assert_eq!(
                    t.selection_text(),
                    None,
                    "empty is_block={is_block} {w}x{h}"
                );
                assert!(!t.is_cell_selected(0, 2));
            }
        }
    }

    #[test]
    fn cropped_blank_origin_word_and_wrapped_line_drag() {
        // probe18a: Word started on blank row (2, 4), dragged UP to (0, 1), shrunk to 80x1, then updated.
        let mut t = new_term(80, 24);
        t.advance(b"hello world");
        t.selection_start(2, 4, SelectionMode::Word);
        t.selection_update(0, 1);
        assert_eq!(t.selection_text(), Some("hello world\n\n".to_owned()));
        t.resize(80, 1);
        assert_eq!(t.selection_text(), Some("hello world".to_owned()));
        t.selection_update(0, 1);
        assert_eq!(t.selection_text(), Some("hello world".to_owned()));
        t.selection_update(0, 7);
        assert_eq!(t.selection_text(), Some("world".to_owned()));
        // Subsequent vertical growth keeps origin_cropped_bottom & virtual boundary intact.
        t.resize(80, 5);
        t.selection_update(0, 1);
        assert_eq!(t.selection_text(), Some("hello world".to_owned()));
        t.selection_update(2, 3);
        assert_eq!(t.selection_text(), Some("\n".to_owned()));

        // Reviewer 2 Probe1a: "hello_w" across 80x1, 40x1, and wrapped 5x2.
        let mut t1a = new_term(80, 24);
        t1a.advance(b"hello_w");
        t1a.selection_start(2, 3, SelectionMode::Word);
        t1a.selection_update(0, 1);
        assert_eq!(t1a.selection_text(), Some("hello_w\n\n".to_owned()));
        t1a.resize(80, 1);
        assert_eq!(t1a.selection_text(), Some("hello_w".to_owned()));
        t1a.selection_update(0, 1);
        assert_eq!(t1a.selection_text(), Some("hello_w".to_owned()));
        t1a.selection_update(0, 5);
        assert_eq!(t1a.selection_text(), Some("hello_w".to_owned()));
        t1a.resize(5, 2);
        t1a.selection_update(1, 0);
        assert_eq!(t1a.selection_text(), Some("hello_w".to_owned()));

        // Reviewer 2 Probe1b: "ab cd" -> update(0, 3) selects "cd", never expanding to "ab cd".
        let mut t1b = new_term(80, 24);
        t1b.advance(b"ab cd");
        t1b.selection_start(2, 0, SelectionMode::Word);
        t1b.selection_update(0, 1);
        assert_eq!(t1b.selection_text(), Some("ab cd\n\n".to_owned()));
        t1b.resize(80, 1);
        assert_eq!(t1b.selection_text(), Some("ab cd".to_owned()));
        t1b.selection_update(0, 3);
        assert_eq!(t1b.selection_text(), Some("cd".to_owned()));

        // Soft-wrapped Line selection started on blank row -> dragged onto wrapped line -> cropped -> updated.
        let mut t_line = new_term(10, 24);
        t_line.advance(b"hello_world_long");
        t_line.selection_start(3, 2, SelectionMode::Line);
        t_line.selection_update(1, 2);
        assert_eq!(
            t_line.selection_text(),
            Some("hello_world_long\n\n".to_owned())
        );
        t_line.resize(10, 2);
        assert_eq!(t_line.selection_text(), Some("hello_world_long".to_owned()));
        t_line.selection_update(1, 2);
        assert_eq!(t_line.selection_text(), Some("hello_world_long".to_owned()));
        t_line.selection_update(0, 2);
        assert_eq!(t_line.selection_text(), Some("hello_world_long".to_owned()));
        // Grow taller and drag below last_kept -> does not falsely include wrapped line above origin.
        t_line.resize(10, 5);
        t_line.selection_update(1, 2);
        assert_eq!(t_line.selection_text(), Some("hello_world_long".to_owned()));
        t_line.selection_update(3, 2);
        assert!(!t_line.is_cell_selected(1, 0));
        assert!(t_line.is_cell_selected(2, 0));
        assert!(t_line.is_cell_selected(3, 0));
    }

    #[test]
    fn alt_screen_vertical_crop_selection_eviction_and_block() {
        // probe18b: alt-screen top crop must clear destroyed top selection instead of hijacking L3.
        let mut t_top = new_term(20, 5);
        t_top.advance(b"\x1b[?1049hL0\r\nL1\r\nL2\r\nL3\r\nL4");
        t_top.selection_start(0, 0, SelectionMode::Stream);
        t_top.selection_update(0, 1);
        t_top.selection_finish();
        assert_eq!(t_top.selection_text(), Some("L0".to_owned()));
        t_top.resize(20, 2);
        assert_eq!(t_top.selection_text(), None);
        assert!(t_top.selection().is_none());

        // Partial top crop in Stream mode clamps top endpoint to (0, 0).
        let mut t_partial = new_term(20, 5);
        t_partial.advance(b"\x1b[?1049hL0\r\nL1\r\nL2\r\nL3\r\nL4");
        t_partial.selection_start(1, 1, SelectionMode::Stream);
        t_partial.selection_update(3, 1);
        t_partial.selection_finish();
        t_partial.resize(20, 2);
        assert_eq!(t_partial.selection_text(), Some("L3".to_owned()));

        // Partial top crop in Block mode preserves anchor column.
        let mut t_block = new_term(20, 5);
        t_block.advance(b"\x1b[?1049hL0\r\nL1\r\nL2\r\nL3\r\nL4");
        t_block.selection_block(1, 1);
        t_block.selection_update(4, 1);
        t_block.selection_finish();
        t_block.resize(20, 2);
        assert_eq!(t_block.selection_text(), Some("3\n4".to_owned()));

        // Alt-screen bottom blank-row crop clears blank selection & preserves crossing tail selection.
        for is_block in [false, true] {
            let mut t_bot = new_term(20, 5);
            t_bot.advance(b"\x1b[?1049hL0\r\nL1");
            if is_block {
                t_bot.selection_block(2, 1);
            } else {
                t_bot.selection_start(2, 1, SelectionMode::Stream);
            }
            t_bot.selection_update(3, 4);
            t_bot.selection_finish();
            t_bot.resize(20, 2);
            assert_eq!(t_bot.selection_text(), None);
            assert!(!t_bot.is_cell_selected(1, 2));
        }

        let mut t_cross = new_term(20, 5);
        t_cross.advance(b"\x1b[?1049hL0\r\nL1");
        t_cross.selection_start(0, 1, SelectionMode::Stream);
        t_cross.selection_update(3, 4);
        t_cross.selection_finish();
        t_cross.resize(20, 2);
        assert!(t_cross.is_cell_selected(1, 10));
        assert_eq!(t_cross.selection_text(), Some("0\nL1".to_owned()));

        // Alt-screen Word started on cropped blank row -> drag UP -> crop -> selection_update.
        let mut t_alt_word = new_term(20, 5);
        t_alt_word.advance(b"\x1b[?1049haa bb");
        t_alt_word.selection_start(3, 4, SelectionMode::Word);
        t_alt_word.selection_update(0, 1);
        t_alt_word.resize(20, 2);
        t_alt_word.selection_update(0, 1);
        assert_eq!(t_alt_word.selection_text(), Some("aa bb\n".to_owned()));
        t_alt_word.selection_update(0, 3);
        assert_eq!(t_alt_word.selection_text(), Some("bb\n".to_owned()));

        // Alt-screen Word top eviction sets origin_evicted and preserves (0, 0) drag anchor.
        let mut t_alt_evict = new_term(20, 5);
        t_alt_evict.advance(b"\x1b[?1049haa bb\r\ncc dd\r\nee ff\r\ngg hh\r\nii jj");
        t_alt_evict.selection_start(0, 0, SelectionMode::Word);
        t_alt_evict.selection_update(4, 1);
        t_alt_evict.resize(20, 2);
        t_alt_evict.selection_update(0, 1);
        assert_eq!(t_alt_evict.selection_text(), Some("gg".to_owned()));
        t_alt_evict.selection_update(1, 4);
        assert_eq!(
            t_alt_evict.selection_text(),
            Some("gg hh\nii jj".to_owned())
        );
    }

    #[test]
    fn cropped_blank_forward_drag_and_re_shrink_probes() {
        // probe19: Line cropped blank drag onto wrapped text -> shrink -> grow -> forward drag.
        let mut t19 = new_term(10, 6);
        t19.advance(b"0123456789abcd");
        t19.selection_start(2, 4, SelectionMode::Line);
        t19.selection_update(0, 1);
        t19.resize(10, 2);
        assert_eq!(t19.selection_text(), Some("0123456789abcd".to_owned()));
        t19.selection_update(1, 0);
        assert_eq!(t19.selection_text(), Some("0123456789abcd".to_owned()));
        t19.selection_update(0, 1);
        assert_eq!(t19.selection_text(), Some("0123456789abcd".to_owned()));
        t19.resize(10, 6);
        t19.selection_update(3, 2);
        assert!(!t19.is_cell_selected(0, 0));
        assert!(!t19.is_cell_selected(1, 0));
        assert_eq!(t19.selection_text(), Some("\n".to_owned()));
        t19.advance(b"\r\nnextline");
        t19.selection_update(2, 2);
        assert_eq!(t19.selection_text(), Some("nextline".to_owned()));

        // probe20: Word cropped blank -> shrink -> grow -> "\r\nalpha beta" -> forward drag.
        let mut t20 = new_term(80, 24);
        t20.advance(b"hello world");
        t20.selection_start(2, 4, SelectionMode::Word);
        t20.selection_update(0, 1);
        t20.resize(80, 1);
        t20.resize(80, 5);
        t20.advance(b"\r\nalpha beta");
        t20.selection_update(1, 1);
        assert!(!t20.is_cell_selected(0, 0));
        assert_eq!(t20.selection_text(), Some("alpha".to_owned()));
        // Dragging back up onto row 0 restores cropped-bottom origin behavior.
        t20.selection_update(0, 1);
        assert_eq!(t20.selection_text(), Some("hello world".to_owned()));

        // probe21: Alt-screen Line cropped blank -> shrink -> grow -> forward drag.
        let mut t21 = new_term(20, 5);
        t21.advance(b"\x1b[?1049hL0\r\nL1");
        t21.selection_start(3, 4, SelectionMode::Line);
        t21.selection_update(0, 1);
        t21.resize(20, 2);
        assert_eq!(t21.selection_text(), Some("L0\nL1".to_owned()));
        t21.resize(20, 5);
        t21.selection_update(3, 4);
        assert!(!t21.is_cell_selected(1, 0));
        assert_eq!(t21.selection_text(), Some("\n".to_owned()));

        // probe24: Word cropped blank -> shrink -> grow -> forward drag -> re-shrink clears to None.
        let mut t24 = new_term(80, 24);
        t24.advance(b"hello world");
        t24.selection_start(2, 4, SelectionMode::Word);
        t24.selection_update(0, 1);
        t24.resize(80, 1);
        t24.resize(80, 5);
        t24.selection_update(2, 3);
        assert!(!t24.is_cell_selected(0, 0));
        assert_eq!(t24.selection_text(), Some("\n".to_owned()));
        t24.resize(80, 1);
        assert_eq!(t24.selection_text(), None);
        assert!(t24.selection().is_none());

        // Reviewer 2: Line crop -> grow -> drag-down -> re-crop clears without false row 0 highlight.
        let mut t_r2 = new_term(20, 5);
        t_r2.advance(b"hello");
        t_r2.selection_start(2, 0, SelectionMode::Line);
        t_r2.selection_update(0, 0);
        t_r2.resize(20, 1);
        t_r2.resize(20, 5);
        t_r2.selection_update(2, 0);
        assert!(!t_r2.is_cell_selected(0, 0));
        assert_eq!(t_r2.selection_text(), Some("\n".to_owned()));
        t_r2.resize(20, 1);
        assert!(!t_r2.is_cell_selected(0, 0));
        assert_eq!(t_r2.selection_text(), None);
        assert!(t_r2.selection().is_none());

        // Direct Selection::row_col_range check when start.line == end.line and start_past_eol == true.
        let mut past_eol_line = Selection::new(
            SelectionMode::Line,
            GridPoint::new(AbsLine(0), 20),
            GridPoint::new(AbsLine(0), 20),
        );
        past_eol_line.anchor_past_eol = true;
        past_eol_line.head_past_eol = true;
        assert_eq!(
            past_eol_line.row_col_range(AbsLine(0), t_r2.grid().visible_row(0).unwrap()),
            None
        );
        assert_eq!(selection::extract_text(t_r2.grid(), &past_eol_line), None);
    }

    #[test]
    fn widen_and_narrow_wrap_after_bottom_crop_matrix() {
        // Primary Line widen matrix: 80x24->80x1->120x5 / 200x3, and 80x24->40x1->80x24.
        for (mid_w, end_w, end_h) in [(80, 120, 5), (80, 200, 3), (40, 80, 24)] {
            let mut t = new_term(80, 24);
            t.advance(b"hello_world");
            t.selection_start(2, 4, SelectionMode::Line);
            t.selection_update(0, 1);
            t.resize(mid_w, 1);
            t.resize(end_w, end_h);
            t.selection_update(2, 3);
            assert!(!t.is_cell_selected(0, 0), "{mid_w}->{end_w}x{end_h}");
            assert_eq!(t.selection_text(), Some("\n".to_owned()));
        }

        // Primary Word widen: 80x24 -> 80x1 -> 200x3 (drag down AND drag to (0, 90)).
        let mut tw_down = new_term(80, 24);
        tw_down.advance(b"hello world");
        tw_down.selection_start(2, 4, SelectionMode::Word);
        tw_down.selection_update(0, 1);
        tw_down.resize(80, 1);
        tw_down.resize(200, 3);
        tw_down.selection_update(2, 3);
        assert!(!tw_down.is_cell_selected(0, 100));
        assert_eq!(tw_down.selection_text(), Some("\n".to_owned()));

        let mut tw_row0 = new_term(80, 24);
        tw_row0.advance(b"hello world");
        tw_row0.selection_start(2, 4, SelectionMode::Word);
        tw_row0.selection_update(0, 1);
        tw_row0.resize(80, 1);
        tw_row0.resize(200, 3);
        tw_row0.selection_update(0, 90);
        assert!(tw_row0.is_cell_selected(0, 150));
        assert!(!tw_row0.is_cell_selected(0, 89));

        // Primary Stream crossing tail widened: 80x24 -> 80x2 -> 200x2 (probe14d widened).
        let mut ts = new_term(80, 24);
        ts.advance(b"hello\r\n");
        ts.selection_start(0, 2, SelectionMode::Stream);
        ts.selection_update(2, 4);
        ts.selection_finish();
        ts.resize(80, 2);
        ts.resize(200, 2);
        assert!(ts.is_cell_selected(1, 150));
        assert_eq!(ts.selection_text(), Some("llo\n".to_owned()));

        // Alt-screen Line & Word widen after bottom crop.
        let mut ta_line = new_term(80, 5);
        ta_line.advance(b"\x1b[?1049hL0\r\nL1");
        ta_line.selection_start(3, 4, SelectionMode::Line);
        ta_line.selection_update(0, 1);
        ta_line.resize(80, 2);
        ta_line.resize(120, 5);
        ta_line.selection_update(3, 2);
        assert!(!ta_line.is_cell_selected(1, 0));
        assert_eq!(ta_line.selection_text(), Some("\n".to_owned()));

        let mut ta_word = new_term(20, 5);
        ta_word.advance(b"\x1b[?1049haa bb");
        ta_word.selection_start(3, 4, SelectionMode::Word);
        ta_word.selection_update(0, 1);
        ta_word.resize(20, 2);
        ta_word.resize(50, 4);
        ta_word.selection_update(3, 1);
        assert!(!ta_word.is_cell_selected(1, 30));
        ta_word.selection_update(1, 30);
        assert!(ta_word.is_cell_selected(1, 45));
        assert!(!ta_word.is_cell_selected(1, 29));

        // Reviewer 1: Narrow with content wrap (50 * 'a') across 80x24 -> 80x1 -> 45x2 -> 45x5.
        let payload_50a = "a".repeat(50);
        let mut t50_line = new_term(80, 24);
        t50_line.advance(payload_50a.as_bytes());
        t50_line.selection_start(2, 4, SelectionMode::Line);
        t50_line.selection_update(0, 1);
        t50_line.resize(80, 1);
        t50_line.resize(45, 2);
        assert_eq!(t50_line.selection_text(), Some(payload_50a.clone()));
        t50_line.resize(45, 5);
        t50_line.selection_update(3, 0);
        assert!(!t50_line.is_cell_selected(0, 0));
        assert!(!t50_line.is_cell_selected(1, 0));
        assert_eq!(t50_line.selection_text(), Some("\n".to_owned()));

        let mut t50_stream = new_term(80, 24);
        t50_stream.advance(payload_50a.as_bytes());
        t50_stream.selection_start(0, 2, SelectionMode::Stream);
        t50_stream.selection_update(2, 4);
        t50_stream.resize(80, 1);
        t50_stream.resize(45, 2);
        assert!(t50_stream.is_cell_selected(1, 44));
        assert_eq!(t50_stream.selection_text(), Some("a".repeat(48)));

        // Reviewer 1: Narrow with cursor wrap ("hello\x1b[1;51H") across 80x24 -> 80x1 -> 45x2 -> 45x5.
        let mut t_cur = new_term(80, 24);
        t_cur.advance(b"hello\x1b[1;51H");
        t_cur.selection_start(2, 4, SelectionMode::Line);
        t_cur.selection_update(0, 1);
        t_cur.resize(80, 1);
        t_cur.resize(45, 2);
        t_cur.resize(45, 5);
        t_cur.selection_update(3, 0);
        assert!(!t_cur.is_cell_selected(0, 0));
        assert!(!t_cur.is_cell_selected(1, 0));
        assert_eq!(t_cur.selection_text(), Some("\n".to_owned()));
        t_cur.selection_update(0, 1);
        assert!(t_cur.is_cell_selected(0, 0));
        assert!(t_cur.is_cell_selected(1, 44));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_bounds_term() {
        use serde::Deserialize as _;
        use serde::de::{
            Deserializer, Error as _, IntoDeserializer, Visitor,
            value::{Error as DeError, MapDeserializer},
        };

        const fn assert_serde<T: serde::Serialize + for<'de> serde::Deserialize<'de>>() {}
        assert_serde::<CoreConfig>();
        assert_serde::<TerminalEvent>();
        assert_serde::<ClipboardSelection>();
        assert_serde::<CursorShape>();
        assert_serde::<CursorStyle>();
        assert_serde::<Modes>();
        assert_serde::<MouseProtocol>();
        assert_serde::<MouseEncoding>();
        assert_serde::<Selection>();
        assert_serde::<CellSnapshot>();

        #[derive(Clone, Copy)]
        enum LegacyVal {
            Str(&'static str),
            Bool(bool),
            U64(u64),
            Point(u64, usize),
        }

        impl<'de> Deserializer<'de> for LegacyVal {
            type Error = DeError;

            fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                match self {
                    Self::Str(s) => visitor.visit_str(s),
                    Self::Bool(b) => visitor.visit_bool(b),
                    Self::U64(n) => visitor.visit_u64(n),
                    Self::Point(line, col) => {
                        let map = MapDeserializer::new(
                            [("line", Self::U64(line)), ("col", Self::U64(col as u64))].into_iter(),
                        );
                        visitor.visit_map(map)
                    }
                }
            }

            fn deserialize_newtype_struct<V: Visitor<'de>>(
                self,
                _name: &'static str,
                visitor: V,
            ) -> Result<V::Value, Self::Error> {
                visitor.visit_newtype_struct(self)
            }

            fn deserialize_enum<V: Visitor<'de>>(
                self,
                _name: &'static str,
                _variants: &'static [&'static str],
                visitor: V,
            ) -> Result<V::Value, Self::Error> {
                match self {
                    Self::Str(s) => visitor.visit_enum(s.into_deserializer()),
                    _ => Err(DeError::custom("expected enum variant string")),
                }
            }

            serde::forward_to_deserialize_any! {
                bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
                bytes byte_buf option unit unit_struct seq tuple
                tuple_struct map struct identifier ignored_any
            }
        }

        impl<'de> IntoDeserializer<'de, DeError> for LegacyVal {
            type Deserializer = Self;
            fn into_deserializer(self) -> Self::Deserializer {
                self
            }
        }

        // Legacy payload omitting `origin_evicted`, `origin_cropped_bottom`, and `*_past_eol` fields.
        let legacy_de = MapDeserializer::<_, DeError>::new(
            [
                ("mode", LegacyVal::Str("Line")),
                ("anchor", LegacyVal::Point(1, 0)),
                ("head", LegacyVal::Point(2, 19)),
                ("finished", LegacyVal::Bool(false)),
                ("origin_start", LegacyVal::Point(1, 0)),
                ("origin_end", LegacyVal::Point(1, 19)),
            ]
            .into_iter(),
        );
        let legacy_sel = Selection::deserialize(legacy_de).expect("legacy selection deserializes");
        assert_eq!(legacy_sel.mode, SelectionMode::Line);
        assert_eq!(legacy_sel.anchor, GridPoint::new(AbsLine(1), 0));
        assert_eq!(legacy_sel.head, GridPoint::new(AbsLine(2), 19));
        assert!(!legacy_sel.origin_evicted);
        assert!(!legacy_sel.origin_cropped_bottom);
        assert!(!legacy_sel.anchor_past_eol);
        assert!(!legacy_sel.head_past_eol);
        assert!(!legacy_sel.origin_start_past_eol);
        assert!(!legacy_sel.origin_end_past_eol);

        // Full payload with `origin_cropped_bottom` and `*_past_eol` flags preserves all flags.
        let full_de = MapDeserializer::<_, DeError>::new(
            [
                ("mode", LegacyVal::Str("Word")),
                ("anchor", LegacyVal::Point(0, 80)),
                ("head", LegacyVal::Point(0, 0)),
                ("finished", LegacyVal::Bool(false)),
                ("origin_start", LegacyVal::Point(0, 80)),
                ("origin_end", LegacyVal::Point(0, 80)),
                ("origin_evicted", LegacyVal::Bool(false)),
                ("origin_cropped_bottom", LegacyVal::Bool(true)),
                ("anchor_past_eol", LegacyVal::Bool(true)),
                ("head_past_eol", LegacyVal::Bool(false)),
                ("origin_start_past_eol", LegacyVal::Bool(true)),
                ("origin_end_past_eol", LegacyVal::Bool(true)),
            ]
            .into_iter(),
        );
        let full_sel = Selection::deserialize(full_de).expect("full selection deserializes");
        assert!(full_sel.origin_cropped_bottom);
        assert!(full_sel.anchor_past_eol);
        assert!(!full_sel.head_past_eol);
        assert!(full_sel.origin_start_past_eol);
        assert!(full_sel.origin_end_past_eol);
    }

    #[test]
    fn has_blank_tail_crop_on_partial_shrink_and_past_cols_bottom_crop() {
        // probe_has_blank_tail_crop_on_partial_shrink Cases A, B, C, D across 80x5 & 40x5.
        for (new_w, new_h) in [(80, 5), (40, 5)] {
            // Case A: cropped blank Stream (10,2)..=(12,4) clears to None.
            let mut ta = new_term(80, 24);
            ta.advance(b"hello");
            ta.selection_start(10, 2, SelectionMode::Stream);
            ta.selection_update(12, 4);
            ta.selection_finish();
            ta.resize(new_w, new_h);
            assert_eq!(ta.selection_text(), None, "Case A {new_w}x{new_h}");
            assert!(ta.selection().is_none(), "Case A sel {new_w}x{new_h}");
            assert!(!ta.is_cell_selected(4, 3), "Case A cell {new_w}x{new_h}");

            // Case B: crossing Stream (0,2)..=(10,4) preserves past_eol tail on row 4.
            let mut tb = new_term(80, 24);
            tb.advance(b"hello");
            tb.selection_start(0, 2, SelectionMode::Stream);
            tb.selection_update(10, 4);
            tb.selection_finish();
            tb.resize(new_w, new_h);
            assert!(
                tb.is_cell_selected(4, 10),
                "Case B cell(4,10) {new_w}x{new_h}"
            );
            assert!(
                tb.is_cell_selected(4, usize::from(new_w) - 1),
                "Case B last col {new_w}x{new_h}"
            );
            assert_eq!(tb.selection_text(), Some("llo\n\n\n\n".to_owned()));

            // Case C: cropped blank Block (10,2)..=(12,4) clears to None.
            let mut tc = new_term(80, 24);
            tc.advance(b"hello");
            tc.selection_block(10, 2);
            tc.selection_update(12, 4);
            tc.selection_finish();
            tc.resize(new_w, new_h);
            assert_eq!(tc.selection_text(), None, "Case C {new_w}x{new_h}");
            assert!(tc.selection().is_none(), "Case C sel {new_w}x{new_h}");
            assert!(!tc.is_cell_selected(4, 3), "Case C cell {new_w}x{new_h}");

            // Case D: Word & Line started on off-bottom blank row 10, dragged to (0,1),
            // shrunk to (new_w, 5), grown back to 80x24, then forward-dragged to (10, 4).
            for mode in [SelectionMode::Word, SelectionMode::Line] {
                let mut td = new_term(80, 24);
                td.advance(b"hello world");
                td.selection_start(10, 4, mode);
                td.selection_update(0, 1);
                td.resize(new_w, new_h);
                assert!(
                    td.is_cell_selected(4, 10),
                    "Case D {mode:?} {new_w}x{new_h}: row 4 col 10 must be selected"
                );
                assert!(
                    td.is_cell_selected(4, usize::from(new_w) - 1),
                    "Case D {mode:?} {new_w}x{new_h}: row 4 last col must be selected"
                );
                assert_eq!(td.selection_text(), Some("hello world\n\n\n\n".to_owned()));

                td.resize(80, 24);
                td.selection_update(10, 4);
                assert!(
                    !td.is_cell_selected(4, 0),
                    "Case D {mode:?} {new_w}x{new_h}: forward drag after grow must not select row 4"
                );
                assert!(
                    td.is_cell_selected(5, 0),
                    "Case D {mode:?} {new_w}x{new_h}: forward drag starts at row 5"
                );
                assert_eq!(td.selection_text(), Some("\n\n\n\n\n".to_owned()));
            }
        }

        // probe25a: 80x24 -> 80x5 / 40x5, Stream/Word/Line/Block strictly on off-bottom rows 10..=15 -> None.
        for (new_w, new_h) in [(80, 5), (40, 5)] {
            for mode in [
                SelectionMode::Stream,
                SelectionMode::Word,
                SelectionMode::Line,
                SelectionMode::Block,
            ] {
                let mut t = new_term(80, 24);
                t.advance(b"hello");
                t.selection_start(10, 2, mode);
                t.selection_update(15, 8);
                t.selection_finish();
                t.resize(new_w, new_h);
                assert_eq!(
                    t.selection_text(),
                    None,
                    "probe25a {mode:?} {new_w}x{new_h}"
                );
                assert!(t.selection().is_none(), "probe25a {mode:?} {new_w}x{new_h}");
                assert!(
                    !t.is_cell_selected(4, 2),
                    "probe25a {mode:?} {new_w}x{new_h}"
                );
            }
        }

        // probe25b: 80x24 -> 80x5 / 40x5, Word/Line (10, 5) -> (2, 1) -> row 4 last col selected.
        for (new_w, new_h) in [(80, 5), (40, 5)] {
            for mode in [SelectionMode::Word, SelectionMode::Line] {
                let mut t = new_term(80, 24);
                t.advance(b"hello");
                t.selection_start(10, 5, mode);
                t.selection_update(2, 1);
                t.resize(new_w, new_h);
                assert!(
                    t.is_cell_selected(4, usize::from(new_w) - 1),
                    "probe25b {mode:?} {new_w}x{new_h}: row 4 last col must be selected"
                );
                assert!(
                    t.is_cell_selected(3, 0),
                    "probe25b {mode:?} {new_w}x{new_h}: row 3 must be selected"
                );
                assert_eq!(t.selection_text(), Some("\n\n".to_owned()));
            }
        }

        // probe26a: primary 80x24 -> 20x1, "hello\x1b[1;51H", Stream (0, 60)..(2, 10) -> None.
        let mut t26a = new_term(80, 24);
        t26a.advance(b"hello\x1b[1;51H");
        t26a.selection_start(0, 60, SelectionMode::Stream);
        t26a.selection_update(2, 10);
        t26a.selection_finish();
        t26a.resize(20, 1);
        assert_eq!(t26a.selection_text(), None);
        assert!(t26a.selection().is_none());
        assert!(!t26a.is_cell_selected(0, 0));

        // probe26b: alt-screen 20x5 -> 10x1, "hello", Stream (0, 15)..(3, 5) -> None
        // and reverse Word/Line (3, 5) -> (0, 15) -> None.
        let mut t26b = new_term(20, 5);
        t26b.advance(b"\x1b[?1049hhello");
        t26b.selection_start(0, 15, SelectionMode::Stream);
        t26b.selection_update(3, 5);
        t26b.selection_finish();
        t26b.resize(10, 1);
        assert_eq!(t26b.selection_text(), None);
        assert!(t26b.selection().is_none());
        assert!(!t26b.is_cell_selected(0, 0));

        let mut t26b_rev_word = new_term(20, 5);
        t26b_rev_word.advance(b"\x1b[?1049hhello");
        t26b_rev_word.selection_start(3, 5, SelectionMode::Word);
        t26b_rev_word.selection_update(0, 15);
        t26b_rev_word.resize(10, 1);
        assert_eq!(t26b_rev_word.selection_text(), None, "probe26b rev Word");
        assert!(t26b_rev_word.selection().is_none(), "probe26b rev Word");

        let mut t26b_rev_line = new_term(20, 5);
        t26b_rev_line.advance(b"\x1b[?1049hhello");
        t26b_rev_line.selection_start(3, 5, SelectionMode::Line);
        t26b_rev_line.selection_update(0, 15);
        t26b_rev_line.resize(10, 1);
        // Line mode snaps `head` to column 0, so the entire row 0 remains selected.
        assert_eq!(
            t26b_rev_line.selection_text(),
            Some("hello".to_owned()),
            "probe26b rev Line"
        );
    }

    #[test]
    fn grid_content_epoch_scroll_invariant_vs_mutations() {
        let mut term = new_term(20, 3);
        for i in 0..8 {
            term.advance(format!("line{i}\r\n").as_bytes());
        }
        let epoch_before_scroll = term.grid().content_epoch();
        let dmg_before_scroll = term.grid().damage_epoch();

        term.scroll_display(3);
        assert_eq!(term.grid().content_epoch(), epoch_before_scroll);
        assert!(term.grid().damage_epoch() > dmg_before_scroll);

        term.scroll_to_bottom();
        assert_eq!(term.grid().content_epoch(), epoch_before_scroll);

        term.advance(b"edit");
        assert!(term.grid().content_epoch() > epoch_before_scroll);

        let epoch_before_resize = term.grid().content_epoch();
        term.resize(15, 3);
        assert!(term.grid().content_epoch() > epoch_before_resize);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn snapshot_export_restore_and_defensive_sanitization() {
        const fn assert_serde<T: serde::Serialize + for<'de> serde::Deserialize<'de>>() {}
        assert_serde::<TerminalSnapshot>();
        assert_serde::<CoreConfig>();

        let mut term = new_term(20, 4);
        term.advance(
            "👨‍👩‍👧‍👦 e\u{0301} \x1b[4:3;58:2::255:0:0merr\x1b[0m \x1b]4;200;rgb:10/20/30\x07".as_bytes(),
        );
        let mut snap = term.export_snapshot();
        assert_eq!(snap.cols, 20);
        assert_eq!(snap.rows, 4);
        assert!(snap.extended_palette.contains(&(200, [0x10, 0x20, 0x30])));

        // Round-trip restores exact cells, extras, cursor, and extended palette.
        let restored = Terminal::from_snapshot(snap.clone(), CoreConfig::default());
        assert_eq!(restored.size(), (20, 4));
        assert_eq!(restored.cell_at(0, 0).unwrap().grapheme, "👨‍👩‍👧‍👦");
        assert_eq!(restored.cell_at(3, 0).unwrap().grapheme, "e\u{0301}");
        let err = restored.cell_at(5, 0).unwrap();
        assert_eq!(err.underline_style, UnderlineStyle::Curly);
        assert_eq!(err.underline_color, Some(PackedColor::rgb(255, 0, 0)));
        restored.grid().debug_verify_extra_invariants();
        restored.grid().debug_verify_wide_invariants();

        // Tampered snapshot (out-of-bounds cursor/margins, orphan WIDE_SPACER/WRAP_SPACER,
        // dangling extra_id, excess scrollback) sanitizes cleanly without panics.
        snap.cursor.row = 999;
        snap.cursor.col = 999;
        snap.scroll_top = 10;
        snap.scroll_bottom = 2;
        snap.modes.sync_output = true;
        let mut bad_row = Row::new(5, 0);
        bad_row.cells_mut()[0] = Cell::new(
            ' ',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::WIDE_SPACER,
        );
        bad_row.cells_mut()[0].extra_id = 999_999;
        bad_row.cells_mut()[4] = Cell::new(
            '中',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::WIDE_LEAD,
        );
        snap.primary = GridSnapshot {
            cols: 5,
            viewport_rows: 2,
            rows: vec![bad_row],
            extras: CellExtraTable::new(),
            display_offset: 99,
            total_evicted: 0,
            damage_epoch: 0,
            content_epoch: 0,
        };

        let sanitized = Terminal::from_snapshot(
            snap,
            CoreConfig {
                scrollback_limit: 2,
                ..CoreConfig::default()
            },
        );
        assert_eq!(sanitized.size(), (5, 2));
        assert_eq!(sanitized.cursor_pos(), (4, 1));
        assert!(!sanitized.is_synchronized_output());
        sanitized.grid().debug_verify_extra_invariants();
        sanitized.grid().debug_verify_wide_invariants();
    }

    #[cfg(feature = "serde")]
    #[test]
    fn tampered_snapshot_reconciles_alt_size_wide_flags_and_hyperlink_pool() {
        // 1. Mismatched primary (40x20) vs alternate (2x2) with alt_screen=true and cursor=(39,19).
        let big = new_term(40, 20);
        let small = new_term(2, 2);
        let mut snap = big.export_snapshot();
        snap.alternate = small.export_snapshot().alternate;
        snap.modes.alt_screen = true;
        snap.cursor.row = 19;
        snap.cursor.col = 39;
        snap.scroll_top = 0;
        snap.scroll_bottom = 19;

        let mut restored = Terminal::from_snapshot(snap, CoreConfig::default());
        assert_eq!(restored.size(), (40, 20));
        assert_eq!(restored.cursor_pos(), (39, 19));
        restored.advance(b"X\r\n\x1b[J\x1b[2J\x1b[S\x1b[L\x1b[M");
        restored.grid().debug_verify_wide_invariants();

        // 2. Mutually-exclusive WIDE_LEAD | WIDE_SPACER flags on row 0 are sanitized.
        let mut snap2 = new_term(5, 2).export_snapshot();
        let mut bad_row = Row::new(5, 1);
        bad_row.cells_mut()[0] = Cell::new(
            '中',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::WIDE_LEAD,
        );
        bad_row.cells_mut()[1] = Cell::new(
            ' ',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::WIDE_LEAD | CellFlags::WIDE_SPACER,
        );
        bad_row.cells_mut()[2] = Cell::new(
            ' ',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::WIDE_SPACER,
        );
        snap2.primary.rows[0] = bad_row;
        let restored2 = Terminal::from_snapshot(snap2, CoreConfig::default());
        let c1 = restored2.cell_at(1, 0).unwrap();
        assert!(!(c1.is_wide && c1.is_wide_spacer));
        restored2.grid().debug_verify_wide_invariants();

        // 3. Tampered HyperlinkPool (OOB free_slots, wrong live_count, invalid control-char URI,
        //    dangling hyperlink_id in CellExtra & Cursor) sanitizes cleanly without panicking on OSC 8.
        let mut t3 = new_term(20, 4);
        t3.advance(b"\x1b]8;;https://valid.example\x07hi\x1b]8;;\x07");
        let mut snap3 = t3.export_snapshot();
        snap3.hyperlink_pool.corrupt_for_test();
        let dangling_hid = crate::cell::HyperlinkId::from_raw_for_test(9999);
        snap3.cursor.attrs.hyperlink_id = Some(dangling_hid);
        let mut bad_cell = Cell::new(
            'z',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::empty(),
        );
        bad_cell.extra_id = snap3.primary.extras.insert(CellExtra {
            combining: Vec::new(),
            underline_color: None,
            hyperlink_id: Some(dangling_hid),
            text_sizing: None,
        });
        snap3.primary.rows[0].cells_mut()[3] = bad_cell;

        let mut restored3 = Terminal::from_snapshot(snap3, CoreConfig::default());
        assert!(restored3.cursor().attrs.hyperlink_id.is_none());
        restored3.grid().debug_verify_extra_invariants();
        restored3.advance(b"\r\n\x1b]8;;https://new.example\x07go\x1b]8;;\x07");
        assert_eq!(restored3.visible_line_text(1).as_deref(), Some("go"));
        #[cfg(feature = "hyperlinks")]
        assert_eq!(
            restored3.hyperlink_at(0, 1).map(|h| h.uri.as_str()),
            Some("https://new.example")
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn snapshot_roundtrip_preserves_soft_wrap_wrap_spacer_and_all_features() {
        let cfg = CoreConfig {
            scrollback_limit: 16,
            ..CoreConfig::default()
        };
        let mut orig = Terminal::new(16, 4, cfg.clone());
        // Populate scrollback, wrapped ASCII, WRAP_SPACER wide char, wrapped implicit URL,
        // wrapped OSC 8 + SGR 58 + ZWJ emoji, OSC 133 zones, and Kitty keyboard flags.
        orig.advance(
            "sb0\r\nsb1\r\nabcdefghijklmnopqrst\r\n123456789012345中\r\nsee ftp://gamma.net/file.tar.gz\r\n\x1b]8;id=l1;https://osc8.example/long\x07\x1b[4:3;58:2::10:20:30m👨\u{200d}👩\u{200d}👧\u{200d}👦click_wrapped_link\x1b[0m\x1b]8;;\x07\r\n\x1b]133;A\x07$ \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi_output\r\n\x1b]133;D;0\x07\x1b[>15u"
                .as_bytes(),
        );

        // Scroll up to select across a soft-wrapped line in scrollback.
        orig.scroll_display(4);
        orig.selection_start(0, 0, SelectionKind::Line);
        orig.selection_finish();
        assert!(orig.display_offset() > 0);
        assert!(orig.selection_text().is_some());

        // Round-trip while on primary screen.
        let snap_pri = orig.export_snapshot();
        let restored_pri = Terminal::from_snapshot(snap_pri, cfg.clone());
        assert_eq!(restored_pri.full_text(), orig.full_text());
        assert_eq!(restored_pri.display_offset(), orig.display_offset());
        assert_eq!(restored_pri.selection_text(), orig.selection_text());
        assert!(
            (0..restored_pri.state.primary.total_len()).any(|i| {
                let r = restored_pri.state.primary.row_by_buf(i).unwrap();
                r.soft_wrapped() && r.cells().last().is_some_and(Cell::is_wrap_spacer)
            }),
            "WRAP_SPACER and soft_wrapped must survive snapshot round-trip"
        );
        #[cfg(feature = "shell-integration")]
        {
            use crate::features::shell_integration::ZoneKind;
            let orig_out = orig
                .shell_zones()
                .iter()
                .find(|z| z.kind == ZoneKind::Output)
                .and_then(|z| orig.command_output_at(z.start_line));
            let rest_out = restored_pri
                .shell_zones()
                .iter()
                .find(|z| z.kind == ZoneKind::Output)
                .and_then(|z| restored_pri.command_output_at(z.start_line));
            assert_eq!(rest_out, orig_out);
            assert_eq!(rest_out.as_deref(), Some("hi_output"));
        }
        #[cfg(feature = "kitty-keyboard")]
        assert_eq!(
            restored_pri.kitty_keyboard_flags(),
            orig.kitty_keyboard_flags()
        );
        restored_pri.grid().debug_verify_extra_invariants();
        restored_pri.grid().debug_verify_wide_invariants();
        #[cfg(feature = "hyperlinks")]
        {
            let mut restored_pri = restored_pri;
            let mut found_ftp = false;
            let mut found_osc8 = false;
            for offset in 0..=restored_pri.scrollback_len() {
                restored_pri.scroll_to_bottom();
                if offset > 0 {
                    restored_pri.scroll_display(offset as i32);
                }
                for y in 0..4u16 {
                    for x in 0..16u16 {
                        if let Some(h) = restored_pri.hyperlink_at(x, y) {
                            if h.uri == "ftp://gamma.net/file.tar.gz" {
                                found_ftp = true;
                            } else if h.uri == "https://osc8.example/long" {
                                found_osc8 = true;
                            }
                        }
                    }
                }
            }
            assert!(found_ftp, "wrapped implicit ftp URL must survive restore");
            assert!(found_osc8, "wrapped OSC 8 link must survive restore");
            restored_pri.scroll_to_bottom();
            restored_pri.scroll_display(orig.display_offset() as i32);
        }

        // Resizing wider after restore must unwrap lines identically to the original.
        let mut orig_wide = Terminal::from_snapshot(orig.export_snapshot(), cfg.clone());
        orig.resize(40, 4);
        orig_wide.resize(40, 4);
        assert_eq!(orig_wide.full_text(), orig.full_text());
        assert!(orig_wide.full_text().contains("abcdefghijklmnopqrst"));
        assert!(orig_wide.full_text().contains("123456789012345中"));
        assert!(
            orig_wide
                .full_text()
                .contains("see ftp://gamma.net/file.tar.gz")
        );

        // Round-trip while on alternate screen with alt soft-wrap, WRAP_SPACER, URL, and Kitty stack.
        let mut orig_alt = Terminal::new(16, 4, cfg.clone());
        orig_alt.advance(
            "abcdefghijklmnopqrst\r\n\x1b[>15u\x1b[?1049h123456789012345中\r\nftp://alt.test/long_path\x1b[>3u"
                .as_bytes(),
        );
        let restored_alt = Terminal::from_snapshot(orig_alt.export_snapshot(), cfg);
        assert!(restored_alt.is_alt_screen());
        assert!(restored_alt.grid().row_by_buf(0).unwrap().soft_wrapped());
        assert!(
            restored_alt
                .cell_at(15, 0)
                .unwrap()
                .flags
                .contains(CellFlags::WRAP_SPACER)
        );
        #[cfg(feature = "hyperlinks")]
        assert_eq!(
            restored_alt.hyperlink_at(0, 2).map(|h| h.uri.as_str()),
            Some("ftp://alt.test/long_path")
        );
        #[cfg(feature = "kitty-keyboard")]
        assert_eq!(
            restored_alt.kitty_keyboard_flags(),
            KittyKeyboardFlags::from_bits_truncate(3)
        );
        restored_alt.grid().debug_verify_extra_invariants();
        restored_alt.grid().debug_verify_wide_invariants();

        // Exiting alt screen preserves restored primary soft-wrapped lines and Kitty stack.
        let mut exited = restored_alt;
        exited.advance(b"\x1b[?1049l");
        assert!(!exited.is_alt_screen());
        assert!(exited.full_text().contains("abcdefghijklmnopqrst"));
        #[cfg(feature = "kitty-keyboard")]
        assert_eq!(
            exited.kitty_keyboard_flags(),
            KittyKeyboardFlags::from_bits_truncate(15)
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn tampered_snapshot_exhaustive_sanitization_guards() {
        // 1. Tampered soft_wrapped row with WRAP_SPACER|WIDE_LEAD|WIDE_SPACER + extra_id at last col,
        //    plus mismatched row widths (shorter and longer than cols) with soft_wrapped=true,
        //    plus duplicate extra_id across two cells, plus row.generation() > damage_epoch.
        let mut base = new_term(6, 3);
        base.advance(b"hello");
        let mut snap = base.export_snapshot();
        let mut row0 = Row::new(6, 999_999);
        row0.set_soft_wrapped(true);
        let ext_id = snap.primary.extras.insert(CellExtra {
            combining: vec!['\u{0301}'],
            underline_color: None,
            hyperlink_id: None,
            text_sizing: None,
        });
        row0.cells_mut()[0] = Cell::new(
            'e',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::empty(),
        );
        row0.cells_mut()[0].extra_id = ext_id;
        row0.cells_mut()[1] = Cell::new(
            'a',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::empty(),
        );
        row0.cells_mut()[1].extra_id = ext_id; // duplicate extra_id
        row0.cells_mut()[2] = Cell::new(
            'x',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::from_bits_retain(0xFFFF_FFFF),
        );
        row0.cells_mut()[5] = Cell::new(
            ' ',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::WIDE_LEAD | CellFlags::WIDE_SPACER | CellFlags::WRAP_SPACER,
        );
        row0.cells_mut()[5].extra_id = ext_id;

        let mut short_row = Row::new(3, 50);
        short_row.set_soft_wrapped(true);
        let mut long_row = Row::new(10, 50);
        long_row.set_soft_wrapped(true);

        snap.primary.rows = vec![row0, short_row, long_row];
        snap.primary.damage_epoch = 5;
        snap.primary.content_epoch = 5;

        let restored = Terminal::from_snapshot(snap, CoreConfig::default());
        let r0 = restored.state.primary.row_by_buf(0).unwrap();
        assert!(r0.soft_wrapped());
        assert_eq!(r0.cells()[2].flags.bits() & !CellFlags::all().bits(), 0);
        assert_eq!(
            r0.cells()[2].flags,
            CellFlags::WRITE_ATTR_MASK | CellFlags::SIZED_SPACER
        );
        assert_eq!(r0.cells()[5].flags, CellFlags::WRAP_SPACER);
        assert_eq!(r0.cells()[5].extra_id, 0);
        assert!(!restored.state.primary.row_by_buf(1).unwrap().soft_wrapped());
        assert!(!restored.state.primary.row_by_buf(2).unwrap().soft_wrapped());
        for i in 0..restored.state.primary.total_len() {
            let r = restored.state.primary.row_by_buf(i).unwrap();
            assert!(r.generation() <= restored.state.primary.damage_epoch());
        }
        restored.grid().debug_verify_extra_invariants();
        restored.grid().debug_verify_wide_invariants();

        // 2. u64::MAX epochs / total_evicted + dirty cursor attrs flags + saved cursor flags.
        let mut snap_max = new_term(8, 3).export_snapshot();
        snap_max.primary.damage_epoch = u64::MAX;
        snap_max.primary.content_epoch = u64::MAX;
        snap_max.primary.total_evicted = u64::MAX;
        snap_max.alternate.total_evicted = 42;
        snap_max.cursor.attrs.flags = CellFlags::all();
        snap_max.primary_saved_cursor = Some(SavedCursor {
            attrs: CursorAttrs {
                flags: CellFlags::all(),
                ..CursorAttrs::default()
            },
            ..SavedCursor::default()
        });
        let mut restored_max = Terminal::from_snapshot(snap_max, CoreConfig::default());
        assert_eq!(restored_max.state.alternate.total_evicted(), 0);
        assert_eq!(
            restored_max.cursor().attrs.flags & !CellFlags::WRITE_ATTR_MASK,
            CellFlags::empty()
        );
        assert_eq!(
            restored_max
                .state
                .primary_saved_cursor
                .as_ref()
                .unwrap()
                .attrs
                .flags
                & !CellFlags::WRITE_ATTR_MASK,
            CellFlags::empty()
        );
        restored_max.advance(b"hi\r\n");
        restored_max.resize(12, 4);

        // 3. Dimension clamping: cols = 70_000 -> u16::MAX, and cols = 0 / rows = 0 -> (1, 1).
        let mut snap_wide = new_term(4, 1).export_snapshot();
        snap_wide.primary.cols = 70_000;
        snap_wide.primary.viewport_rows = 1;
        snap_wide.alternate.cols = 70_000;
        snap_wide.alternate.viewport_rows = 1;
        let restored_wide = Terminal::from_snapshot(snap_wide, CoreConfig::default());
        assert_eq!(restored_wide.size(), (u16::MAX, 1));

        let mut snap_zero = new_term(4, 2).export_snapshot();
        snap_zero.primary.cols = 0;
        snap_zero.primary.viewport_rows = 0;
        snap_zero.alternate.cols = 0;
        snap_zero.alternate.viewport_rows = 0;
        let restored_zero = Terminal::from_snapshot(snap_zero, CoreConfig::default());
        assert_eq!(restored_zero.size(), (1, 1));

        // 4. Selection with valid anchor/head but OOB origin_start/origin_end must be cleared.
        let mut t_sel = new_term(10, 4);
        t_sel.advance(b"hello world");
        let mut snap_sel = t_sel.export_snapshot();
        let mut bad_sel = Selection::new(
            SelectionMode::Word,
            GridPoint::new(AbsLine(0), 0),
            GridPoint::new(AbsLine(0), 4),
        );
        bad_sel.origin_start = GridPoint::new(AbsLine(999), 0);
        bad_sel.origin_end = GridPoint::new(AbsLine(999), 4);
        snap_sel.selection = Some(bad_sel);
        let restored_sel = Terminal::from_snapshot(snap_sel, CoreConfig::default());
        assert!(restored_sel.selection_text().is_none());

        // 5. Tampered ShellIntegrationState (inverted/OOB zones and OOB ActivePhase).
        #[cfg(feature = "shell-integration")]
        {
            let mut t_si = new_term(10, 4);
            t_si.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07\x1b]133;C\x07");
            let mut snap_si = t_si.export_snapshot();
            if let Some(ref mut si) = snap_si.shell_integration {
                si.tamper_for_test();
            }
            let mut restored_si = Terminal::from_snapshot(snap_si, CoreConfig::default());
            for z in restored_si.shell_zones() {
                assert!(z.start_line <= z.end_line);
                assert!(z.end_line < restored_si.total_lines());
                assert!(z.start_col <= 10);
                assert!(z.end_col <= 10);
            }
            restored_si.advance(b"more\r\n\x1b]133;D;0\x07");
            restored_si.resize(5, 4);
        }

        // 6. Tampered KittyKeyboardFlags with unknown high bits (0xFF) truncated to <= 0x1F.
        #[cfg(feature = "kitty-keyboard")]
        {
            let mut snap_k = new_term(10, 4).export_snapshot();
            snap_k.kitty_primary_stack = vec![KittyKeyboardFlags::from_bits_retain(0xFF)];
            snap_k.kitty_alt_stack = vec![KittyKeyboardFlags::from_bits_retain(0xE1)];
            let mut restored_k = Terminal::from_snapshot(snap_k, CoreConfig::default());
            assert_eq!(
                restored_k.kitty_keyboard_flags(),
                KittyKeyboardFlags::from_bits_truncate(0xFF)
            );
            assert!(restored_k.kitty_keyboard_flags().bits() <= 0x1F);
            restored_k.state.modes.alt_screen = true;
            assert_eq!(
                restored_k.kitty_keyboard_flags(),
                KittyKeyboardFlags::from_bits_truncate(0xE1)
            );
            assert!(restored_k.kitty_keyboard_flags().bits() <= 0x1F);
        }
    }

    #[test]
    fn resize_clamps_primary_and_alt_saved_cursor_for_decrc() {
        // 1. Primary screen DECSC -> shrink -> grow -> DECRC clamps to shrunk bounds.
        let mut t = new_term(20, 4);
        t.advance(b"\x1b[3;6H\x1b7");
        t.resize(10, 2);
        t.resize(20, 6);
        t.advance(b"\x1b8");
        assert_eq!(t.cursor_pos(), (5, 1));

        // 2. Primary screen DECSC at col 15 -> shrink cols -> grow cols -> DECRC clamps col.
        let mut t_col = new_term(20, 4);
        t_col.advance(b"\x1b[2;16H\x1b7");
        t_col.resize(8, 4);
        t_col.resize(20, 4);
        t_col.advance(b"\x1b8");
        assert_eq!(t_col.cursor_pos(), (7, 1));

        // 3. Alt screen DECSC -> shrink -> grow -> DECRC clamps alt_saved_cursor.
        let mut t_alt = new_term(20, 4);
        t_alt.advance(b"\x1b[?47h\x1b[4;15H\x1b7");
        t_alt.resize(10, 2);
        t_alt.resize(20, 6);
        t_alt.advance(b"\x1b8");
        assert_eq!(t_alt.cursor_pos(), (9, 1));

        // 4. Primary DECSC with pending_wrap -> widen -> shrink -> DECRC clears pending_wrap.
        let mut t_pw = new_term(10, 4);
        t_pw.advance(b"1234567890\x1b7\x1b[1;1H");
        t_pw.resize(20, 4);
        t_pw.resize(10, 4);
        t_pw.advance(b"\x1b8");
        assert_eq!(t_pw.cursor_pos(), (9, 0));
        assert!(!t_pw.cursor().pending_wrap);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn resize_saved_cursor_snapshot_roundtrip_and_decrc_parity() {
        let cfg = CoreConfig::default();

        // Primary screen (Challenger Bug 2 minimal reproducer)
        let mut live = Terminal::new(20, 4, cfg.clone());
        live.advance(b"\x1b[3;6H\x1b7");
        live.resize(10, 2);
        let snap = live.export_snapshot();
        let mut rest = Terminal::from_snapshot(snap.clone(), cfg.clone());
        assert_eq!(rest.export_snapshot(), snap);
        live.resize(20, 6);
        rest.resize(20, 6);
        live.advance(b"\x1b8");
        rest.advance(b"\x1b8");
        assert_eq!(live.cursor_pos(), (5, 1));
        assert_eq!(live.cursor_pos(), rest.cursor_pos());

        // Alt screen DECSC parity
        let mut live_alt = Terminal::new(20, 4, cfg.clone());
        live_alt.advance(b"\x1b[?47h\x1b[3;15H\x1b7");
        live_alt.resize(10, 2);
        let snap_alt = live_alt.export_snapshot();
        let mut rest_alt = Terminal::from_snapshot(snap_alt.clone(), cfg.clone());
        assert_eq!(rest_alt.export_snapshot(), snap_alt);
        live_alt.resize(20, 6);
        rest_alt.resize(20, 6);
        live_alt.advance(b"\x1b8");
        rest_alt.advance(b"\x1b8");
        assert_eq!(live_alt.cursor_pos(), (9, 1));
        assert_eq!(live_alt.cursor_pos(), rest_alt.cursor_pos());

        // ?1049h saves primary_saved_cursor; resize on alt screen clamps both primary & alt saved cursors.
        let mut live_1049 = Terminal::new(20, 4, cfg.clone());
        live_1049.advance(b"\x1b[4;16H\x1b[?1049h\x1b[3;14H\x1b7");
        live_1049.resize(8, 2);
        let snap_1049 = live_1049.export_snapshot();
        let mut rest_1049 = Terminal::from_snapshot(snap_1049.clone(), cfg.clone());
        assert_eq!(rest_1049.export_snapshot(), snap_1049);
        live_1049.resize(20, 6);
        rest_1049.resize(20, 6);
        live_1049.advance(b"\x1b8\x1b[?1049l");
        rest_1049.advance(b"\x1b8\x1b[?1049l");
        assert_eq!(live_1049.cursor_pos(), (7, 1));
        assert_eq!(live_1049.cursor_pos(), rest_1049.cursor_pos());

        // pending_wrap saved cursor widen parity
        let mut live_pw = Terminal::new(10, 4, cfg.clone());
        live_pw.advance(b"1234567890\x1b7\x1b[1;1H");
        live_pw.resize(20, 4);
        let snap_pw = live_pw.export_snapshot();
        let mut rest_pw = Terminal::from_snapshot(snap_pw.clone(), cfg);
        assert_eq!(rest_pw.export_snapshot(), snap_pw);
        live_pw.resize(10, 4);
        rest_pw.resize(10, 4);
        live_pw.advance(b"\x1b8");
        rest_pw.advance(b"\x1b8");
        assert_eq!(live_pw.cursor_pos(), rest_pw.cursor_pos());
        assert_eq!(live_pw.cursor().pending_wrap, rest_pw.cursor().pending_wrap);
    }

    #[test]
    fn dcs_xtgettcap_and_decrqss_queries() {
        let mut t = new_term(80, 24);

        // XTGETTCAP: TN (544e), RGB (524742), colors (636f6c6f7273), unknown (464f4f)
        t.advance(b"\x1bP+q544e;524742;636f6c6f7273;464f4f\x1b\\");
        let reply = String::from_utf8(t.drain_outgoing()).unwrap();
        assert!(reply.contains("\x1bP1+r544e=656775695F787465726D\x1b\\"));
        assert!(reply.contains("\x1bP1+r524742=382F382F38\x1b\\"));
        assert!(reply.contains("\x1bP1+r636f6c6f7273=323536\x1b\\"));
        assert!(reply.contains("\x1bP0+r464f4f\x1b\\"));

        // XTGETTCAP: invalid hex -> 0+r
        t.advance(b"\x1bP+qZZZZ\x1b\\");
        assert_eq!(t.drain_outgoing(), b"\x1bP0+rZZZZ\x1b\\");

        // DECRQSS: m (SGR), r (DECSTBM), q (DECSCUSR), "q (DECSCA), unsupported
        t.advance(b"\x1b[1;4:3;38;2;10;20;30m\x1bP$qm\x1b\\");
        assert_eq!(t.drain_outgoing(), b"\x1bP1$r0;1;4:3;38;2;10;20;30m\x1b\\");

        t.advance(b"\x1b[3;10r\x1bP$qr\x1b\\");
        assert_eq!(t.drain_outgoing(), b"\x1bP1$r3;10r\x1b\\");

        t.advance(b"\x1b[4 q\x1bP$q q\x1b\\");
        assert_eq!(t.drain_outgoing(), b"\x1bP1$r4 q\x1b\\");

        t.advance(b"\x1bP$q\"q\x1b\\");
        assert_eq!(t.drain_outgoing(), b"\x1bP1$r0\"q\x1b\\");

        t.advance(b"\x1bP$qxyz\x1b\\");
        assert_eq!(t.drain_outgoing(), b"\x1bP0$r\x1b\\");

        // DECRQSS s: unsupported when ?69 is off, supported when ?69 is on
        t.advance(b"\x1bP$qs\x1b\\");
        assert_eq!(t.drain_outgoing(), b"\x1bP0$r\x1b\\");
        t.advance(b"\x1b[?69h\x1b[5;20s\x1bP$qs\x1b\\");
        assert_eq!(t.drain_outgoing(), b"\x1bP1$r5;20s\x1b\\");
    }

    #[test]
    fn csi_xtversion_xtwinops_and_theme_dsr() {
        let mut t = new_term(80, 24);

        // XTVERSION
        t.advance(b"\x1b[>q\x1b[>0q");
        let expected_ver = format!("\x1bP>|egui_xterm {}\x1b\\", env!("CARGO_PKG_VERSION"));
        assert_eq!(
            String::from_utf8(t.drain_outgoing()).unwrap(),
            format!("{expected_ver}{expected_ver}")
        );

        // XTWINOPS 14/16/18 with default fallback then explicit pixel size
        t.advance(b"\x1b[14t\x1b[16t\x1b[18t");
        assert_eq!(
            t.drain_outgoing(),
            b"\x1b[4;384;640t\x1b[6;16;8t\x1b[8;24;80t"
        );
        t.set_pixel_size(800, 480);
        assert_eq!(t.pixel_size(), (800, 480));
        t.advance(b"\x1b[14t\x1b[16t");
        assert_eq!(t.drain_outgoing(), b"\x1b[4;480;800t\x1b[6;20;10t");

        // XTWINOPS 22/23 title stack
        t.advance(b"\x1b]0;Initial\x07\x1b[22;0t\x1b]0;Temp\x07");
        assert_eq!(t.title(), "Temp");
        assert_eq!(t.icon_name(), "Temp");
        let _ = t.drain_events();
        t.advance(b"\x1b[23;0t");
        assert_eq!(t.title(), "Initial");
        assert_eq!(t.icon_name(), "Initial");
        let evs = t.drain_events();
        assert!(evs.contains(&TerminalEvent::TitleChanged("Initial".to_owned())));
        assert!(evs.contains(&TerminalEvent::IconNameChanged("Initial".to_owned())));

        // DEC ?2027, ?2031, and DSR ?996 n
        t.advance(b"\x1b[?2027$p\x1b[?2027l\x1b[?2027$p\x1b[?2027h");
        assert_eq!(t.drain_outgoing(), b"\x1b[?2027;1$y\x1b[?2027;2$y");

        t.advance(b"\x1b[?996n\x1b[?2031$p\x1b[?2031h\x1b[?2031$p");
        assert_eq!(
            t.drain_outgoing(),
            b"\x1b[?997;1n\x1b[?2031;2$y\x1b[?2031;1$y"
        );

        t.set_theme(Theme::light());
        assert_eq!(t.drain_outgoing(), b"\x1b[?997;2n");
        t.advance(b"\x1b[?996n");
        assert_eq!(t.drain_outgoing(), b"\x1b[?997;2n");
    }

    #[test]
    fn decslrm_margins_wrap_scroll_and_scosc() {
        let mut t = new_term(10, 4);

        // When ?69 is unset, CSI s acts as SCOSC (save cursor)
        t.advance(b"\x1b[2;4H\x1b[s\x1b[1;1H\x1b[u");
        assert_eq!(t.cursor_pos(), (3, 1));

        // Enable ?69, set margins 3..=6 (0-based 2..=5), and write wrapping text
        t.advance(b"\x1b[?69h\x1b[?69$p\x1b[3;6s");
        assert_eq!(t.drain_outgoing(), b"\x1b[?69;1$y");
        t.advance(b"\x1b[1;3HABCDEFGH");
        assert_eq!(t.visible_line_text(0).as_deref(), Some("  ABCD"));
        assert_eq!(t.visible_line_text(1).as_deref(), Some("  EFGH"));

        // CR stays at left margin (col 2)
        t.advance(b"\rX");
        assert_eq!(t.visible_line_text(1).as_deref(), Some("  XFGH"));

        // Scroll up within box (rows 0..=3, cols 2..=5) preserves cols outside margins
        t.advance(b"\x1b[1;1H1\x1b[2;1H2\x1b[1S");
        assert_eq!(t.visible_line_text(0).as_deref(), Some("1 XFGH"));
        assert_eq!(t.visible_line_text(1).as_deref(), Some("2"));

        // Disabling ?69 resets horizontal margins
        t.advance(b"\x1b[?69l\x1b[?69$p");
        assert_eq!(t.drain_outgoing(), b"\x1b[?69;2$y");
        assert_eq!(t.state.scroll_left, 0);
        assert_eq!(t.state.scroll_right, 9);
    }

    #[test]
    fn mouse_1016_sgr_pixels_and_modern_osc_events() {
        let mut t = new_term(80, 24);
        t.advance(b"\x1b[?1002h\x1b[?1016h\x1b[?1016$p");
        assert_eq!(t.drain_outgoing(), b"\x1b[?1016;1$y");
        assert_eq!(t.modes().mouse_encoding, MouseEncoding::SgrPixels);

        let press = MouseInputEvent {
            kind: MouseEventKind::Press,
            button: Some(MouseButton::Left),
            col: 5,
            row: 3,
            modifiers: MouseModifiers {
                shift: false,
                alt: false,
                ctrl: true,
            },
        };
        assert_eq!(
            t.encode_mouse_event_pixels(press, 123, 456).as_deref(),
            Some(b"\x1b[<16;123;456M".as_slice())
        );

        let release = MouseInputEvent {
            kind: MouseEventKind::Release,
            ..press
        };
        assert_eq!(
            t.encode_mouse_event_pixels(release, 123, 456).as_deref(),
            Some(b"\x1b[<16;123;456m".as_slice())
        );

        // OSC 9;4 progress, OSC 9 notification, OSC 99 (chunked + base64), OSC 777, OSC 22
        t.advance(b"\x1b]9;4;1;42\x07");
        t.advance(b"\x1b]9;Build complete\x07");
        t.advance(b"\x1b]99;p=title:d=0;Alerts\x1b\\");
        t.advance(b"\x1b]99;p=body:e=1;aGVsbG8=\x1b\\");
        t.advance(b"\x1b]777;notify;Done;All tests passed\x07");
        t.advance(b"\x1b]22;pointer\x07");

        let events = t.drain_events();
        assert_eq!(
            events,
            vec![
                TerminalEvent::Progress {
                    state: 1,
                    progress: 42
                },
                TerminalEvent::Notification {
                    title: String::new(),
                    body: "Build complete".to_owned()
                },
                TerminalEvent::Notification {
                    title: "Alerts".to_owned(),
                    body: "hello".to_owned()
                },
                TerminalEvent::Notification {
                    title: "Done".to_owned(),
                    body: "All tests passed".to_owned()
                },
                TerminalEvent::MouseShape("pointer".to_owned()),
            ]
        );
    }

    #[test]
    fn terminal_state_split_utf8_run_and_put_slice() {
        let mut t = new_term(20, 4);
        // UTF-8 CJK + emoji + combining + ASCII run + SS2 UK/DEC charset
        let payload = "你好e\u{0301}🚀abc";
        t.advance(payload.as_bytes());
        assert_eq!(
            t.visible_line_text(0).as_deref(),
            Some("你好e\u{0301}🚀abc")
        );
        t.grid().debug_verify_wide_invariants();
        t.grid().debug_verify_extra_invariants();

        // Direct Parser + TerminalState usage without wrappers
        let mut parser = Parser::new();
        let mut state = TerminalState::new(16, 4, CoreConfig::default());
        parser.advance(b"\x1bPq\"1;1;1;1#0;2;100;0;0#0~\x1b\\", &mut state);
        parser.advance(b"\x1bP$q\"q\x1b\\", &mut state);
        assert_eq!(state.drain_outgoing(), b"\x1bP1$r0\"q\x1b\\");
        #[cfg(feature = "sixel")]
        assert_eq!(state.image_placements().len(), 1);
    }

    #[test]
    fn sl_sr_horizontal_shift_and_margins() {
        let mut t = new_term(10, 4);
        t.advance(b"ABCDEFGHIJ\r\n0123456789\r\nQRSTUVWXYZ");
        // Full-screen SL 2 (`CSI 2 SP @`)
        t.advance(b"\x1b[2 @");
        assert_eq!(t.visible_line_text(0).as_deref(), Some("CDEFGHIJ"));
        assert_eq!(t.visible_line_text(1).as_deref(), Some("23456789"));

        // Full-screen SR 1 (`CSI 1 SP A`)
        t.advance(b"\x1b[1 A");
        assert_eq!(t.visible_line_text(0).as_deref(), Some(" CDEFGHIJ"));
        assert_eq!(t.visible_line_text(1).as_deref(), Some(" 23456789"));

        // With DECLRMM + DECSTBM box margins (rows 2..3, cols 3..6)
        t.advance(b"\x1b[?69h\x1b[2;3r\x1b[3;6s\x1b[2 @");
        assert_eq!(t.visible_line_text(0).as_deref(), Some(" CDEFGHIJ"));
        assert_eq!(t.visible_line_text(1).as_deref(), Some(" 256  789"));
    }

    #[test]
    fn decsca_decsed_decsel_and_decrqss_protection() {
        let mut t = new_term(10, 3);
        // Enable DECSCA 1, write protected text; SGR 0 must NOT clear protection
        t.advance(b"A\x1b[1\"q\x1b[31mB\x1b[0mC\x1b[0\"qD");
        assert!(
            !t.cell_at(0, 0)
                .unwrap()
                .flags
                .contains(CellFlags::PROTECTED)
        );
        assert!(
            t.cell_at(1, 0)
                .unwrap()
                .flags
                .contains(CellFlags::PROTECTED)
        );
        assert!(
            t.cell_at(2, 0)
                .unwrap()
                .flags
                .contains(CellFlags::PROTECTED)
        );
        assert!(
            !t.cell_at(3, 0)
                .unwrap()
                .flags
                .contains(CellFlags::PROTECTED)
        );

        // Ignored DECSCA >= 3 leaves state unchanged; 2 unprotects; DECRQSS reports "q dynamically
        t.advance(b"\x1b[1\"q\x1b[5\"q\x1bP$q\"q\x1b\\");
        assert_eq!(t.drain_outgoing(), b"\x1bP1$r1\"q\x1b\\");
        t.advance(b"\x1b[2\"q\x1bP$q\"q\x1b\\");
        assert_eq!(t.drain_outgoing(), b"\x1bP1$r0\"q\x1b\\");

        // DECSEL 2 on row 0 erases A and D while preserving protected B and C
        t.advance(b"\x1b[1;1H\x1b[?2K");
        assert_eq!(t.cell_at(0, 0).unwrap().ch, ' ');
        assert_eq!(t.cell_at(1, 0).unwrap().ch, 'B');
        assert_eq!(t.cell_at(2, 0).unwrap().ch, 'C');
        assert_eq!(t.cell_at(3, 0).unwrap().ch, ' ');

        // DECSED 2 leaves protected cells on row 0, clears unprotected row 1
        t.advance(b"\x1b[2;1HUNPROT\x1b[?2J");
        assert_eq!(t.cell_at(1, 0).unwrap().ch, 'B');
        assert_eq!(t.cell_at(2, 0).unwrap().ch, 'C');
        assert_eq!(t.visible_line_text(1).as_deref(), Some(""));

        // Standard ED 2 erases protected cells too
        t.advance(b"\x1b[2J");
        assert_eq!(t.visible_line_text(0).as_deref(), Some(""));
    }

    #[test]
    fn vt420_rect_ops_deccra_decera_decfra_decsera_and_decom() {
        let mut t = new_term(10, 4);
        t.advance(b"12345\r\nABCDE");
        // DECCRA: copy (1,1..2,3) to (2,3) -> overlapping copy
        t.advance(b"\x1b[1;1;2;3;1;2;3;1$v");
        assert_eq!(t.visible_line_text(1).as_deref(), Some("AB123"));
        assert_eq!(t.visible_line_text(2).as_deref(), Some("  ABC"));

        // DECFRA invalid char (10) ignored; valid '*' (42) fills (1,2..2,3) with DECSCA
        t.advance(b"\x1b[1\"q\x1b[10;1;2;2;3$x");
        assert_eq!(t.visible_line_text(0).as_deref(), Some("12345"));
        t.advance(b"\x1b[42;1;2;2;3$x\x1b[0\"q");
        assert_eq!(t.visible_line_text(0).as_deref(), Some("1**45"));
        assert!(
            t.cell_at(1, 0)
                .unwrap()
                .flags
                .contains(CellFlags::PROTECTED)
        );

        // DECSERA keeps protected '*' at (1,1..1,2) while erasing unprotected (1,0)
        t.advance(b"\x1b[1;1;1;5${");
        assert_eq!(t.visible_line_text(0).as_deref(), Some(" **"));

        // DECERA erases protected cells too
        t.advance(b"\x1b[1;1;1;5$z");
        assert_eq!(t.visible_line_text(0).as_deref(), Some(""));

        // DECOM relative rect fill inside margins (rows 2..3, cols 3..6)
        t.advance(b"\x1b[?69h\x1b[2;3r\x1b[3;6s\x1b[?6h\x1b[65;1;1;1;2$x");
        assert_eq!(t.cell_at(2, 1).unwrap().ch, 'A');
        assert_eq!(t.cell_at(3, 1).unwrap().ch, 'A');
    }

    #[test]
    fn xtpushsgr_xtpopsgr_selective_and_overflow() {
        let mut t = new_term(10, 2);
        // Set bold + italic + fg red, push full, change to underline + fg blue, pop
        t.advance(b"\x1b[1;3;31m\x1b[#{");
        t.advance(b"\x1b[0;4;34m");
        assert_eq!(
            t.cursor().attrs.flags.underline_style(),
            UnderlineStyle::Single
        );
        assert!(!t.cursor().attrs.flags.contains(CellFlags::BOLD));
        t.advance(b"\x1b[#}");
        assert!(t.cursor().attrs.flags.contains(CellFlags::BOLD));
        assert!(t.cursor().attrs.flags.contains(CellFlags::ITALIC));
        assert_eq!(
            t.cursor().attrs.flags.underline_style(),
            UnderlineStyle::None
        );
        assert_eq!(t.cursor().attrs.fg, PackedColor::Indexed(1));

        // Selective push of ONLY bold (1) and bg (11), using alias #p / #q
        t.advance(b"\x1b[0;1;42m\x1b[1;11#p\x1b[0;3;44m\x1b[#q");
        assert!(t.cursor().attrs.flags.contains(CellFlags::BOLD));
        assert!(t.cursor().attrs.flags.contains(CellFlags::ITALIC));
        assert_eq!(t.cursor().attrs.bg, PackedColor::Indexed(2));

        // Depth-10 overflow tracking: push 12 times, pop 12 times restores initial attrs
        t.advance(b"\x1b[0;1m");
        for _ in 0..12 {
            t.advance(b"\x1b[#{\x1b[0;3m");
        }
        for _ in 0..12 {
            t.advance(b"\x1b[#}");
        }
        assert!(t.cursor().attrs.flags.contains(CellFlags::BOLD));
    }

    #[test]
    fn xtwinops_11_13_16_19_20_21_and_sanitization() {
        let mut t = new_term(80, 24);
        t.set_window_iconified(true);
        t.set_window_pos(120, 340);
        t.set_cell_pixel_size(11, 22);
        t.set_screen_char_size(Some((160, 48)));
        t.advance(b"\x1b]1;ic\x08on\x07\x1b]2;ti\x7ftle\x1b\\");
        t.advance(b"\x1b[11t\x1b[13t\x1b[16t\x1b[19t\x1b[20t\x1b[21t");
        assert_eq!(
            String::from_utf8_lossy(&t.drain_outgoing()),
            "\x1b[2t\x1b[3;120;340t\x1b[6;22;11t\x1b[9;48;160t\x1b]Licon\x1b\\\x1b]ltitle\x1b\\"
        );
    }

    #[test]
    fn charsets_g2_g3_uk_ls_ss2_ss3_decaln_decbi_decfi() {
        let mut t = new_term(10, 3);
        // Designate G2 = UK ('A'), G3 = DEC Special ('0'), test SS2 (`ESC N`) and LS3 (`ESC o`)
        t.advance(b"\x1b*A\x1b+0");
        t.advance(b"##\x1bN#\x1bNA\x1boq");
        assert_eq!(t.visible_line_text(0).as_deref(), Some("##£A─"));

        // DECALN (`ESC # 8`) fills entire grid with 'E' and resets cursor/margins
        t.advance(b"\x1b#8");
        assert_eq!(t.visible_line_text(0).as_deref(), Some("EEEEEEEEEE"));
        assert_eq!(t.visible_line_text(2).as_deref(), Some("EEEEEEEEEE"));
        assert_eq!(t.cursor_pos(), (0, 0));

        // DECBI at col 0 shifts right; DECFI at col 9 shifts left
        t.advance(b"\x1b[2J\x1b[1;1HABC\x1b[1;1H\x1b6");
        assert_eq!(t.visible_line_text(0).as_deref(), Some(" ABC"));
        t.advance(b"\x1b[1;10HZ\x1b9");
        assert_eq!(t.visible_line_text(0).as_deref(), Some("ABC     Z"));
    }

    #[test]
    fn declrmm_parity_edge_cases() {
        let mut t = new_term(12, 4);
        // CSI Ps;Ps s ignored when ?69 is OFF (must NOT save cursor at 0,0 nor change margins)
        t.advance(b"\x1b[2;4H\x1b7\x1b[3;8s\x1b[1;1H\x1b8");
        assert_eq!(t.cursor_pos(), (3, 1));

        // Enable ?69, set margins 3..8 (0-based 2..7), test BS/HT/CHT/CBT/NEL/DECSTBM
        t.advance(b"\x1b[?69h\x1b[3;8s");
        // BS inside margin stops at scroll_left (2)
        t.advance(b"\x1b[1;4H\x08\x08\x08");
        assert_eq!(t.cursor_pos().0, 2);
        // HT / CHT clamp at scroll_right (7); CBT clamps at scroll_left (2)
        t.advance(b"\x09\x1b[5I");
        assert_eq!(t.cursor_pos().0, 7);
        t.advance(b"\x1b[5Z");
        assert_eq!(t.cursor_pos().0, 2);
        // NEL (`ESC E`) homes to scroll_left (2) when col >= scroll_left
        t.advance(b"\x1b[1;6H\x1bE");
        assert_eq!(t.cursor_pos(), (2, 1));
        // DECSTBM with DECOM homes col to scroll_left (2)
        t.advance(b"\x1b[?6h\x1b[2;3r");
        assert_eq!(t.cursor_pos(), (2, 1));
    }

    #[test]
    fn modern_osc_5_6_9_22_66_99_and_osc52_policy() {
        let mut t = new_term(20, 4);
        let epoch0 = t.palette_epoch();
        // OSC 5 set & query, OSC 105 reset
        t.advance(b"\x1b]5;0;rgb:11/22/33\x07\x1b]5;0;?\x07");
        assert_eq!(t.special_colors().bold, Some([0x11, 0x22, 0x33]));
        assert!(t.palette_epoch() > epoch0);
        assert_eq!(t.drain_outgoing(), b"\x1b]5;0;rgb:1111/2222/3333\x07");
        t.advance(b"\x1b]105;0\x07");
        assert_eq!(t.special_colors().bold, None);

        // OSC 6 document URI vs iTerm2 tab color disambiguation
        t.advance(b"\x1b]6;1;bg;red;brightness;255\x07");
        assert_eq!(t.document_uri(), None);
        t.advance(b"\x1b]6;file:///tmp/notes.txt\x07");
        assert_eq!(t.document_uri(), Some("file:///tmp/notes.txt"));

        // OSC 9;4 validation: st=5 ignored, pr=250 clamped to 100
        t.advance(b"\x1b]9;4;5;50\x07\x1b]9;4;1;250\x07");

        // OSC 22 push/pop/strip/query
        t.advance(b"\x1b]22;>pointer\x07\x1b]22;>text\x07\x1b]22;?__current__\x07\x1b]22;<\x07\x1b]22;?__current__\x07");
        assert_eq!(t.drain_outgoing(), b"\x1b]22;text\x07\x1b]22;pointer\x07");

        // OSC 66 Kitty Text Sizing: s=2 auto footprint & SIZED_SPACER
        t.advance(b"\x1b[1;1H\x1b]66;s=2;Hi\x1b\\");
        assert_eq!(t.visible_line_text(0).as_deref(), Some("Hi"));
        assert_eq!(t.render_snapshot_line_text(0), t.visible_line_text(0));
        assert_eq!(t.cursor_pos(), (4, 0));
        let c0 = t.cell_at(0, 0).unwrap();
        assert_eq!(c0.ch, 'H');
        let extra_id0 = t.grid().viewport_row(0).unwrap().cells()[0].extra_id;
        assert_eq!(
            t.cell_extra(extra_id0)
                .and_then(|e| e.text_sizing)
                .map(|ts| ts.scale),
            Some(2)
        );
        assert!(
            t.cell_at(1, 0)
                .unwrap()
                .flags
                .contains(CellFlags::SIZED_SPACER)
        );
        assert!(
            t.cell_at(0, 1)
                .unwrap()
                .flags
                .contains(CellFlags::SIZED_SPACER)
        );
        t.grid().debug_verify_wide_invariants();
        t.grid().debug_verify_extra_invariants();

        // OSC 99 keyed i=<id> chunking & close & query
        t.advance(b"\x1b]99;i=a:p=title:d=0;First\x1b\\");
        t.advance(b"\x1b]99;i=b:p=title:d=0;DropMe\x1b\\");
        t.advance(b"\x1b]99;i=b:p=close;\x1b\\");
        t.advance(b"\x1b]99;i=a:p=body:d=1;Done\x1b\\");

        let events = t.drain_events();
        assert!(events.contains(&TerminalEvent::DocumentChanged(Some(
            "file:///tmp/notes.txt".to_owned()
        ))));
        assert!(events.contains(&TerminalEvent::Progress {
            state: 1,
            progress: 100
        }));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, TerminalEvent::Progress { state: 5, .. }))
        );
        assert!(events.contains(&TerminalEvent::Notification {
            title: "First".to_owned(),
            body: "Done".to_owned()
        }));
        assert_eq!(
            events
                .iter()
                .filter(|e| **e == TerminalEvent::MouseShape("pointer".to_owned()))
                .count(),
            2
        );
        assert!(!events.iter().any(|e| matches!(
            e,
            TerminalEvent::Notification { title, .. } if title == "DropMe"
        )));

        // Osc52ReadPolicy::Prompt emits ClipboardReadPrompt and accepts provide_clipboard_read
        t.set_osc52_read_policy(Osc52ReadPolicy::Prompt);
        t.advance(b"\x1b]52;c;?\x07");
        assert!(
            t.drain_events()
                .contains(&TerminalEvent::ClipboardReadPrompt {
                    selection: ClipboardSelection::Clipboard
                })
        );
        t.provide_clipboard_read(ClipboardSelection::Clipboard, "ok");
        assert_eq!(t.drain_outgoing(), b"\x1b]52;c;b2s=\x1b\\");
    }

    #[cfg(feature = "kitty-keyboard")]
    #[test]
    fn modify_other_keys_kitty_precedence_and_zero_alloc_paths() {
        use crate::features::kitty_keyboard::{KittyEventType, KittyKey, KittyModifiers};

        let mut t = new_term(80, 24);
        t.advance(b"\x1b[>4;2m\x1b[?4m");
        assert_eq!(t.modify_other_keys(), ModifyOtherKeys::Always);
        assert_eq!(t.drain_outgoing(), b"\x1b[>4;2m");

        let enter_ctrl = KittyKeyEvent {
            key: KittyKey::Enter,
            modifiers: KittyModifiers {
                ctrl: true,
                ..Default::default()
            },
            event_type: KittyEventType::Press,
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        let mut buf = Vec::new();
        assert!(t.encode_key_event_into(&enter_ctrl, &mut buf));
        assert_eq!(buf, b"\x1b[27;5;13~");

        // Kitty keyboard flags take strict precedence over modifyOtherKeys
        t.advance(b"\x1b[>1u");
        buf.clear();
        assert!(!t.encode_modify_other_keys_into(&enter_ctrl, &mut buf));
        assert!(t.encode_key_event_into(&enter_ctrl, &mut buf));
        assert_eq!(buf, b"\x1b[13;5u");

        t.advance(b"\x1b[>4n");
        assert_eq!(t.modify_other_keys(), ModifyOtherKeys::Disabled);

        // ?1016 pixel fallback using configured cell pixel size (10x20)
        t.set_cell_pixel_size(10, 20);
        t.advance(b"\x1b[?1000h\x1b[?1016h");
        let click = MouseInputEvent {
            kind: MouseEventKind::Press,
            button: Some(MouseButton::Left),
            col: 2,
            row: 1,
            modifiers: MouseModifiers::default(),
        };
        buf.clear();
        assert!(t.encode_mouse_event_into(click, &mut buf));
        assert_eq!(buf, b"\x1b[<0;25;30M");

        // VisibleRowsView + has_non_empty_selection
        t.advance(b"hello");
        let snap = t.active_snapshot();
        assert_eq!(snap.visible_rows.len(), 24);
        assert!(!t.has_non_empty_selection());
        t.selection_start(0, 0, SelectionKind::Stream);
        t.selection_update(0, 2);
        assert!(t.has_non_empty_selection());
    }

    #[test]
    fn m2_iter2_remediation_regressions() {
        // 1. RenderSnapshot::line_text skips SIZED_SPACER and matches visible_line_text
        let mut t = new_term(10, 4);
        t.advance(b"\x1b]66;s=2:w=1;A\x07");
        assert_eq!(t.visible_line_text(0).as_deref(), Some("A"));
        assert_eq!(t.render_snapshot_line_text(0), t.visible_line_text(0));
        assert_eq!(t.active_snapshot().line_text(0).as_deref(), Some("A"));

        // 2. OSC 66 ignores unknown metadata keys with non-integer values
        let mut t_meta = new_term(10, 4);
        t_meta.advance(b"\x1b]66;s=2:foo=bar:w=1;Z\x07");
        assert_eq!(t_meta.visible_line_text(0).as_deref(), Some("Z"));
        assert_eq!(
            t_meta.render_snapshot_line_text(0),
            t_meta.visible_line_text(0)
        );
        let extra_id = t_meta.grid().viewport_row(0).unwrap().cells()[0].extra_id;
        assert_eq!(
            t_meta
                .cell_extra(extra_id)
                .and_then(|e| e.text_sizing)
                .map(|ts| ts.scale),
            Some(2)
        );

        // 3. DECFRA: CSI $ x defaults Pch to 32 (' '), while explicit 0 or empty slot 0 is ignored
        let mut t_fra = new_term(6, 3);
        t_fra.advance(b"\x1b[35;1;1;2;4$x");
        assert_eq!(t_fra.visible_line_text(0).as_deref(), Some("####"));
        assert_eq!(t_fra.visible_line_text(1).as_deref(), Some("####"));
        t_fra.advance(b"\x1b[0;1;1;2;2$x\x1b[;1;1;2;2$x");
        assert_eq!(t_fra.visible_line_text(0).as_deref(), Some("####"));
        assert_eq!(t_fra.visible_line_text(1).as_deref(), Some("####"));
        t_fra.advance(b"\x1b[$x");
        assert_eq!(t_fra.visible_line_text(0).as_deref(), Some(""));
        assert_eq!(t_fra.visible_line_text(1).as_deref(), Some(""));
        assert_eq!(t_fra.cell_at(0, 0).unwrap().ch, ' ');

        // 4. DECCRA: non-existent page params >= 2 are ignored; 0 or 1 operate on current page
        let mut t_cra = new_term(8, 3);
        t_cra.advance(b"ABCD");
        t_cra.advance(b"\x1b[1;1;1;4;2;2;1;1$v");
        assert_eq!(t_cra.visible_line_text(1).as_deref(), Some(""));
        t_cra.advance(b"\x1b[1;1;1;4;1;2;1;2$v");
        assert_eq!(t_cra.visible_line_text(1).as_deref(), Some(""));
        t_cra.advance(b"\x1b[1;1;1;4;0;2;1;0$v");
        assert_eq!(t_cra.visible_line_text(1).as_deref(), Some("ABCD"));

        // 5. OSC 99 unknown p=<part> (p=icon / p=buttons) does not pollute title/body
        let mut t_99 = new_term(20, 4);
        t_99.advance(
            b"\x1b]99;i=x:p=title:d=0;RealTitle\x1b\\\
              \x1b]99;i=x:p=icon:d=0;ICONDATA\x1b\\\
              \x1b]99;i=x:p=buttons:d=0;BTN\x1b\\\
              \x1b]99;i=x:p=body:d=1;Body\x1b\\",
        );
        assert_eq!(
            t_99.drain_events(),
            vec![TerminalEvent::Notification {
                title: "RealTitle".to_owned(),
                body: "Body".to_owned(),
            }]
        );

        // 6. OSC 99 d=1 finalizer without ;payload (and d=1 on p=icon) flushes buffered id
        t_99.advance(b"\x1b]99;i=y:p=title:d=0;Hello\x1b\\\x1b]99;i=y:d=1\x1b\\");
        assert_eq!(
            t_99.drain_events(),
            vec![TerminalEvent::Notification {
                title: "Hello".to_owned(),
                body: String::new(),
            }]
        );
        t_99.advance(b"\x1b]99;i=w:p=title:d=0;Buffered\x1b\\\x1b]99;i=w:p=icon:d=1;ICON\x1b\\");
        assert_eq!(
            t_99.drain_events(),
            vec![TerminalEvent::Notification {
                title: "Buffered".to_owned(),
                body: String::new(),
            }]
        );
        t_99.advance(b"\x1b]99;i=missing:d=1\x1b\\");
        assert!(t_99.drain_events().is_empty());

        // 7. OSC 99 invalid i=<id> rejects the entire sequence instead of falling back to id=""
        t_99.advance(
            b"\x1b]99;p=title:d=0;ValidDefault\x1b\\\
              \x1b]99;i=bad/id:p=body:d=1;Hijacked\x1b\\",
        );
        assert!(t_99.drain_events().is_empty());
        t_99.advance(b"\x1b]99;p=body:d=1;RealBody\x1b\\");
        assert_eq!(
            t_99.drain_events(),
            vec![TerminalEvent::Notification {
                title: "ValidDefault".to_owned(),
                body: "RealBody".to_owned(),
            }]
        );

        // 8. Unconsumed SS2/SS3 single_shift is cleared by CSI, non-SS ESC, and C0/C1 controls
        //    (except NUL/DEL and 8-bit SS2/SS3 0x8E/0x8F)
        let mut t_ss = new_term(10, 4);
        t_ss.advance(b"\x1b(B\x1b*0\x1bN\x1b[2Cq");
        assert_eq!(t_ss.visible_line_text(0).as_deref(), Some("  q"));

        t_ss.advance(b"\r\n\x1b+0\x1bO\x1b7q");
        assert_eq!(t_ss.visible_line_text(1).as_deref(), Some("q"));

        t_ss.advance(b"\r\nA\x1bN\x08q");
        assert_eq!(t_ss.visible_line_text(2).as_deref(), Some("q"));

        // NUL and DEL do not cancel pending SS2
        t_ss.advance(b"\r\n\x1bN\x00q\x1bN\x7fq");
        assert_eq!(t_ss.visible_line_text(3).as_deref(), Some("──"));

        // Vt500EightBit: 0x8E arms SS2, while 0x85 (NEL) clears unconsumed SS2
        let mut t_c1 = Terminal::new(
            10,
            2,
            CoreConfig {
                c1_policy: C1Policy::Vt500EightBit,
                ..CoreConfig::default()
            },
        );
        t_c1.advance(b"\x1b*0\x8Eq\x8E\x85q");
        assert_eq!(t_c1.visible_line_text(0).as_deref(), Some("─"));
        assert_eq!(t_c1.visible_line_text(1).as_deref(), Some("q"));
    }

    #[cfg(feature = "search")]
    #[test]
    fn repro4a_incomplete_incremental_search_preserves_dirty_tracking_for_refresh() {
        let mut t1 = Terminal::new(20, 4, CoreConfig::default());
        t1.advance(b"foo one\r\n");
        assert_eq!(t1.search(&SearchQuery::literal("foo")).unwrap().len(), 1);
        t1.advance(b"foo two\r\n");

        // Starting an incomplete incremental search must not drain pending dirty lines.
        let prog = t1
            .begin_incremental_search(&SearchQuery::literal("two"))
            .unwrap();
        assert!(!prog.complete);

        assert!(t1.refresh_search().unwrap());
        assert_eq!(t1.search_matches().len(), 2);

        // Partial stepping before completion also preserves pending dirty lines on refresh.
        t1.advance(b"foo three\r\n");
        let p0 = t1
            .begin_incremental_search(&SearchQuery::literal("three"))
            .unwrap();
        assert!(!p0.complete);
        let p1 = t1.step_incremental_search(1);
        assert!(!p1.complete);

        assert!(t1.refresh_search().unwrap());
        assert_eq!(t1.search_matches().len(), 3);

        // Calling step_incremental_search without an active incremental search must not drain
        // pending dirty lines either.
        t1.advance(b"foo four\r\n");
        let idle = t1.step_incremental_search(10);
        assert!(idle.complete);
        assert!(t1.refresh_search().unwrap());
        assert_eq!(t1.search_matches().len(), 4);
    }

    #[cfg(feature = "search")]
    #[test]
    fn repro4b_invalid_regex_in_search_and_begin_incremental_preserves_dirty_tracking() {
        let mut t2 = Terminal::new(20, 4, CoreConfig::default());
        t2.advance(b"foo one\r\n");
        assert_eq!(t2.search(&SearchQuery::literal("foo")).unwrap().len(), 1);
        t2.advance(b"foo two\r\n");

        assert!(matches!(
            t2.begin_incremental_search(&SearchQuery::regex("[unclosed")),
            Err(SearchError::InvalidRegex(_))
        ));
        assert!(t2.refresh_search().unwrap());
        assert_eq!(t2.search_matches().len(), 2);

        // Synchronous search() returning InvalidRegex must also preserve pending dirty lines.
        t2.advance(b"foo three\r\n");
        assert!(matches!(
            t2.search(&SearchQuery::regex("[unclosed")),
            Err(SearchError::InvalidRegex(_))
        ));
        assert!(t2.refresh_search().unwrap());
        assert_eq!(t2.search_matches().len(), 3);
    }

    #[cfg(feature = "search")]
    #[test]
    fn incremental_search_completion_and_edge_cases_sync_dirty_tracking() {
        // Multi-step completion drains dirty tracking once complete, and later refresh picks up
        // subsequent mutations cleanly.
        let mut t = Terminal::new(20, 6, CoreConfig::default());
        t.advance(b"bar one\r\nother\r\nbar two\r\n");
        let mut prog = t
            .begin_incremental_search(&SearchQuery::literal("bar"))
            .unwrap();
        assert!(!prog.complete);
        while !prog.complete {
            prog = t.step_incremental_search(1);
        }
        assert_eq!(t.search_matches().len(), 2);

        t.advance(b"bar three\r\n");
        assert!(t.refresh_search().unwrap());
        assert_eq!(t.search_matches().len(), 3);

        // Mid-search grid mutation restarts the cursor and clears dirty tracking on completion.
        let mut p_mid = t
            .begin_incremental_search(&SearchQuery::literal("bar"))
            .unwrap();
        assert!(!p_mid.complete);
        t.advance(b"bar four\r\n");
        while !p_mid.complete {
            p_mid = t.step_incremental_search(2);
        }
        assert_eq!(t.search_matches().len(), 4);

        t.advance(b"bar five\r\n");
        assert!(t.refresh_search().unwrap());
        assert_eq!(t.search_matches().len(), 5);

        // 1-step immediate completion on empty grid and empty query syncs cleanly.
        let mut empty_grid_term = Terminal::new(20, 4, CoreConfig::default());
        let p_empty_grid = empty_grid_term
            .begin_incremental_search(&SearchQuery::literal("needle"))
            .unwrap();
        assert!(p_empty_grid.complete);
        assert!(empty_grid_term.search_matches().is_empty());
        empty_grid_term.advance(b"needle here\r\n");
        assert!(empty_grid_term.refresh_search().unwrap());
        assert_eq!(empty_grid_term.search_matches().len(), 1);

        let p_empty_query = empty_grid_term
            .begin_incremental_search(&SearchQuery::literal(""))
            .unwrap();
        assert!(p_empty_query.complete);
        assert!(empty_grid_term.search_matches().is_empty());
        empty_grid_term.advance(b"needle two\r\n");
        assert!(empty_grid_term.refresh_search().unwrap());
        assert!(empty_grid_term.search_matches().is_empty());
    }

    #[cfg(feature = "search")]
    #[test]
    fn repro5_invalid_regex_search_preserves_active_incremental_search_and_dirty_tracking() {
        // [REPRO-5]: Calling search(InvalidRegex) while an incremental search is active must
        // preserve both the active SearchCursor and incremental_search_active, allowing
        // step_incremental_search to finish and then sync dirty tracking cleanly.
        let mut t = Terminal::new(20, 4, CoreConfig::default());
        t.advance(b"foo one\r\nfoo two\r\n");
        let p0 = t
            .begin_incremental_search(&SearchQuery::literal("foo"))
            .unwrap();
        assert!(!p0.complete);
        assert!(matches!(
            t.search(&SearchQuery::regex("[unclosed")),
            Err(SearchError::InvalidRegex(_))
        ));

        let p1 = t.step_incremental_search(100);
        assert!(p1.complete);
        assert_eq!(p1.matches_found, 2);
        assert_eq!(t.search_matches().len(), 2);

        // Completion still drained dirty tracking because incremental_search_active remained true,
        // so subsequent mutations refresh incrementally without losing new lines.
        t.advance(b"foo three\r\n");
        assert!(t.refresh_search().unwrap());
        assert_eq!(t.search_matches().len(), 3);

        // If refresh_search() is called instead of step_incremental_search after InvalidRegex,
        // pending dirty tracking is also preserved so the refresh includes all new lines.
        let mut t2 = Terminal::new(20, 4, CoreConfig::default());
        t2.advance(b"foo one\r\n");
        assert_eq!(t2.search(&SearchQuery::literal("foo")).unwrap().len(), 1);
        t2.advance(b"foo two\r\n");
        let p_inc = t2
            .begin_incremental_search(&SearchQuery::literal("bar"))
            .unwrap();
        assert!(!p_inc.complete);
        assert!(matches!(
            t2.search(&SearchQuery::regex("[unclosed")),
            Err(SearchError::InvalidRegex(_))
        ));
        assert!(t2.refresh_search().unwrap());
        assert_eq!(t2.search_matches().len(), 2);
        let p_after = t2.step_incremental_search(100);
        assert!(p_after.complete);
        assert_eq!(p_after.matches_found, 2);
    }
}
