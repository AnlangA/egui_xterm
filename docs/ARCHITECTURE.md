# `egui_xterm` Architecture & Design Specification

> **Status**: Milestones M1–M5 Complete; E2E Tiers 1–5 Complete (317 `--all-features` tests); Milestone M6 Verified — Phase 1 E2E, Phase 2 Tier 5 Hardening (`516/516` workspace `--all-features`), and Phase 3 Live `egui-mcp` Demo Verification & `--release` Benchmarks Complete (Clean-Room Original Design)
> **Target Ecosystem**: Rust Edition 2024 (MSRV 1.95), `egui` 0.37 / `eframe` 0.37, Native Desktop (`Windows` / `macOS` / `Linux`) & `wasm32-unknown-unknown`

---

## 1. Executive Summary & Requirements Alignment

`egui_xterm` is a production-grade, multi-crate Rust workspace for embedding a high-performance, modern terminal emulator into `egui` applications with minimal boilerplate.

### Core Requirements (`R1`–`R5`)

| ID | Requirement | Architectural Response |
|---|---|---|
| **R1** | Multi-crate layout, single input trait, `egui` auto-layout/scaling/reflow | Split into `egui_xterm_core` (pure engine), `egui_xterm` (`egui` widget & runtime), `egui_xterm_pty` (desktop PTY), `egui_xterm_e2e` (opaque-box tests), and `examples/egui_xterm_demo`. Consumers implement a single non-blocking `TerminalInput::poll_read` method; calling `TerminalRuntime::show(ui)` inside any `egui::Frame` / panel detects `available_rect_before_wrap()`, recomputes grid dimensions or font scale, triggers lossless reflow, and debounces backend resizes. |
| **R2** | Modern terminal capabilities & feature-gated extensions | Core always provides VT500 ANSI/VT parsing, 16/256/24-bit TrueColor, UTF-8 + wide/grapheme handling, lossless wrap/reflow, ring scrollback, selection/clipboard, mouse reporting, alternate screen, and DEC 2026 sync output. High-level features (`search`, `hyperlinks`, `shell-integration`, `kitty-keyboard`, `vector-glyphs`, `serde`, `metrics`) are additive Cargo features. |
| **R3** | Native & `wasm32-unknown-unknown` portability | `egui_xterm_core` (`#![forbid(unsafe_code)]`) and default `egui_xterm` features are 100% pure-Rust and WASM-safe (using `ui.input(|i| i.time)` rather than OS clocks or threads). Desktop-only OS APIs live behind `pty` (`egui_xterm_pty`) and `native-clipboard` (`arboard`). |
| **R4** | Smooth 60fps (`<16ms`/frame) performance | 8-byte SWAR ASCII `Ground` fast-path + `memchr` string/ignore skipping, 20-byte `Copy` `Cell` with sparse `CellExtra`, row pooling, per-row `generation` + `damage_epoch` dirty tracking, coalesced background `epaint::Mesh`, and `Arc<Galley>` `RowRenderCache`. |
| **R5** | Repository sync & clean-room boundary | Synchronized with `https://github.com/AnlangA/egui_xterm` preserving `AGENTS.md`. |

### Clean-Room Engineering Boundary

Every line of terminal parsing, grid storage, reflow, rendering, and PTY orchestration in this workspace is **clean-room original Rust code**. No source code is copied from existing open-source terminal emulators (such as `vte`, `alacritty_terminal`, `wezterm`, `ghostty`, `foot`, or `xterm.js`), and no core logic is delegated to external scripts or binaries.

---

## 2. Modern Terminal Design Survey (Clean-Room Synthesis)

Rather than adopting any single legacy terminal design, `egui_xterm` synthesizes proven architectural ideas from modern high-performance terminals (Ghostty, WezTerm, Alacritty, Kitty, Foot, Windows Terminal, and xterm.js):

