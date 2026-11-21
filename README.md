# `egui_xterm`

A clean-room, high-performance, multi-crate Rust terminal emulator workspace built for seamless integration into [`egui`](https://github.com/emilk/egui) `0.37` applications across native desktops (`Windows`, `macOS`, `Linux`) and `wasm32-unknown-unknown`.

Implement a single non-blocking `TerminalInput::poll_read` method for your byte source, drop `TerminalRuntime::show(ui)` inside any `egui::Frame` or panel, and `egui_xterm` automatically handles container sizing, DPI pixel snapping, resize reflow or font auto-scaling, VT500 parsing, selection, clipboard, IME, mouse tracking, and cached `<16ms` rendering.

---

## Clean-Room Design Highlights

Every line of VT parsing, grid storage, resize reflow, `egui` rendering, and PTY orchestration in this workspace is **clean-room original Rust** (`#![forbid(unsafe_code)]` across `egui_xterm_core`, `egui_xterm`, `egui_xterm_pty`, and `egui_xterm_demo`):

- **Zero-Boilerplate Input Trait (`TerminalInput`)**: Only `poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError>` is required; `write_bytes`, `resize`, and `is_closed` default to no-ops. No `Send + Sync` bound is imposed, making it trivial to wrap `Rc<RefCell<_>>`, WebSockets, in-process shells, or OS PTYs. Built-in adapters (`FnInput`, `ChannelInput`, `RingBufferInput`) and blanket `Box<T>` / `&mut T` impls are included.
- **Frame-Aware Auto-Layout & Auto-Scaling**: Calling `TerminalRuntime::show(ui)` inside an `egui::Frame`, panel, or window reads `ui.available_rect_before_wrap()`, snaps origin and cell metrics to `ctx.pixels_per_point()`, and adapts via:
  - `ScaleMode::ReflowGrid` *(default)* — preserves `font_size * zoom_factor`, computes `(cols, rows)` from the frame's inner rect, triggers lossless primary-screen line reflow/auto-wrap, and debounces `TerminalInput::resize` notifications.
  - `ScaleMode::AutoScaleToFit { cols, rows }` — locks logical `(cols, rows)` and dynamically scales the effective font size to fit the container rect.
- **VT500 Engine & Modern Protocol Support**:
  - 14-state DEC VT500 parser with SWAR ASCII `Ground` fast-path, `memchr` skipping, streaming UTF-8 decoding, and `C1Policy::Utf8Only` default.
  - Compact 20-byte `Copy` `Cell` with sparse `CellExtra` side-table (combining/ZWJ grapheme clusters via `unicode-segmentation` + `unicode-width`, `SGR 58` underline colors, interned `OSC 8` hyperlinks).
  - 16 / 256 / 24-bit TrueColor (`;` and `:` subparameters), 5 underline styles (`4:1..5`), ring scrollback with row pooling, primary & alternate screens (`?47`/`?1047`/`?1049`), DEC Mode 2026 synchronized output with watchdog, selection (stream/word/line/block), and X10/`1000`/`1002`/`1003`/SGR `1006`/UTF-8 `1005`/`1007` mouse modes.
- **4-Stage `<16ms` `egui` Renderer**: Coalesced background/selection `epaint::Mesh`, per-row `generation`-keyed `RowRenderCache` (`Arc<Galley>`) with font-atlas reset detection, pixel-aligned `vector-glyphs` (Box Drawing, Block Elements, Braille, underlines, cursor), and reused frame scratch buffers.
- **WASM32-Safe by Default**: Pure-Rust `egui_xterm_core` and default `egui_xterm` features compile cleanly to `wasm32-unknown-unknown`. Desktop-only OS integrations (`pty`, `native-clipboard`) live behind opt-in features.

---

## Workspace Organization

| Crate / Path | Role | WASM32 Safe |
|---|---|---|
| [`egui_xterm_core`](https://crates.io/crates/egui_xterm_core) | Pure-Rust VT500 parser, 20B `Cell`, ring scrollback `Grid`, lossless reflow, `Terminal`, `TerminalInput` & built-in adapters, and feature-gated protocol extensions | Yes (always) |
| [`egui_xterm`](https://crates.io/crates/egui_xterm) | `egui 0.37` `TerminalRuntime<I>` & `TerminalWidget`, `TerminalConfig`, `ScaleMode`, pixel-snapped auto-layout, 4-stage cached renderer, IME/clipboard/mouse mapping, and UI overlays (`search_bar`, `scrollbar`, `context_menu`, `prompt_gutter`, `hyperlink`) | Yes (default features) |
| [`egui_xterm_pty`](https://crates.io/crates/egui_xterm_pty) | Desktop `portable-pty 0.9` `PtyBackend` implementing `TerminalInput` with background reader thread, repaint callback, cross-platform default shell resolution, and Windows ConPTY handshake handling (re-exported as `egui_xterm::pty` under `pty`) | Re-exports core types only on `wasm32` |
| [`crates/egui_xterm_e2e`](https://github.com/AnlangA/egui_xterm/tree/main/crates/egui_xterm_e2e) | Opaque-box end-to-end test suite (`publish = false`) | Test harness |
| [`examples/egui_xterm_demo`](https://github.com/AnlangA/egui_xterm/tree/main/examples/egui_xterm_demo) | Runnable `eframe 0.37` demo app (`publish = false`) with local PTY tabs (desktop), cross-platform trait-driven `DemoShellInput`, split-pane auto-layout, and live `ScaleMode`/theme/cursor controls | Yes (`--target wasm32-unknown-unknown` checks cleanly) |

See the [architecture guide](https://github.com/AnlangA/egui_xterm/blob/main/docs/ARCHITECTURE.md) and the [widget API documentation](https://docs.rs/egui_xterm).

---

## Quickstart

Add `egui_xterm` to your `Cargo.toml` (Rust `1.95+`, Edition `2024`, `egui 0.37`):

```toml
[dependencies]
egui = "0.37"
egui_xterm = "0.1.1"

# Optional on desktop targets if you want built-in OS PTY & arboard clipboard support:
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
egui_xterm = { version = "0.1.1", features = ["pty", "native-clipboard"] }
```

### 1. Implement `TerminalInput` (or use `RingBufferInput` / `ChannelInput` / `FnInput`)

```rust
use std::collections::VecDeque;
use egui_xterm::{TerminalInput, TerminalIoError, TerminalSize};

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
        for slot in &mut buf[..n] {
            *slot = self.rx.pop_front().expect("bounded by rx.len()");
        }
        Ok(n)
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
        self.rx.extend(bytes);
        Ok(())
    }

    fn resize(&mut self, _size: TerminalSize) -> Result<(), TerminalIoError> {
        // Debounced grid & pixel size notifications arrive here automatically.
        Ok(())
    }
}
```

### 2. Call `TerminalRuntime::show(ui)` inside an `egui::Frame`

```rust
use egui_xterm::{ScaleMode, TerminalConfig, TerminalRuntime};

// Construct once in your app state:
let mut config = TerminalConfig {
    scale_mode: ScaleMode::ReflowGrid, // or ScaleMode::AutoScaleToFit { cols: 80, rows: 24 }
    ..TerminalConfig::default()
};
let mut runtime = TerminalRuntime::with_config(EchoInput::new("Hello egui_xterm!\r\n"), config.clone());

// Inside your eframe::App::ui / egui frame:
egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(18, 20, 24))
        .inner_margin(egui::Margin::same(4))
        .show(ui, |ui| {
            // Ensure the frame occupies the available container area so the terminal auto-sizes to it.
            ui.set_min_size(ui.available_size());

            let resp = runtime.show_with_config(ui, &mut config);
            // Or `runtime.show(ui)` using the runtime's internal TerminalConfig,
            // or `ui.add(runtime.widget())`.
            let _grid_dims = (resp.cols, resp.rows);
        });
});
```

- **Auto-Layout & Auto-Wrap/Reflow**: Under `ScaleMode::ReflowGrid`, resizing the enclosing `egui::Frame` or changing `font_size` / `zoom_factor` / `pixels_per_point()` immediately recalculates `(cols, rows)`, reflows wrapped primary-screen lines without losing logical newlines, and dispatches a debounced `TerminalInput::resize` (`TerminalSize { cols, rows, pixel_width, pixel_height }`).
- **Auto-Scale to Fixed Grid**: Under `ScaleMode::AutoScaleToFit { cols, rows }`, the logical grid stays pinned to `(cols, rows)` and the rendered font size scales dynamically to fit the frame rectangle.
- **Idle input** — The runtime asks `egui` for another frame after 50 ms by default, so an asynchronous data source can implement only `poll_read`. Set `TerminalConfig::idle_poll_interval_ms` to `0` when your producer calls `egui::Context::request_repaint()` on output and EOF.
- **Desktop PTY** — With the `pty` feature enabled, pass `egui_xterm::pty::PtyBackend::spawn(PtyConfig::default())?` into `TerminalRuntime::new(...)`. For immediate wakeups with idle polling disabled, use `PtyBackend::spawn_with_repaint(PtyConfig::default(), Box::new(move || ctx.request_repaint()))?`.

---

## Feature Flags

### `egui_xterm` & `egui_xterm_core` Features

| Feature | Crates | Default | WASM32 Safe | Description |
|---|---|---|---|---|
| `search` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | `memchr` literal & `regex` scrollback/grid search plus floating `search_bar` overlay (`Ctrl+Shift+F`) |
| `hyperlinks` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | Deduplicated `OSC 8` `HyperlinkPool`, implicit URL detection, hover underline/tooltip, and click activation events |
| `shell-integration` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | `OSC 133` A/B/C/D semantic prompt/input/output zones, `jump_to_prompt`, `command_output_at`, and exit status events |
| `kitty-keyboard` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | Kitty progressive keyboard protocol (`CSI > / < / ? u`) per-screen mode stacks and `CSI u` key encoding |
| `vector-glyphs` | `egui_xterm` | **Yes** | Yes | Pixel-snapped analytical rendering for Box Drawing (`U+2500..=U+257F`), Block Elements (`U+2580..=U+259F`), and Braille (`U+2800..=U+28FF`) |
| `serde` | `egui_xterm_core`, `egui_xterm` | No | Yes | `serde::Serialize` / `Deserialize` derives on sizes, colors, cells, `Theme`, `ColorScheme`, `TerminalConfig`, `ScaleMode`, and `TerminalSnapshot` |
| `metrics` | `egui_xterm_core`, `egui_xterm` | No | Yes | Lightweight `TerminalMetrics` counters (`bytes_processed`, `frames_rendered`, `dirty_rows_rendered`, `reflow_count`, etc.) |
| `pty` | `egui_xterm` | No | **No (Desktop)** | Enables `egui_xterm::pty` (`egui_xterm_pty` / `portable-pty 0.9` `PtyBackend`) on `not(target_arch = "wasm32")` |
| `native-clipboard` | `egui_xterm` | No | **No (Desktop)** | Enables `arboard` OS clipboard fallback for context-menu paste outside `egui::Event::Paste` on `not(target_arch = "wasm32")` |

### `egui_xterm_demo` Features

- **`default`**: `["search", "hyperlinks", "shell-integration", "kitty-keyboard", "vector-glyphs", "metrics"]` (plus `pty` and `native-clipboard` automatically on `not(target_arch = "wasm32")`).
- **`inspection`**: Enables `eframe/inspection` for live widget-tree inspection.

---

## Cross-Platform & `wasm32-unknown-unknown` Builds

### Native Desktop (`Windows` / `macOS` / `Linux`)

```bash
cargo check --workspace --all-features
cargo doc --workspace --no-deps --all-features
```

### `wasm32-unknown-unknown`

Install the target once if needed (`rustup target add wasm32-unknown-unknown`), then verify WASM compilation:

```bash
# Core engine (with all WASM-safe features)
cargo check -p egui_xterm_core --all-features --target wasm32-unknown-unknown

# egui widget/runtime (default features are 100% WASM-safe; also supports serde + metrics)
cargo check -p egui_xterm --target wasm32-unknown-unknown
cargo check -p egui_xterm --features "serde,metrics" --target wasm32-unknown-unknown

# Minimal core/widget build without optional features
cargo check -p egui_xterm_core --no-default-features --target wasm32-unknown-unknown
cargo check -p egui_xterm --no-default-features --target wasm32-unknown-unknown

# Demo app compiles cleanly against wasm32-unknown-unknown
cargo check -p egui_xterm_demo --target wasm32-unknown-unknown
```

> Avoid enabling `pty` or `native-clipboard` when targeting `wasm32-unknown-unknown` if you depend directly on native OS PTY or `arboard` APIs.

---

## Running the Demo App

Launch the interactive `eframe 0.37` demo application:

```bash
cargo run -p egui_xterm_demo
```

The demo includes:
- **Local PTY tab** *(desktop)*: Live OS shell via `PtyBackend::spawn_with_repaint`, with `"New PTY"` and `"Restart PTY"` actions.
- **Trait Demo tab** *(all targets)*: Interactive `DemoShellInput` implementing `TerminalInput`, with a collapsible **Trait Showcase** sidebar (`help`, `colors`, `unicode`, `links`, `prompts`, `sync`, `size`, `scroll`, `clear`).
- **Split View & Auto-Layout controls**: Side-by-side resizable panes, live switching between `ScaleMode::ReflowGrid` and `ScaleMode::AutoScaleToFit`, font size & zoom controls, Dark/Light themes, cursor shape/blink toggles, search overlay (`Find` / `Ctrl+Shift+F`), prompt navigation (`Prev Prompt` / `Next Prompt`), and live status/metrics bar.

---

## Tests & Benchmarks

### Running Tests & Lints

```bash
# Formatting & Clippy
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Full workspace & opaque-box E2E suites (518 workspace --all-features tests; 317 E2E tests across Tiers 1–5)
cargo test --workspace
cargo test --workspace --all-features
cargo test -p egui_xterm_e2e --all-features
cargo test -p egui_xterm_e2e --test tier5_adversarial --all-features   # 22 Tier 5 adversarial tests

# Crate unit, headless egui, and desktop PTY tests
cargo test -p egui_xterm_core --all-features   # 111 tests
cargo test -p egui_xterm --all-features        # 66 tests (2 unit + 64 headless_runtime)
cargo test -p egui_xterm --no-default-features
cargo test -p egui_xterm_pty                   # 13 tests (4 unit + 9 integration)
cargo test -p egui_xterm_demo                  # 9 tests
```

### Running Benchmarks

Run both Criterion `--release` benchmark suites sequentially (with `RUSTFLAGS` unset):

```bash
# 200x60 TrueColor single-frame update, auto-layout, 4-stage cached render, and epaint::Tessellator
cargo bench -p egui_xterm --bench frame_render

# Pure VT500 parser throughput, full Terminal::advance ingestion, and 10k-line scrollback reflow
cargo bench -p egui_xterm_core --bench vt_throughput
```

Summary of post-Phase-2 `--release` measurements (`Intel Core Ultra 9 285K`, Windows 11, `< 16.0 ms` 60 fps frame target):

| Suite / Benchmark | Time Estimate | Throughput / Headroom |
|---|---|---|
| `frame_render_200x60_truecolor/idle` | `1.4623 ms` | `8.2061 Melem/s` (`10.94x` below `16.0 ms`) |
| `frame_render_200x60_truecolor/1_dirty_row` | `1.4645 ms` | `8.1942 Melem/s` (`10.93x` below `16.0 ms`) |
| `frame_render_200x60_truecolor/scroll_1_line` | `1.7502 ms` | `6.8564 Melem/s` (`9.14x` below `16.0 ms`) |
| `frame_render_200x60_truecolor/full_redraw` | `3.3178 ms` | `3.6168 Melem/s` (`4.82x` below `16.0 ms`) |
| `vt_stream_throughput/parser_only/ascii_plain` | `69.126 µs` | `7.0640 GiB/s` |
| `vt_stream_throughput/parser_only/utf8_mixed` | `1.0482 ms` | `477.05 MiB/s` |
| `vt_stream_throughput/parser_only/truecolor_sgr` | `1.2384 ms` | `403.79 MiB/s` |
| `vt_stream_throughput/parser_only/osc133_dense` | `335.49 µs` | `995.32 MiB/s` |
| `vt_stream_throughput/terminal/ascii_plain` | `3.7853 ms` | `132.10 MiB/s` |
| `vt_stream_throughput/terminal/utf8_mixed` | `6.2204 ms` | `80.383 MiB/s` |
| `vt_stream_throughput/terminal/truecolor_sgr` | `2.7985 ms` | `178.70 MiB/s` |
| `vt_stream_throughput/terminal/osc133_dense` | `3.3156 ms` | `100.71 MiB/s` |
| `reflow_10k_scrollback/narrow_120x40_to_80x40` | `12.720 ms` | `786.18 Kelem/s` |
| `reflow_10k_scrollback/widen_80x40_to_160x40` | `6.1975 ms` | `1.6136 Melem/s` |

For architecture details and full raw Criterion reports, see:
- [Architecture guide](https://github.com/AnlangA/egui_xterm/blob/main/docs/ARCHITECTURE.md)
- [Benchmark results](https://github.com/AnlangA/egui_xterm/blob/main/docs/BENCHMARKS.md)

---

## License

Licensed under either the [MIT License](https://github.com/AnlangA/egui_xterm/blob/main/LICENSE-MIT) or the [Apache License, Version 2.0](https://github.com/AnlangA/egui_xterm/blob/main/LICENSE-APACHE), at your option.
