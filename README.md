# `egui_xterm`

A clean-room, high-performance, multi-crate Rust terminal emulator workspace built for seamless integration into [`egui`](https://github.com/emilk/egui) `0.37` applications across native desktops (`Windows`, `macOS`, `Linux`) and `wasm32-unknown-unknown`.

Implement a single non-blocking `TerminalInput::poll_read` method for your byte source, drop `TerminalRuntime::show(ui)` inside any `egui::Frame` or panel, and `egui_xterm` automatically handles container sizing, DPI pixel snapping, resize reflow or font auto-scaling, VT500 + modern terminal protocols, inline graphics, selection, clipboard, IME, mouse tracking, and coalesced `<16ms` mesh rendering.

---

## Clean-Room Design Highlights

Every line of VT parsing, grid storage, resize reflow, inline image decoding, `egui` rendering, and PTY orchestration in this workspace is **clean-room original Rust** (`#![forbid(unsafe_code)]` across `egui_xterm_core`, `egui_xterm`, `egui_xterm_pty`, and `egui_xterm_demo`):

- **Zero-Boilerplate Input Trait (`TerminalInput`)**: Only `poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError>` is required; `write_bytes`, `resize`, and `is_closed` default to no-ops. No `Send + Sync` bound is imposed, making it trivial to wrap `Rc<RefCell<_>>`, WebSockets, in-process shells, or OS PTYs. Built-in adapters (`FnInput`, `ChannelInput`, `RingBufferInput`) and blanket `Box<T>` / `&mut T` impls are included, while non-fatal transport errors are surfaced via `TerminalRuntime::last_io_error()` and `take_io_error()`.
- **Frame-Aware Auto-Layout & Auto-Scaling**: Calling `TerminalRuntime::show(ui)` inside an `egui::Frame`, panel, or window reads `ui.available_rect_before_wrap()`, snaps origin and cell metrics to `ctx.pixels_per_point()`, syncs viewport and cell pixel dimensions (`set_pixel_size` / `set_cell_pixel_size`), and adapts via:
  - `ScaleMode::ReflowGrid` *(default)* — preserves `font_size * zoom_factor`, computes `(cols, rows)` from the frame's inner rect, triggers lossless primary-screen line reflow/auto-wrap (`O(Z log Z + L)` with `OSC 133` & image anchors), and debounces `TerminalInput::resize` notifications.
  - `ScaleMode::AutoScaleToFit { cols, rows }` — locks logical `(cols, rows)` and dynamically scales the effective font size to fit the container rect.
- **VT500 Engine & Modern Terminal Protocols**:
  - Split `Parser` + `TerminalState` (eliminating per-`advance` `mem::take`), 14-state DEC VT500 machine with 8-byte SWAR ASCII `Ground` fast-path, batch `print_utf8_run`, `memchr` OSC/DCS/APC skipping, CSI digit fast-path, zero-copy OSC slice dispatch, and `C1Policy::Utf8Only` default.
  - Compact 20-byte `Copy` `Cell` with sparse `CellExtraTable` slot reuse (combining/ZWJ grapheme clusters via `unicode-segmentation` + `unicode-width`, `SGR 58` underline colors, interned `OSC 8` hyperlinks, `OSC 66` `TextSizing` metadata).
  - Full 16 / 256 / 24-bit TrueColor (`;` and `:` subparameters), `OSC 5`/`105` special SGR colors, 5 underline styles (`4:1..=5`), `SL`/`SR`, `DECSCA` + `DECSED`/`DECSEL`, rectangular ops (`DECCRA`/`DECERA`/`DECFRA`/`DECSERA`), `XTPUSHSGR`/`XTPOPSGR`, `XTWINOPS`, `XTGETTCAP`, `DECRQSS`, `XTVERSION`, `G0..G3` charsets (`LS*`/`SS*`), `DECALN`, `DECBI`/`DECFI`, `DECLRMM`/`DECSLRM`, ring scrollback with row pooling, primary & alternate screens (`?47`/`?1047`/`?1049`), DEC Mode 2026 synchronized output with persistent `LatchedFrame` reuse, selection (stream/word/line/block), `OSC 6` document URI, `OSC 9;4` progress, `OSC 22` pointer shapes, `OSC 52` read policy (`Deny`/`Allow`/`Prompt`), `OSC 99` rich notifications, `OSC 133` shell integration, `modifyOtherKeys`, Kitty keyboard, and mouse tracking including `DEC ?1016` SGR-pixels.
- **Inline Graphics (`kitty-graphics` & `sixel`)**: Pure-Rust, zero-external-dependency decoders (base64, RFC 1950/1951 zlib inflate, IEEE CRC-32 + Adam7 PNG, and `O(1)`-amortized row-based Sixel) backed by a 64 MiB LRU `ImageStore` with `StoredImage::revision` invalidation, Kitty `t=d/f/t/s` mediums (policy-gated), concurrent `m=1`/`m=0` chunking, animations (`a=f/a/c`), `U=1` / `U+10EEEE` Unicode placeholders, `P/Q/H/V` relative placements, and Sixel `DECSDM` (`?80`) / `?1070` / `?8452`.
- **Coalesced `<16ms` `egui` Mesh Renderer & PTY Pipeline**: Coalesced `bg_mesh` + `text_mesh: Arc<Mesh>` + `overlay_mesh: Arc<Mesh>` pipeline with `CachedRowSlot` in-place 1-row patching, decoupled cursor quad, direct ASCII mesh emission without `LayoutJob` clones, zero-alloc `cluster_cache`, bold/faux-bold font rendering, `vector-glyphs`, configurable visual bell, `OSC 9;4` top-edge progress bar, `OSC 22` pointer cursor integration, and recycled read/write buffer pools with a dedicated non-blocking writer thread in `PtyBackend`.
- **WASM32-Safe Across All Features**: Pure-Rust `egui_xterm_core` and every `egui_xterm` feature (including `pty` via browser `WebSocket` `WebPtyBackend` and `native-clipboard` via the browser Web Clipboard API) compile cleanly to `wasm32-unknown-unknown`.

---

## Workspace Organization

| Crate / Path | Role | WASM32 Safe |
|---|---|---|
| [`egui_xterm_core`](https://crates.io/crates/egui_xterm_core) | Pure-Rust VT500 parser, 20B `Cell`, ring scrollback `Grid`, lossless reflow, `Terminal` / `TerminalState`, `TerminalInput` & built-in adapters, `ImageStore`, and feature-gated protocol extensions | Yes (always) |
| [`egui_xterm`](https://crates.io/crates/egui_xterm) | `egui 0.37` `TerminalRuntime<I>` & `TerminalWidget`, `TerminalConfig`, `ScaleMode`, pixel-snapped auto-layout, coalesced mesh renderer, IME/clipboard (`arboard` on desktop, Web Clipboard API on `wasm32`)/mouse mapping, and UI overlays (`search_bar`, `scrollbar`, `context_menu`, `prompt_gutter`, `hyperlink`, `progress_bar`) | Yes (all features) |
| [`egui_xterm_pty`](https://crates.io/crates/egui_xterm_pty) | Cross-platform PTY crate implementing `TerminalInput` (re-exported as `egui_xterm::pty` under `pty`): desktop `portable-pty 0.9` `PtyBackend` (recycled read/write buffer pools, background reader/writer threads, repaint callback, default shell resolution, Windows ConPTY handshake), `wasm32` WebSocket `WebPtyBackend`, and cross-platform `protocol` + `WebPtySession` (`TaggedBinary`, `Ttyd`, `JsonResize`, `Raw`) | Yes (`WebPtyBackend` + `protocol` + `WebPtySession` + `PtyBackend` stub) |
| [`crates/egui_xterm_e2e`](https://github.com/AnlangA/egui_xterm/tree/main/crates/egui_xterm_e2e) | Opaque-box end-to-end test suite (`publish = false`) across Tiers 1–5 | Test harness |
| [`examples/egui_xterm_demo`](https://github.com/AnlangA/egui_xterm/tree/main/examples/egui_xterm_demo) | Runnable `eframe 0.37` demo app (`publish = false`) with local PTY tabs (desktop), WebSocket Web PTY tabs (`wasm32`), cross-platform trait-driven `DemoShellInput`, split-pane auto-layout, and live `ScaleMode`/theme/cursor/clipboard controls | Yes (`--target wasm32-unknown-unknown` checks cleanly) |
| [`examples/egui_xterm_web_gateway`](https://github.com/AnlangA/egui_xterm/tree/main/examples/egui_xterm_web_gateway) | Clean-room native WebSocket PTY gateway (`publish = false`, zero extra deps) bridging browser `WebPtyBackend` sessions to `PtyBackend` (`ws://127.0.0.1:7681/ws`) | Server binary |

See the [architecture guide](docs/ARCHITECTURE.md), the [modern terminal gap analysis](docs/MODERN_TERMINAL_GAP_ANALYSIS.md), and the [widget API documentation](https://docs.rs/egui_xterm).

---

## Quickstart

Add `egui_xterm` to your `Cargo.toml` (Rust `1.95+`, Edition `2024`, `egui 0.37`):

```toml
[dependencies]
egui = "0.37"
egui_xterm = "0.3.0"

# Optional cross-platform PTY (desktop portable-pty + wasm32 WebSocket WebPtyBackend)
# and platform clipboard (desktop arboard + wasm32 Web Clipboard API):
# egui_xterm = { version = "0.3.0", features = ["pty", "native-clipboard"] }
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
- **Desktop & Web PTY** — With the `pty` feature enabled:
  - On desktop, open a local shell with `egui_xterm::pty::PtyBackend::spawn_with_repaint(PtyConfig::default(), Box::new(move || ctx.request_repaint()))?`.
  - On `wasm32-unknown-unknown`, connect to a remote PTY gateway (including `ttyd`, `TaggedBinary`, `JsonResize`, or `Raw`) with `egui_xterm::pty::WebPtyBackend::connect_with_repaint(WebPtyConfig::new("wss://host/ws"), Box::new(move || ctx.request_repaint()))?`. Server gateways can reuse `egui_xterm::pty::protocol` and `WebPtySession` across all targets.

---

## Feature Flags

### `egui_xterm` & `egui_xterm_core` Features

| Feature | Crates | Default | WASM32 Safe | Description |
|---|---|---|---|---|
| `search` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | `memchr` literal, `regex`, and `whole_word` scrollback/grid search plus floating `search_bar` overlay (`Ctrl+Shift+F`) |
| `hyperlinks` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | Deduplicated `OSC 8` `HyperlinkPool`, lazy dirty-range `ImplicitLinkStore`, hover URI tooltip/underline, and click activation events |
| `shell-integration` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | `OSC 133` A/B/C/D semantic prompt/input/output zones, `jump_to_prompt`, `command_output_at`, binary-searched `prompt_gutter`, and exit status events |
| `kitty-keyboard` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | Kitty progressive keyboard protocol (`CSI > / < / = / ? u`) per-screen mode stacks, `modifyOtherKeys`, lock-key tracking, and zero-alloc `CSI u` encoding |
| `kitty-graphics` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | Kitty Graphics Protocol (`a=t/T/p/d/f/a/c/q`, `f=24/32/100`, `o=z`, concurrent `m=1`/`m=0`, animations, `U+10EEEE` placeholders, `P/Q/H/V` relative placements, CRC32/Adam7 PNG; `t=f/t/s` gated to `EBADF` on `wasm32`) |
| `sixel` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | Sixel inline graphics (`DCS P1;P2;P3 q`, raster `"Pan;Pad;Ph;Pv`, HLS/RGB `#` registers, `DECSDM` `?80`, `?1070`, `?8452`, `O(1)`-amortized row canvas) |
| `vector-glyphs` | `egui_xterm` | **Yes** | Yes | Pixel-snapped analytical rendering for Box Drawing (`U+2500..=U+257F`), Block Elements (`U+2580..=U+259F`), Braille (`U+2800..=U+28FF`), and Powerline glyphs |
| `serde` | `egui_xterm_core`, `egui_xterm` | No | Yes | `serde::Serialize` / `Deserialize` derives on sizes, colors, cells, `Theme`, `ColorScheme`, `TerminalConfig`, `ScaleMode`, and `TerminalSnapshot` |
| `metrics` | `egui_xterm_core`, `egui_xterm` | No | Yes | Lightweight `TerminalMetrics` counters (`bytes_processed`, `frames_rendered`, `dirty_rows_rendered`, `reflow_count`, `csi_sequences`, `osc_sequences`, `hyperlink_gc_runs`) |
| `pty` | `egui_xterm` | No | **Yes (Desktop + Web)** | Re-exports `egui_xterm_pty` as `egui_xterm::pty`: desktop `portable-pty 0.9` `PtyBackend`, `wasm32` WebSocket `WebPtyBackend`, and cross-platform `protocol` + `WebPtySession` (`TaggedBinary`, `Ttyd`, `JsonResize`, `Raw`) |
| `native-clipboard` | `egui_xterm` | No | **Yes (Desktop + Web)** | Platform clipboard integration outside `egui::Event::Paste` (`Ctrl+Shift+V`, context-menu paste, and `OSC 52` helpers): sync `arboard` on desktop, async browser Web Clipboard API (`navigator.clipboard`) on `wasm32` |

### `egui_xterm_demo` Features

- **`default`**: Enables `search`, `hyperlinks`, `shell-integration`, `kitty-keyboard`, `kitty-graphics`, `sixel`, `vector-glyphs`, and `metrics` (plus `pty` and `native-clipboard` across both desktop and `wasm32`).
- **`serde`**: Opt-in forwarding to `egui_xterm/serde`.
- **`inspection`**: Enables `eframe/inspection` for live widget-tree inspection.

---

## Cross-Platform & `wasm32-unknown-unknown` Builds

### Native Desktop (`Windows` / `macOS` / `Linux`)

```bash
cargo check --workspace --all-features
cargo doc --workspace --no-deps --all-features
```

### `wasm32-unknown-unknown`

Install the `wasm32-unknown-unknown` target on Rust `1.95+` once if needed (`rustup target add wasm32-unknown-unknown`), then verify WASM compilation:

```bash
# Core engine and egui widget/runtime (default features are 100% WASM-safe)
cargo check --target wasm32-unknown-unknown -p egui_xterm_core -p egui_xterm

# Core engine with all WASM-safe features (including serde + metrics)
cargo check --target wasm32-unknown-unknown -p egui_xterm_core --all-features

# egui widget/runtime with all features (including WebSocket PTY & Web Clipboard API)
cargo check --target wasm32-unknown-unknown -p egui_xterm --all-features

# Minimal core/widget build without optional features
cargo check --target wasm32-unknown-unknown -p egui_xterm_core --no-default-features
cargo check --target wasm32-unknown-unknown -p egui_xterm --no-default-features

# Web PTY crate and Demo app compile cleanly against wasm32-unknown-unknown
cargo check --target wasm32-unknown-unknown -p egui_xterm_pty
cargo check --target wasm32-unknown-unknown -p egui_xterm_demo
```

---

## Running the Demo App

Launch the interactive `eframe 0.37` demo application on desktop:

```bash
cargo run -p egui_xterm_demo
```

Or run it in your browser via [Trunk](https://trunkrs.dev/) (`wasm32-unknown-unknown`):

```bash
# Terminal 1: start the built-in local WebSocket PTY gateway (ws://127.0.0.1:7681/ws)
cargo run -p egui_xterm_web_gateway

# Terminal 2: serve the browser app
cd examples/egui_xterm_demo
trunk serve
# Open http://127.0.0.1:8080/ and click "Connect Web PTY" (or ?ws=...&proto=ttyd|tagged|json|raw&autoconnect=1)
```

See [examples/egui_xterm_demo/README.md](examples/egui_xterm_demo/README.md) for gateway, `ttyd`, and Web Clipboard setup instructions.

The demo includes:
- **Local PTY tab** *(desktop)*: Live OS shell via `PtyBackend::spawn_with_repaint`, with `"New PTY"` and `"Restart PTY"` actions.
- **Web PTY tab** *(browser / `wasm32`)*: Live remote shell via `WebPtyBackend::connect_with_repaint`, supporting `TaggedBinary`, `ttyd` (with optional `AuthToken`), `JsonResize`, and `Raw` protocols plus `Connect` / `Reconnect` / `Close` controls.
- **Trait Demo tab** *(all targets)*: Interactive `DemoShellInput` implementing `TerminalInput`, with a collapsible **Trait Showcase** sidebar (`help`, `colors`, `unicode`, `links`, `prompts`, `graphics`, `modern`, `osc52`, `sync`, `size`, `scroll`, `clear`).
- **Split View, Clipboard & Auto-Layout controls**: Side-by-side resizable panes, live switching between `ScaleMode::ReflowGrid` and `ScaleMode::AutoScaleToFit`, font size & zoom controls, Dark/Light themes, cursor shape/blink toggles, toolbar `Paste` + `OSC 52` policy & `Read Clipboard` banner, search overlay (`Find` / `Ctrl+Shift+F`), prompt navigation (`Prev Prompt` / `Next Prompt`), and live status/metrics/I/O-error bar.

---

## Tests & Benchmarks

### Running Tests, Doc-Tests & Lints

```bash
# Formatting & Clippy
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Workspace tests, feature matrix & opaque-box E2E Tiers 1–5
cargo test --workspace
cargo test --workspace --all-features
cargo test -p egui_xterm_core --no-default-features
cargo test -p egui_xterm --no-default-features
cargo test -p egui_xterm_e2e --all-features
cargo test -p egui_xterm_e2e --test tier5_adversarial --all-features

# Runnable doc-tests across feature combinations
cargo test --doc --workspace
cargo test --doc --workspace --all-features
cargo test --doc -p egui_xterm_core --no-default-features
```

### Running Benchmarks

Run the Criterion `--release` benchmark suites sequentially (with `RUSTFLAGS` unset):

```bash
# Core VT500 parser throughput, 10k saturated terminal ingestion, reflow, and input adapters
cargo bench -p egui_xterm_core --all-features --bench vt_throughput

# 200x60 TrueColor/UTF-8/TUI frame update, auto-layout, coalesced mesh render, and epaint::Tessellator
cargo bench -p egui_xterm --all-features --bench frame_render

# Native PTY reader/writer buffer-pool transport throughput
cargo bench -p egui_xterm_pty --bench pty_transport
```

For full architecture details, modern terminal gap analysis, baseline measurements, and before/after Criterion performance analysis, see:
- [Architecture guide](docs/ARCHITECTURE.md)
- [Modern terminal gap analysis](docs/MODERN_TERMINAL_GAP_ANALYSIS.md)
- [Pre-optimization Criterion baseline (M1)](docs/BENCHMARK_BASELINE.md)
- [Criterion benchmark report](docs/BENCHMARKS.md)
- [End-to-end performance report](docs/PERFORMANCE_REPORT.md)

---

## License

Licensed under either the [MIT License](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE), at your option.