| Architectural Area | Industry Lesson Learned | `egui_xterm` Clean-Room Design |
|---|---|---|
| **Parser Engine** | Byte-at-a-time state transitions bottleneck on bulk text; raw C1 (`0x80..=0x9F`) collides with UTF-8 continuation bytes (`Ü` `C3 9C`, `😀` `F0 9F 98 80`). | 14-state DEC VT500 / Paul Williams machine defaulting to `C1Policy::Utf8Only` (with opt-in `C1Policy::Vt500EightBit`). `Ground` uses an 8-byte zero-`unsafe` SWAR `print_ascii_run` fast-path; `DcsPassthrough`, `DcsIgnore`, and `SosPmApcString` use `memchr::memchr3` skipping. Streaming RFC 3629 UTF-8 decoder emits maximal-subpart `U+FFFD` and supports C0/`ESC` interrupts. |
| **Cell Memory Footprint** | Storing `String`, `Vec`, or hyperlink strings directly in every grid cell bloats cache lines and kills scrollback throughput. | Fixed **20-byte `Copy` `Cell`** (`char`, 4-byte `PackedColor` `fg`/`bg`, 32-bit `CellFlags` packing 3-bit `UnderlineStyle`, and 32-bit `extra_id` index). Combining marks, `SGR 58` underline colors, and `HyperlinkId`s live in a sparse `CellExtra` side-table + deduplicated `HyperlinkPool`. |
| **Scrollback & Allocation Churn** | Shifting `Vec<Row>` on every newline causes O(N) moves; allocating fresh `Vec<Cell>` per line hammers the allocator. | `VecDeque<Row>` ring-buffer primary grid + scrollback combined with an evicted-row free-list pool that recycles `Row` capacities during scrolling. |
| **Lossless Resize Reflow** | Wrapping on screen buffer coordinates alone loses logical line breaks or splits double-width CJK/emoji across margins. | Distinct logical vs. `soft_wrapped` physical rows. Primary-screen resize reflows logical lines losslessly, inserts `WRAP_SPACER` for atomic wide-glyph wrapping at margins, and remaps cursor, selection, viewport, and `OSC 133` anchors. Alternate screen (`?47`/`?1047`/`?1049`) never reflows. |
| **Dirty Tracking & Sync Output** | Re-shaping every visible row on every cursor blink wastes CPU/GPU; partial frames during TUI redraws cause flicker. | Per-row monotonic `generation` + grid `damage_epoch` skip unchanged rows in `egui`. DEC Mode 2026 (`CSI ? 2026 h/l`, `$p`) latches a frozen display snapshot with a 250ms safety watchdog so heavy TUI updates render atomically. |
| **Subparameters & Modern Protocols** | Legacy `;`-only CSI parsers fail on `38:2::R:G:B`, `4:3` curly underlines, and Kitty keyboard `CSI u`. | Inline 64-slot `CsiParams` storing both `;` parameters and `:` subparameters in a zero-allocation flat slice table, plus bounded 4-byte intermediates and 4 MiB OSC payloads. |

---

## 3. Multi-Crate Workspace & Data Flow

```text
┌──────────────────────────────────────────────────────────────────────┐
│                   examples/egui_xterm_demo                           │
│   (eframe 0.37 app: PTY tab on desktop + Trait-driven Echo/Mock tab) │
└───────────────┬──────────────────────────────────┬───────────────────┘
                │                                  │ (desktop cfg)
                ▼                                  ▼
┌───────────────────────────────┐      ┌───────────────────────────────┐
│         egui_xterm            │      │        egui_xterm_pty         │
│  TerminalRuntime<I>, Config,  │◄─────┤  PtyBackend (portable-pty)    │
│  Auto-Layout/Scale, Renderer  │ opt  │  impl TerminalInput           │
└───────────────┬───────────────┘      └───────────────┬───────────────┘
                │ (default-features = false)           │
                ▼                                      ▼
┌──────────────────────────────────────────────────────────────────────┐
│                          egui_xterm_core                             │
│  TerminalInput, PackedColor, Cell, VT500 Parser, Grid/Reflow, Modes, │
│  Selection, Features (search, hyperlinks, shell-integration, kitty)  │
└──────────────────────────────────────────────────────────────────────┘
                ▲
                │
┌───────────────┴───────────────┐
│        egui_xterm_e2e         │
│  Opaque-box E2E Suite (T1–T5) │
└───────────────────────────────┘
```

### End-to-End Frame Data Flow

