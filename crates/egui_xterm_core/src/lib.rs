//! Pure-Rust, WASM-safe terminal emulator core.
//!
//! `egui_xterm_core` provides the [`TerminalInput`] stream abstraction, compact
//! [`Cell`] and [`PackedColor`] data model, clean-room VT500 / ANSI [`Parser`],
//! scrollback ring buffer [`Grid`] with lossless resize reflow, split
//! [`TerminalState`] + [`RenderSnapshot`] view, and the full [`Terminal`]
//! state machine.
//!
//! # Examples
//!
//! Drive a split [`Parser`] and [`TerminalState`] directly and inspect the
//! zero-allocation [`RenderSnapshot`]:
//!
//! ```
//! use egui_xterm_core::{CoreConfig, Parser, TerminalEvent, TerminalState};
//!
//! let mut parser = Parser::new();
//! let mut state = TerminalState::new(40, 4, CoreConfig::default());
//!
//! // Combine SGR styling with OSC 5 (special colors), OSC 6 (document URI),
//! // OSC 9;4 (progress), and OSC 22 (pointer cursor shape).
//! let stream = b"\x1b]5;0;rgb:ff/80/00\x07\
//!                \x1b]6;file:///workspace/lib.rs\x07\
//!                \x1b]9;4;1;60\x07\
//!                \x1b]22;pointer\x07\
//!                \x1b[1;32mready\x1b[0m\r\n";
//! parser.advance(stream, &mut state);
//! state.post_advance();
//!
//! let snap = state.active_snapshot();
//! assert_eq!((snap.cols, snap.rows), (40, 4));
//! assert_eq!(snap.visible_rows.len(), 4);
//! assert_eq!(snap.line_text(0).as_deref(), Some("ready"));
//! assert_eq!((snap.cursor.col, snap.cursor.row), (0, 1));
//! assert_eq!(snap.special_colors[0], Some([0xFF, 0x80, 0x00]));
//! assert!(snap.palette_epoch >= 1);
//! assert_eq!(state.document_uri(), Some("file:///workspace/lib.rs"));
//!
//! let events = state.drain_events();
//! assert!(events.contains(&TerminalEvent::Progress {
//!     state: 1,
//!     progress: 60,
//! }));
//! assert!(events.contains(&TerminalEvent::MouseShape("pointer".to_owned())));
//! ```
//!
//! Use [`Terminal`] with DEC Mode 2026 synchronized output latching and
//! [`Osc52ReadPolicy`]:
//!
//! ```
//! use egui_xterm_core::{
//!     ClipboardSelection, CoreConfig, Osc52ReadPolicy, Terminal, TerminalEvent,
//! };
//!
//! let cfg = CoreConfig {
//!     osc52_read_policy: Osc52ReadPolicy::Prompt,
//!     ..CoreConfig::default()
//! };
//! let mut term = Terminal::new(40, 4, cfg);
//! term.advance_with_time(b"frame-0", 1.0);
//!
//! // While DEC ?2026h is latched, active_snapshot() stays frozen on "frame-0".
//! term.advance_with_time(b"\x1b[?2026h\r\x1b[2Kframe-1", 1.05);
//! assert!(term.is_synchronized_output());
//! assert_eq!(term.active_snapshot().line_text(0).as_deref(), Some("frame-0"));
//!
//! // Releasing DEC ?2026l exposes "frame-1" and processes queued events.
//! term.advance_with_time(b"\x1b[?2026l\x1b]52;c;?\x07", 1.10);
//! assert!(!term.is_synchronized_output());
//! assert_eq!(term.active_snapshot().line_text(0).as_deref(), Some("frame-1"));
//! assert!(term.drain_events().contains(&TerminalEvent::ClipboardReadPrompt {
//!     selection: ClipboardSelection::Clipboard,
//! }));
//! ```

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
