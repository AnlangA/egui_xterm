# `egui_xterm`

A clean-room, high-performance terminal widget and runtime for [`egui`](https://github.com/emilk/egui) `0.37` across native desktops (`Windows`, `macOS`, `Linux`) and `wasm32-unknown-unknown`.

Implement a single non-blocking `TerminalInput::poll_read` method (no `Send + Sync` bound required) and call `TerminalRuntime::show(ui)` inside any `egui::Frame` or panel. `egui_xterm` automatically detects container bounds, snaps cell geometry to `pixels_per_point()`, applies `ScaleMode::ReflowGrid` or `ScaleMode::AutoScaleToFit`, and renders via a coalesced `<16ms` mesh pipeline with inline graphics (`kitty-graphics` & `sixel`), hyperlinks, search, shell integration, IME, and clipboard support.

## Installation

Add `egui_xterm` to your `Cargo.toml` (Rust `1.95+`, Edition `2024`, `egui 0.37`):

```toml
[dependencies]
egui = "0.37"
egui_xterm = "0.3.0"

# Optional cross-platform PTY (desktop portable-pty + wasm32 WebSocket WebPtyBackend)
# and platform clipboard (desktop arboard + wasm32 Web Clipboard API):
# egui_xterm = { version = "0.3.0", features = ["pty", "native-clipboard"] }
```

## Quickstart

```rust
use std::collections::VecDeque;
use egui_xterm::{
    ScaleMode, TerminalConfig, TerminalInput, TerminalIoError, TerminalRuntime, TerminalSize,
};

struct EchoInput {
    rx: VecDeque<u8>,
}

impl EchoInput {
    fn new(banner: &str) -> Self {
        Self {
            rx: banner.as_bytes().iter().copied().collect(),
        }
    }
}

impl TerminalInput for EchoInput {
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
        let n = buf.len().min(self.rx.len());
        for (slot, byte) in buf[..n].iter_mut().zip(self.rx.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
        self.rx.extend(bytes);
        Ok(())
    }

    fn resize(&mut self, _size: TerminalSize) -> Result<(), TerminalIoError> {
        Ok(())
    }
}

fn show_terminal(ui: &mut egui::Ui, runtime: &mut TerminalRuntime<EchoInput>) {
    let mut config = TerminalConfig {
        scale_mode: ScaleMode::ReflowGrid,
        ..TerminalConfig::default()
    };
    egui::Frame::new().show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        let resp = runtime.show_with_config(ui, &mut config);
        let _dims = (resp.cols, resp.rows);
    });
}
```

## Layout Modes

- **`ScaleMode::ReflowGrid` *(default)***: Preserves `font_size * zoom_factor`, computes `(cols, rows)` from `ui.available_rect_before_wrap()`, triggers lossless primary-screen reflow, and debounces `TerminalInput::resize`.
- **`ScaleMode::AutoScaleToFit { cols, rows }`**: Locks logical `(cols, rows)` and dynamically scales the rendered font size to fit the frame rectangle.

## Feature Flags

| Feature | Default | WASM32 Safe | Description |
|---|---|---|---|
| `search` | **Yes** | Yes | Literal/regex/whole-word scrollback search and floating `search_bar` (`Ctrl+Shift+F`) |
| `hyperlinks` | **Yes** | Yes | `OSC 8` + implicit URLs, hover tooltip/underline, and click activation events |
| `shell-integration` | **Yes** | Yes | `OSC 133` prompt/input/output zones, `jump_to_prompt`, and `prompt_gutter` overlay |
| `kitty-keyboard` | **Yes** | Yes | Kitty progressive keyboard protocol (`CSI > / < / = / ? u`) and `modifyOtherKeys` |
| `kitty-graphics` | **Yes** | Yes | Kitty Graphics Protocol (`a=t/T/p/d/f/a/c/q`, animations, `U+10EEEE` placeholders, PNG/zlib; `t=f/t/s` disabled on `wasm32`) |
| `sixel` | **Yes** | Yes | Sixel inline graphics (`DCS ... q`, `DECSDM` `?80`, `?1070`, `?8452`, HLS/RGB registers) |
| `vector-glyphs` | **Yes** | Yes | Pixel-aligned analytical rendering for Box Drawing, Block Elements, Braille, and Powerline glyphs |
| `serde` | No | Yes | `Serialize`/`Deserialize` on `TerminalConfig`, `ScaleMode`, `Theme`, `ColorScheme`, and `TerminalSnapshot` |
| `metrics` | No | Yes | Lightweight `TerminalMetrics` throughput and frame counters |
| `pty` | No | **Yes (Desktop + Web)** | Re-exports `egui_xterm_pty` as `egui_xterm::pty` (desktop `portable-pty` `PtyBackend`, `wasm32` WebSocket `WebPtyBackend`, and cross-platform `protocol` + `WebPtySession`) |
| `native-clipboard` | No | **Yes (Desktop + Web)** | Platform clipboard integration outside `egui::Event::Paste` (`arboard` on desktop, async Web Clipboard API `navigator.clipboard` on `wasm32`) |

## `wasm32-unknown-unknown` Verification

All features of `egui_xterm` compile cleanly for WebAssembly (`#![forbid(unsafe_code)]`):

```sh
cargo check -p egui_xterm --target wasm32-unknown-unknown --all-features
```

[API documentation](https://docs.rs/egui_xterm) · [Architecture guide](https://github.com/AnlangA/egui_xterm/blob/main/docs/ARCHITECTURE.md) · [Modern terminal gap analysis](https://github.com/AnlangA/egui_xterm/blob/main/docs/MODERN_TERMINAL_GAP_ANALYSIS.md)

Licensed under either the MIT License or Apache License, Version 2.0, at your option.