1. **Poll Input**: `TerminalRuntime::show(ui)` polls `I: TerminalInput` via `poll_read(&mut scratch_buf)` up to `config.read_budget_bytes_per_frame` per frame, keeping frame latency bounded even under multi-megabyte output bursts.
2. **Parse & Mutate State**: Raw bytes flow into `Terminal::advance_with_time(bytes, ui_time_secs)`. The VT500 parser invokes `VtHandler` callbacks to update the active `Grid`, `Cursor`, `Modes`, `Selection`, `HyperlinkPool`, optional semantic zones, and outgoing VT response queue (e.g., `DA1`/`DA2`/`DSR`/`DECRQM`).
3. **Auto-Layout & Reflow**: `layout.rs` inspects `ui.available_rect_before_wrap()`, snaps origin and `(cell_w, cell_h)` to `ctx.pixels_per_point()`, computes target `(cols, rows)` or scaled font size per `ScaleMode`, resizes `Terminal` (triggering primary-screen reflow), and dispatches debounced `TerminalInput::resize(TerminalSize)` calls.
4. **Input & Responses**: Focused `egui::Event` items (`Key`, `Text`, `Ime`, `Copy`, `Paste`, raw pointer mouse/wheel) are mapped to VT/Kitty/SGR-mouse byte sequences, combined with pending VT query replies, and flushed via `input.write_bytes(...)`.
5. **Render**: The 4-stage `TerminalRenderer` updates only rows whose `row.generation()`, selection range, or cursor state changed (or all rows when `GlobalRenderKey` changes or `egui` font-atlas resets/option updates occur) and paints coalesced background/selection meshes, text `Galley`s, and pixel-aligned vector chrome.

---

## 4. `TerminalInput` Trait & Adapter Ergonomics

### Why a Single-Method Non-Blocking Trait?

Immediate-mode GUIs (`egui`) run on a single UI thread per frame and cannot block on `std::io::Read` without freezing rendering, nor can `wasm32-unknown-unknown` safely assume multi-threaded async runtimes or `Send + Sync` JS handles. Therefore:

```rust
pub trait TerminalInput {
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError>;
    fn write_bytes(&mut self, _bytes: &[u8]) -> Result<(), TerminalIoError> { Ok(()) }
    fn resize(&mut self, _size: TerminalSize) -> Result<(), TerminalIoError> { Ok(()) }
    fn is_closed(&self) -> bool { false }
}
```

- **Minimal Implementation Surface**: A read-only stream or mock source needs **only** `poll_read`. Interactive transports override `write_bytes`, `resize`, and `is_closed`.
- **No `Send + Sync` Bound**: Enables wrapping `Rc<RefCell<_>>`, WebSocket/WebSerial callbacks, and single-threaded JS bridges directly.
- **Object-Safe**: Usable as `Box<dyn TerminalInput>` or `&mut dyn TerminalInput` via blanket implementations for `Box<T>` and `&mut T` (`T: TerminalInput + ?Sized`).

### Built-In Adapters (`egui_xterm_core::input`)

- **`FnInput<F>`**: Wraps `FnMut(&mut [u8]) -> Result<usize, TerminalIoError>` for zero-boilerplate inline streams.
- **`ChannelInput`**: Non-blocking `std::sync::mpsc` adapter with chunk coalescing, empty-chunk skipping, optional duplex `tx`, optional resize callback, and guaranteed drain-before-`Closed` semantics.
- **`RingBufferInput`**: In-memory `VecDeque<u8>` ring-copy adapter with `push`/`push_str`, `drain_written`, and optional write/resize hooks—ideal for WebSockets, SSH bindings, in-process shells, and deterministic testing.

---

## 5. `egui 0.37` Auto-Layout, Auto-Scaling & `<16ms` Rendering Pipeline

### Frame Auto-Layout & `ScaleMode`

1. Inside any `egui::Frame`, `CentralPanel`, `SidePanel`, or `Window`, `TerminalRuntime::show(ui)` inspects `ui.available_rect_before_wrap()`, clamping infinite scroll-area axes to sensible fallbacks before calling `ui.allocate_exact_size(..., egui::Sense::click_and_drag())`.
2. Cell dimensions `(cell_w, cell_h)` are queried from `ctx.fonts_mut(|f| (f.glyph_width(&font_id, 'M'), f.row_height(&font_id)))` and snapped to physical pixels via `ctx.pixels_per_point()` to prevent subpixel shimmer.
3. Two scaling policies are provided via `TerminalConfig::scale_mode`:
   - **`ScaleMode::ReflowGrid` (Default)**: Keeps `font_size * zoom_factor` constant, derives `(cols, rows)` from the snapped inner rect, triggers primary-screen reflow immediately, and debounces `TerminalInput::resize` using `ui.input(|i| i.time)`.
   - **`ScaleMode::AutoScaleToFit { cols, rows }`**: Locks logical `(cols, rows)` and dynamically scales the effective font size so the entire grid fits snugly inside the available frame rect.
