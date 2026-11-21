//! High-performance, auto-layout terminal widget and runtime for [`egui`].
//!
//! Consumers provide an implementation of [`TerminalInput`] (such as [`ChannelInput`],
//! [`RingBufferInput`], [`FnInput`], or a custom non-blocking stream) and call
//! [`TerminalRuntime::show`] inside any `egui` frame or panel. Layout sizing, pixel snapping,
//! resize reflow, VT500 parsing, selection, clipboard, IME, and 4-stage cached rendering happen
//! automatically.

#![forbid(unsafe_code)]

pub mod config;
mod input_map;
mod layout;
mod renderer;
pub mod runtime;
pub mod widgets;

pub use crate::config::{
    CopyBehavior, CursorRenderConfig, ScaleMode, TerminalConfig, TerminalResponse,
};
pub use crate::runtime::{TerminalRuntime, TerminalWidget};

pub use egui_xterm_core::{
    CellFlags, CellSnapshot, ChannelInput, ColorScheme, CoreConfig, CursorShape, FnInput,
    PaletteOverrides, RingBufferInput, SelectionKind, SelectionMode, Terminal, TerminalEvent,
    TerminalInput, TerminalIoError, TerminalSize, Theme, UnderlineStyle,
};

#[cfg(feature = "metrics")]
pub use egui_xterm_core::TerminalMetrics;
#[cfg(feature = "serde")]
pub use egui_xterm_core::TerminalSnapshot;
#[cfg(feature = "hyperlinks")]
pub use egui_xterm_core::{HyperlinkInfo, HyperlinkSpan};
#[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
pub use egui_xterm_core::{ImageFormat, ImagePlacement, StoredImage};
#[cfg(feature = "kitty-keyboard")]
pub use egui_xterm_core::{
    KITTY_STACK_CAP, KittyEventType, KittyKey, KittyKeyEvent, KittyKeyboardFlags, KittyModifiers,
    encode_kitty_key,
};
#[cfg(feature = "search")]
pub use egui_xterm_core::{SearchError, SearchMatch, SearchQuery};
#[cfg(feature = "shell-integration")]
pub use egui_xterm_core::{ShellZone, ZoneKind};

#[cfg(all(feature = "pty", not(target_arch = "wasm32")))]
pub use egui_xterm_pty as pty;
