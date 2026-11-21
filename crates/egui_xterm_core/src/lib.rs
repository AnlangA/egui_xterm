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
    MAX_HYPERLINK_URI_BYTES, UnderlineStyle,
};
pub use crate::color::{ColorScheme, PackedColor, Theme};
#[cfg(feature = "hyperlinks")]
pub use crate::features::hyperlinks::{HyperlinkInfo, HyperlinkSpan};
#[cfg(feature = "kitty-keyboard")]
pub use crate::features::kitty_keyboard::{
    KITTY_STACK_CAP, KittyEventType, KittyKey, KittyKeyEvent, KittyKeyboardFlags, KittyModifiers,
    encode_kitty_key,
};
#[cfg(feature = "metrics")]
pub use crate::features::metrics::TerminalMetrics;
#[cfg(feature = "search")]
pub use crate::features::search::{SearchError, SearchMatch, SearchQuery};
#[cfg(feature = "shell-integration")]
pub use crate::features::shell_integration::{ShellZone, ZoneKind};
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
    MouseInputEvent, MouseModifiers, MouseProtocol, PaletteOverrides, RenderSnapshot, SavedCursor,
    Selection, SelectionKind, SelectionMode, TabStops, Terminal, TerminalEvent, encode_mouse_event,
    encode_mouse_event_pixels, focus_event_bytes, format_paste_bytes,
};