4. Accessibility & focus: Exposes `egui::Role::Terminal` via `WidgetInfo` and locks keyboard focus with `ui.memory_mut(|m| m.set_focus_lock_filter(id, EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }))`.

### 4-Stage `<16ms` Rendering Pipeline

1. **Stage 1 — Coalesced Background & Selection Mesh**: Consecutive cells sharing identical resolved background, selection, or search-highlight colors are merged horizontally into minimal quads inside a reusable `bg_mesh: Arc<Mesh>`, cached across idle frames via `BgCacheKey`.
2. **Stage 2 — Dirty-Row `RowRenderCache`**: Maintains per-row `CachedRowSlot` (`Arc<egui::Galley>`) entries keyed by `(row.generation(), selection_range, cursor_col)` under `GlobalRenderKey` (grid dimensions, `pixels_per_point`, font size, cell metrics, alt-screen/reverse-video flags, and theme/palette hashes), paired with an `AsciiGlyphCache` per-glyph ASCII fast-path and viewport scroll slot rotation. Only mutated rows are re-shaped; font-atlas resets and text-option changes are detected via `atlas_sentinel_galley`, `fonts.font_atlas_fill_ratio()`, `fonts.font_image_size()`, and `fonts.options()`, automatically invalidating cached galleys and cluster widths.
3. **Stage 3 — Pixel-Aligned `vector-glyphs` & Chrome**: Under the `vector-glyphs` feature, Box Drawing (`U+2500..=U+257F`), Block Elements (`U+2580..=U+259F`), and Braille (`U+2800..=U+28FF`) are rendered analytically on snapped pixel coordinates, alongside 5 underline styles (`Single`, `Double`, `Curly`, `Dotted`, `Dashed`), cursor shapes, and IME preedit underlines.
4. **Stage 4 — Batched Submission**: Submits background mesh, cached row galleys, and vector shapes in deterministic paint order with reused per-frame scratch buffers.

### Input, IME & Clipboard Semantics

- **Copy (`Event::Copy`)**: Copies active selection text; when `copy_on_ctrl_c_when_selected` is active and a selection exists, copies without sending `\x03`, otherwise forwards `ETX` (`\x03`).
- **Paste (`Event::Paste`)**: Wraps pasted text in `\x1b[200~ ... \x1b[201~` when DEC Mode 2004 is enabled.
- **IME (`Event::Ime`)**: Renders preedit text at the cursor position and reports `PlatformOutput::ime` (`ImeOutput { rect, cursor_rect }`).
- **Mouse**: Reports X10, VT200 (`1000`), button-event (`1002`), any-event (`1003`), SGR (`1006`), UTF-8 (`1005`), and alternate-scroll (`1007`) directly from raw pointer/scroll inputs.

---

## 6. Feature Matrix & WASM32 Compatibility Policy

### Feature Matrix

| Feature | Crate(s) | Default | WASM32 Safe | Purpose |
|---|---|---|---|---|
| Core engine | `egui_xterm_core`, `egui_xterm` | Always on | Yes | VT500 parser, `TerminalInput`, 20B `Cell`, 16/256/24-bit color, ring scrollback, reflow, selection, mouse, alt screen, DEC 2026 |
| `search` | `egui_xterm_core`, `egui_xterm` | Yes | Yes | `memchr` literal & `regex` scrollback search + search overlay widget |
| `hyperlinks` | `egui_xterm_core`, `egui_xterm` | Yes | Yes | `OSC 8` deduplicated `HyperlinkPool`, implicit URL detection, hover/click events |
| `shell-integration` | `egui_xterm_core`, `egui_xterm` | Yes | Yes | `OSC 133` A/B/C/D prompt/command zones, jump-to-prompt, output selection |
| `kitty-keyboard` | `egui_xterm_core`, `egui_xterm` | Yes | Yes | `CSI > / < / ? u` per-screen stacks, all 5 progressive enhancement flags |
| `vector-glyphs` | `egui_xterm` | Yes | Yes | Crisp pixel-snapped vector Box Drawing, Block Elements, Braille |
| `serde` | `egui_xterm_core`, `egui_xterm` | No | Yes | `Serialize`/`Deserialize` on sizes, colors, cells, config, themes |
| `metrics` | `egui_xterm_core`, `egui_xterm` | No | Yes | Parser throughput & per-frame timing counters |
| `pty` | `egui_xterm` (`egui_xterm_pty`) | No | **No (Desktop)** | `portable-pty 0.9` `PtyBackend` + cross-platform default shell spawn |
| `native-clipboard` | `egui_xterm` (`arboard`) | No | **No (Desktop)** | OS clipboard fallback outside `egui` event context |

