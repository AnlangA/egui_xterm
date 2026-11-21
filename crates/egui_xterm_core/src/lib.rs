//! Pure-Rust, WASM-safe terminal emulator core.
//!
//! `egui_xterm_core` provides the [`TerminalInput`] stream abstraction, compact
//! [`Cell`] and [`PackedColor`] data model, clean-room VT500 / ANSI [`Parser`],
//! scrollback ring buffer [`Grid`] with lossless resize reflow, and the full
//! [`Terminal`] state machine.

#![forbid(unsafe_code)]

pub mod cell;
pub mod color;
pub mod features;
pub mod grid;
pub mod input;
pub mod parser;
pub mod term;

pub use crate::cell::{
    Cell, CellExtra, CellFlags, Hyperlink, HyperlinkId, HyperlinkPool, MAX_HYPERLINK_ID_BYTES,
    MAX_HYPERLINK_URI_BYTES, TextSizing, UnderlineStyle,
};
pub use crate::color::{ColorScheme, PackedColor, SpecialColors, Theme};
#[cfg(feature = "hyperlinks")]
pub use crate::features::hyperlinks::{HyperlinkInfo, HyperlinkSpan};
#[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
pub use crate::features::inline_images::{
    DEFAULT_IMAGE_MEMORY_BUDGET, ImageFormat, ImagePlacement, ImageStore, StoredImage,
};
#[cfg(feature = "kitty-graphics")]
pub use crate::features::inline_images::{
    KITTY_DIACRITICS, KITTY_PLACEHOLDER_CHAR, KittyFileMediumPolicy, KittyPlaceholderCell,
    KittyShmReader, KittyTransportPolicy, decode_placeholder_cell, diacritic_index,
};
#[cfg(feature = "kitty-keyboard")]
pub use crate::features::kitty_keyboard::{
    KITTY_STACK_CAP, KittyEventType, KittyKey, KittyKeyEvent, KittyKeyboardFlags, KittyModifiers,
    ModifyOtherKeys, encode_kitty_key, encode_kitty_key_into, encode_modify_other_keys_into,
};
#[cfg(feature = "metrics")]
pub use crate::features::metrics::TerminalMetrics;
#[cfg(feature = "search")]
pub use crate::features::search::{SearchError, SearchMatch, SearchQuery, SearchStepProgress};
#[cfg(feature = "shell-integration")]
pub use crate::features::shell_integration::{ShellZone, ShellZonesIter, ShellZonesView, ZoneKind};
pub use crate::grid::{
    AbsLine, CellExtraTable, DirtyTracker, Grid, GridPoint, ROW_POOL_CAP, ReflowAnchors,
    ReflowResult, Row, ScreenKind, ViewportPos,
};
pub use crate::input::{
    ChannelInput, FnInput, RingBufferInput, TerminalInput, TerminalIoError, TerminalSize,
};
pub use crate::parser::{C1Policy, CsiParams, OscTerminator, Parser, VtHandler};
#[cfg(feature = "serde")]
pub use crate::term::TerminalSnapshot;
pub use crate::term::{
    CellSnapshot, Charset, CharsetSlots, ClipboardSelection, CoreConfig, Cursor, CursorAttrs,
    CursorShape, CursorSnapshot, CursorStyle, Modes, MouseButton, MouseEncoding, MouseEventKind,
    MouseInputEvent, MouseModifiers, MouseProtocol, Osc52ReadPolicy, PaletteOverrides,
    RenderSnapshot, SavedCursor, Selection, SelectionKind, SelectionMode, TabStops, Terminal,
    TerminalEvent, TerminalState, VisibleRowsIter, VisibleRowsView, encode_mouse_event,
    encode_mouse_event_into, encode_mouse_event_pixels, encode_mouse_event_pixels_into,
    focus_event_bytes, format_paste_bytes, format_paste_bytes_into,
};