### WASM32 Invariants

1. `egui_xterm_core` enforces `#![forbid(unsafe_code)]` and contains zero `std::thread`, `std::process`, filesystem, socket, or `std::time::Instant` usage.
2. `egui_xterm` uses `ui.input(|i| i.time)` for all monotonic frame timing (blink phase, resize debounce, and DEC Mode 2026 watchdog via `advance_with_time`) and depends on `egui_xterm_core` with `default-features = false`, forwarding each feature explicitly.
3. Desktop-only dependencies (`egui_xterm_pty`, `arboard`) are strictly opt-in and never included in `default` features.

---

## 7. Verification Strategy, Benchmarks & Milestone Plan

### Milestones

| Milestone | Scope | Status |
|---|---|---|
| **M1 (`repo_bootstrap_and_parser`)** | Git init/sync, `wasm32-unknown-unknown`, workspace `Cargo.toml` + stubs, `docs/ARCHITECTURE.md`, `egui_xterm_core` (`input.rs`, `color.rs`, `cell.rs`, `parser/`) + unit tests | Complete |
| **M2 (`core_grid_term_reflow`)** | `grid/` (ring scrollback, row pool, dirty tracker, lossless reflow), `term/` (modes, DEC 2026, selection, mouse, events), `CellExtra` grapheme clustering | Complete |
| **M3 (`core_optional_features`)** | `search`, `hyperlinks`, `shell-integration`, `kitty-keyboard`, `serde`, `metrics` | Complete |
| **M4 (`egui_xterm_integration`)** | `egui_xterm` runtime, auto-layout/scaling, 4-stage `<16ms` renderer, `vector-glyphs`, focus/IME/clipboard, `frame_render` Criterion benchmark | Complete |
| **M5 (`pty_and_demo_app`)** | `egui_xterm_pty` + `examples/egui_xterm_demo` + live `egui-mcp` UI verification | Complete |
| **E2E (`e2e_testing_track`)** | Opaque-box E2E test suite (`crates/egui_xterm_e2e`: Tiers 1–4 `295` + Tier 5 adversarial `22` = `317` `--all-features` tests) | Complete |
| **M6 (`final_milestone`)** | `vt_throughput` & `frame_render` `--release` benchmarks, Tier 1–4 E2E pass, Tier 5 adversarial hardening (`516/516` workspace `--all-features`), `README.md`/docs sync, live `egui-mcp` demo verification, git commit & push to `AnlangA/egui_xterm` | Phase 1 E2E, Phase 2 Tier 5, and Phase 3 Live `egui-mcp` Demo & `--release` Benchmarks Verified |

### Verification Matrix

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features` (`516` tests: `111` `egui_xterm_core`, `66` `egui_xterm`, `13` `egui_xterm_pty`, `9` `egui_xterm_demo`, `317` `egui_xterm_e2e`) and `cargo test -p egui_xterm_e2e --all-features` (`317` tests across Tiers 1–5)
- `cargo check -p egui_xterm_core --no-default-features`
- `cargo check -p egui_xterm --no-default-features`
- `cargo check --workspace --exclude egui_xterm_pty --exclude egui_xterm_e2e --target wasm32-unknown-unknown`
- `cargo check -p egui_xterm_core --no-default-features --target wasm32-unknown-unknown`
- `cargo check -p egui_xterm --no-default-features --target wasm32-unknown-unknown`

### Benchmark Plan

`criterion` benchmarks in `crates/egui_xterm_core/benches/vt_throughput.rs` (M6) and `crates/egui_xterm/benches/frame_render.rs` (M4) measure:
1. Bulk plain ASCII, mixed UTF-8/CJK/emoji, 24-bit TrueColor SGR, and dense `OSC 133` prompt/output VT stream throughput (MiB/s / GiB/s) across both pure `Parser` and full `Terminal`.
2. Large-grid (`10,000`-line scrollback) narrow and widen resize reflow latency.
3. Single-frame `200x60` (`12,000` cells) TrueColor grid update + 4-stage render + `epaint::Tessellator` mesh generation latency in `--release` (target: **`< 16.0 ms`** for 60fps smooth rendering).

## Measured Benchmarks

> **Profile**: Cargo `bench` (`--release` `[optimized]`, Criterion `0.8`, `RUSTFLAGS` unset, executed sequentially)  
> **Full Raw Report**: See [`docs/BENCHMARKS.md`](./BENCHMARKS.md)

### `egui_xterm_core` — `vt_throughput` (`cargo bench -p egui_xterm_core --bench vt_throughput`, recorded `2026-11-20T16:04:18Z`)

| Benchmark | Time Estimate | Throughput | Notes |
|---|---|---|---|
| `vt_stream_throughput/parser_only/ascii_plain` | `69.126 µs` | `7.0640 GiB/s` | `524,316` B plain ASCII logs via SWAR `print_ascii_run` |
| `vt_stream_throughput/parser_only/utf8_mixed` | `1.0482 ms` | `477.05 MiB/s` | `524,308` B ASCII + CJK + emoji + ZWJ + combining accents |
| `vt_stream_throughput/parser_only/truecolor_sgr` | `1.2384 ms` | `403.79 MiB/s` | `524,367` B `;` and `:` 24-bit TrueColor + `4:3` curly underline |
| `vt_stream_throughput/parser_only/osc133_dense` | `335.49 µs` | `995.32 MiB/s` | `350,138` B (`2,500` `OSC 133` A/B/C/D commands, `BEL` & `ST`) |
| `vt_stream_throughput/terminal/ascii_plain` | `3.7853 ms` | `132.10 MiB/s` | Full `120x40` `Terminal::advance` (`524,316` B) + scrollback |
| `vt_stream_throughput/terminal/utf8_mixed` | `6.2204 ms` | `80.383 MiB/s` | Full `120x40` `Terminal::advance` (`524,308` B) + `CellExtra` graphemes |
| `vt_stream_throughput/terminal/truecolor_sgr` | `2.7985 ms` | `178.70 MiB/s` | Full `120x40` `Terminal::advance` (`524,367` B) + 20B `Cell` SGR |
| `vt_stream_throughput/terminal/osc133_dense` | `3.3156 ms` | `100.71 MiB/s` | Full `120x40` `Terminal::advance` (`350,138` B) + `7,500` `ShellZone`s |
| `reflow_10k_scrollback/narrow_120x40_to_80x40` | `12.720 ms` | `786.18 Kelem/s` | Lossless resize reflow of `10,000`-line scrollback (`120x40 -> 80x40`) |
| `reflow_10k_scrollback/widen_80x40_to_160x40` | `6.1975 ms` | `1.6136 Melem/s` | Lossless unwrap reflow of `10,000`-line scrollback (`80x40 -> 160x40`) |

### `egui_xterm` — `frame_render` (`cargo bench -p egui_xterm --bench frame_render`, recorded `2026-11-20T16:02:23Z`)

| Benchmark (`200x60` TrueColor, `12,000` cells) | Time Estimate | Throughput (`Melem/s`) | `< 16.0 ms` Budget |
|---|---|---|---|
| `frame_render_200x60_truecolor/idle` | `1.4623 ms` | `8.2061 Melem/s` | **PASS** (`10.94x` headroom) |
| `frame_render_200x60_truecolor/1_dirty_row` | `1.4645 ms` | `8.1942 Melem/s` | **PASS** (`10.93x` headroom) |
| `frame_render_200x60_truecolor/scroll_1_line` | `1.7502 ms` | `6.8564 Melem/s` | **PASS** (`9.14x` headroom) |
| `frame_render_200x60_truecolor/full_redraw` | `3.3178 ms` | `3.6168 Melem/s` | **PASS** (`4.82x` headroom) |
