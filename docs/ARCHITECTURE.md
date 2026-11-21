# `egui_xterm` Architecture & Design Specification

> **Status**: Milestones M1–M5 Complete — Clean-Room Original Architecture (`#![forbid(unsafe_code)]`)
> **Target Ecosystem**: Rust Edition 2024 (MSRV 1.95), `egui` 0.37 / `eframe` 0.37, Native Desktop (`Windows` / `macOS` / `Linux`) & `wasm32-unknown-unknown`

---

## 1. Executive Summary & Requirements Alignment

`egui_xterm` is a production-grade, multi-crate Rust workspace for embedding a high-performance, modern terminal emulator into `egui` applications with minimal boilerplate.

### Core Requirements (`R1`–`R5`)

| ID | Requirement | Architectural Response |
|---|---|---|
| **R1** | Multi-crate layout, single input trait, `egui` auto-layout/scaling/reflow | Split into `egui_xterm_core` (pure engine), `egui_xterm` (`egui` widget & runtime), `egui_xterm_pty` (desktop PTY), `egui_xterm_e2e` (opaque-box tests), and `examples/egui_xterm_demo`. Consumers implement a single non-blocking `TerminalInput::poll_read` method; calling `TerminalRuntime::show(ui)` inside any `egui::Frame` / panel detects `available_rect_before_wrap()`, recomputes grid dimensions or font scale, triggers lossless reflow, and debounces backend resizes. |
| **R2** | Modern terminal capabilities & feature-gated extensions | Core provides VT500/VT520 ANSI parsing, 16/256/24-bit TrueColor, `OSC 5` special colors, UTF-8 + wide/grapheme handling, rectangular ops (`DECCRA`/`ERA`/`FRA`/`SERA`), `DECSCA`/`DECSED`/`DECSEL`, `DECLRMM`/`DECSLRM`, `XTPUSHSGR`/`XTPOPSGR`, `XTWINOPS`, `XTGETTCAP`/`DECRQSS`/`XTVERSION`, `G0..G3` charsets, lossless wrap/reflow, ring scrollback, selection/clipboard, mouse reporting (`?1016` SGR-pixels), `modifyOtherKeys`, alternate screen, DEC 2026 sync output, `OSC 6`/`9;4`/`22`/`52`/`66`/`99`. High-level features (`search`, `hyperlinks`, `shell-integration`, `kitty-keyboard`, `kitty-graphics`, `sixel`, `vector-glyphs`, `serde`, `metrics`) are additive Cargo features. |
| **R3** | Native & `wasm32-unknown-unknown` portability | `egui_xterm_core` (`#![forbid(unsafe_code)]`) and default `egui_xterm` features are 100% pure-Rust and WASM-safe (using `ui.input(\|i\| i.time)` rather than OS clocks or threads). Desktop-only OS APIs live behind `pty` (`egui_xterm_pty`), `native-clipboard` (`arboard`), and desktop-gated `t=f/t/s` Kitty file/shm policies. |
| **R4** | Smooth 60 fps (`<16ms`/frame) end-to-end performance | Split `Parser` + `TerminalState`, 8-byte SWAR ASCII + batch UTF-8 + `memchr` string/CSI fast-paths, 20-byte `Copy` `Cell` with `ExtraSlot` reuse, `Row::trimmed_len_cache` + row pooling, `O(Z log Z + L)` reflow, zero-alloc `RenderSnapshot<'_>`, coalesced `bg_mesh`/`text_mesh`/`overlay_mesh` with 1-row patching, and `PtyBackend` recycled read/write buffer pools. |
| **R5** | Repository sync & clean-room boundary | Synchronized with `https://github.com/AnlangA/egui_xterm` preserving `AGENTS.md`. |

### Clean-Room Engineering Boundary

Every line of terminal parsing, grid storage, reflow, inline image decoding, rendering, and PTY orchestration in this workspace is **clean-room original Rust code**. No source code is copied from existing open-source terminal emulators (such as `vte`, `alacritty_terminal`, `wezterm`, `ghostty`, `foot`, or `xterm.js`), and no core logic is delegated to external scripts or binaries.

---

## 2. Modern Terminal Design Synthesis

`egui_xterm` synthesizes proven architectural ideas from modern high-performance terminals (Ghostty, Kitty, WezTerm, Alacritty, Foot, Windows Terminal, and xterm.js) into a clean-room, `#![forbid(unsafe_code)]` Rust architecture:

| Architectural Area | Industry Lesson Learned | `egui_xterm` Clean-Room Design |
|---|---|---|
| **Parser Engine & State Split** | Byte-at-a-time transitions and per-`advance` parser swaps bottleneck bulk text; raw C1 (`0x80..=0x9F`) collides with UTF-8 continuations. | `Terminal` holds `parser: Parser` and `state: TerminalState` separately, eliminating `std::mem::take(&mut self.parser)` on every `advance`. `Ground` combines an 8-byte SWAR `print_ascii_run` fast-path with batch `std::str::from_utf8` `print_utf8_run`; `OscString`, `DcsPassthrough`, `ApcString`, and ignore states use `memchr` slice forwarding; `dispatch_osc_slice` splits `OSC` payloads on `;` via stack slice buffers with zero heap copying; `CsiParams` holds 64 `;`/`:` slots inline. |
| **Cell Memory & Extra Reuse** | Storing `String`, `Vec`, or URLs in every cell bloats cache lines and triggers allocator churn. | Fixed **20-byte `Copy` `Cell`** (`codepoint`, 4-byte `PackedColor` `fg`/`bg`, 32-bit `CellFlags` packing 3-bit `UnderlineStyle` + `PROTECTED` + `SIZED_SPACER`, and 32-bit `extra_id`). Sparse `CellExtra` (`combining`, `underline_color`, `hyperlink_id`, `text_sizing`) lives in `CellExtraTable` with `ExtraSlot` capacity retention, bitset GC, and deduplicated `HyperlinkPool`. |
| **Scrollback, Row Caching & Reflow** | Shifting `Vec<Row>`, backward `trimmed_len()` scans, and `O(L × Z)` anchor scans kill 10k-line ingestion and resize latency. | `VecDeque<Row>` ring buffer + `ROW_POOL_CAP` free-list pool, `Row::trimmed_len_cache` (`40B` `Row` on 64-bit), `VecDeque<ShellZone>` with `O(1)` eviction, lazy dirty-range `ImplicitLinkStore` with `"://"` prefilter, and `O(Z log Z + L)` primary-screen reflow with cell-buffer pooling and `ReflowAnchors::extra_points` remapping for selection, `OSC 133` zones, and inline image placements. |
| **Zero-Alloc Snapshot & Sync Output** | Cloning visible rows on every frame or `DEC ?2026` transition wastes bandwidth. | `Terminal::active_snapshot(&self) -> RenderSnapshot<'_>` borrows live visible slices via `VisibleRowsView<'a>` with zero heap allocation. `DEC ?2026` latches into a persistent, capacity-reused `LatchedFrame` with a 250 ms safety watchdog; `palette_epoch` and `damage_epoch` let the renderer skip hashing. |
| **Inline Graphics (`ImageStore`)** | External image/zlib C libraries hinder `wasm32` builds and `#![forbid(unsafe_code)]`; naive Sixel decoders allocate `O(N²)` per band. | Pure-Rust base64, RFC 1950/1951 zlib inflate, IEEE CRC-32 + Adam7 PNG decoder, and `O(1)`-amortized row-based `SixelDecoder` (`rows: Vec<Vec<[u8; 4]>>`) feed a 64 MiB LRU `ImageStore`. Supports Kitty `t=d/f/t/s` (policy-gated), concurrent `m=1`/`m=0` chunking, `a=f/a/c` animations (`tick_animations`), `U=1`/`U+10EEEE` virtual placeholders, `P/Q/H/V` relative placements, `StoredImage::revision: u64`, Sixel `P1`/`P2`/`Pan;Pad;Ph;Pv`, `DECSDM` (`?80`), `?1070`, `?8452`, and dynamic `cell_pixel_size`. |
| **Modern VT / OSC / Input** | Modern CLI apps, editors, and multiplexers rely on rich queries, notifications, progress, and unambiguous input. | Implements `XTGETTCAP`, `DECRQSS`, `XTVERSION`, `DECLRMM`/`DECSLRM`, `DECSCA`/`DECSED`/`DECSEL`, `DECCRA`/`ERA`/`FRA`/`SERA`, `XTPUSHSGR`/`XTPOPSGR`, `XTWINOPS`, `G2`/`G3`, `DECALN`, `DECBI`/`DECFI`, `OSC 5/6/9;4/22/52/66/99`, `DEC ?1016` SGR-pixels, `DEC ?2027`/`?2031`/`DSR ?996 n`, `modifyOtherKeys`, Kitty keyboard, and zero-alloc `*_into` encoders. |

See [`MODERN_TERMINAL_GAP_ANALYSIS.md`](./MODERN_TERMINAL_GAP_ANALYSIS.md) for the full comparative matrix against Ghostty, Kitty, WezTerm, Alacritty, and Foot.

---

## 3. Multi-Crate Workspace & Data Flow

```text
┌──────────────────────────────────────────────────────────────────────┐
│                   examples/egui_xterm_demo                           │
│   (eframe 0.37 app: PTY tab on desktop + Trait-driven DemoShell tab) │
└───────────────┬──────────────────────────────────┬───────────────────┘
                │                                  │ (desktop cfg)
                ▼                                  ▼
┌───────────────────────────────┐      ┌───────────────────────────────┐
│         egui_xterm            │      │        egui_xterm_pty         │
│  TerminalRuntime<I>, Config,  │◄─────┤  PtyBackend (portable-pty)    │
│  Auto-Layout/Scale, Coalesced │ opt  │  Recycled reader/writer pools │
│  Mesh Renderer, Widgets       │      │  + decoupled child waiter     │
└───────────────┬───────────────┘      └───────────────┬───────────────┘
                │ (default-features = false)           │
                ▼                                      ▼
┌──────────────────────────────────────────────────────────────────────┐
│                          egui_xterm_core                             │
│  TerminalInput, 20B Cell, Split Parser + TerminalState, Grid/Reflow, │
│  Zero-Alloc RenderSnapshot, ImageStore, Features (search, links,     │
│  shell-integration, kitty-keyboard, kitty-graphics, sixel, metrics)  │
└──────────────────────────────────────────────────────────────────────┘
                ▲
                │
┌───────────────┴───────────────┐
│        egui_xterm_e2e         │
│  Opaque-box E2E Suite (T1–T5) │
└───────────────────────────────┘
```

### End-to-End Frame Data Flow

1. **Native PTY Transport (`egui_xterm_pty`, desktop)**: `PtyBackend`'s background reader thread reuses `Vec<u8>` buffers via a bounded return channel (`RECYCLE_POOL_CAPACITY`), `write_bytes` dispatches through a dedicated background writer thread with recycled buffers (`WRITER_RECYCLE_CAPACITY`), and `try_wait()` is decoupled from per-frame `poll_read()`.
2. **Coalesced Input Poll & Parse**: `TerminalRuntime::show_with_config` polls `I: TerminalInput` up to `config.read_budget_bytes_per_frame` per frame, coalesces chunks into a single `Terminal::advance_with_time(buf, now_secs)` pass, ticks Kitty animations (`tick_animations(now_secs)`), checks the `DEC ?2026` watchdog, and schedules animation or idle repaints. Non-fatal backend errors are recorded in `last_io_error()` / `take_io_error()`.
3. **Auto-Layout & Reflow**: `layout.rs` caches `measure_cell`, reads `ui.available_rect_before_wrap()`, snaps origin and `(cell_w, cell_h)` to `ctx.pixels_per_point()`, syncs `terminal.set_pixel_size` and `terminal.set_cell_pixel_size`, computes `(cols, rows)` or scaled font size per `ScaleMode`, triggers primary-screen reflow, and dispatches debounced `TerminalInput::resize(TerminalSize)`.
4. **Coalesced Input & Event Encoding**: Focused `egui::Event` items (`Key`, `Text`, `Ime`, `Copy`, `Paste`, raw pointer/wheel) encode Kitty keyboard (with `CapsLock`/`NumLock` and `Key::Clear` -> `KpBegin`), `modifyOtherKeys`, legacy keys, paste, `?1016` SGR-pixels, and `DEC ?1004` focus transitions directly into persistent `outgoing_scratch: Vec<u8>` buffers without per-event heap allocations, flushing alongside VT query replies via `input.write_bytes(...)`.
5. **Coalesced Mesh Rendering**: `TerminalRenderer` borrows `Terminal::active_snapshot() -> RenderSnapshot<'_>` with zero allocations and paints coalesced `bg_mesh`, `text_mesh`, `overlay_mesh`, `PathShape` curly underlines, cursor shapes, inline image textures, visual bell, progress bar, scrollbar, and search bar.

---

## 4. `TerminalInput` Trait & Adapter Ergonomics

Immediate-mode GUIs (`egui`) execute on a single UI thread per frame and cannot block on `std::io::Read`, nor can `wasm32-unknown-unknown` assume multi-threaded async runtimes or `Send + Sync` JS handles:

```rust
pub trait TerminalInput {
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError>;
    fn write_bytes(&mut self, _bytes: &[u8]) -> Result<(), TerminalIoError> { Ok(()) }
    fn resize(&mut self, _size: TerminalSize) -> Result<(), TerminalIoError> { Ok(()) }
    fn is_closed(&self) -> bool { false }
}
```

- **Minimal Surface**: Read-only streams implement only `poll_read`.
- **No `Send + Sync` Bound**: Supports `Rc<RefCell<_>>`, WebSocket/WebSerial callbacks, and single-threaded JS bridges directly.
- **Object-Safe**: Usable via `Box<dyn TerminalInput>` and `&mut dyn TerminalInput`.
- **Built-In Adapters (`egui_xterm_core::input`)**: `FnInput<F>`, `ChannelInput` (duplex `mpsc` + chunk coalescing), and `RingBufferInput` (`VecDeque<u8>` FIFO + write/resize hooks).

---

## 5. `egui 0.37` Auto-Layout, Auto-Scaling & `<16ms` Coalesced Mesh Pipeline

### Frame Auto-Layout & `ScaleMode`

1. Inside any `egui::Frame`, `CentralPanel`, `SidePanel`, or `Window`, `TerminalRuntime::show(ui)` reads `ui.available_rect_before_wrap()`, clamps infinite axes to fallback dimensions, and calls `ui.allocate_space(...)` + `ui.interact(...)`.
2. `measure_cell` measures `glyph_width('M')` and `row_height()` snapped to `pixels_per_point()` (cached across matching `(font_size, ppp)` frames).
3. `ScaleMode::ReflowGrid` (default) keeps `font_size * zoom_factor` constant and reflows the primary screen; `ScaleMode::AutoScaleToFit { cols, rows }` locks `(cols, rows)` and binary-searches the optimal font size (cached in `AutoScaleCache`).
4. Accessibility & focus: Exposes `egui::Role::Terminal` via zero-alloc `terminal.title()` borrow in `WidgetInfo::labeled` and locks Tab/Arrow/Escape keys via `TERMINAL_FOCUS_FILTER`.

### 4-Stage `<16ms` Coalesced Mesh Rendering Pipeline

1. **Stage 1 — Incremental `bg_mesh: Arc<Mesh>` (`CachedRowSlot`)**:
   - `GlobalRenderKey` and `BgLayoutKey` exclude `damage_epoch` and `block_cursor`, preventing cursor blink toggles or 1-row edits from invalidating the entire `12,000`-cell background.
   - Each `CachedRowSlot` caches horizontal `BgRun` spans via `bg_valid`, `bg_row_gen`, `bg_sel_range`, and `bg_search_sig` (computed from `(row.generation(), bg_sel, row_search_sig)`), with slot rotation on scroll, in-place 1-row `bg_mesh` vertex patching (`write_colored_rect_vertices`), and `O(log M)` `partition_point` search highlight slicing.
2. **Stage 2 — Coalesced `text_mesh: Arc<Mesh>` (`CachedRowSlot`)**:
   - Replaces per-row `Shape::Text` (`Arc<Galley>`) tessellation overhead with a single coalesced `text_mesh: Arc<Mesh>` (`TextureId::default()`, normalized `[0..1]` UVs). Each `CachedRowSlot` retains row-local texel-UV vertices so dynamic `font_image_size()` atlas growth re-normalizes without re-shaping, and 1-row updates patch in place.
   - **ASCII Fast Path (`build_ascii_row_mesh`)**: Writes quads directly from `AsciiGlyphCache` into `slot.text_vertices`/`text_indices` with zero `LayoutJob`, `LayoutSection`, `FontId::clone`, or `Galley` allocations. `is_vec`, hidden, and `U+10EEEE` cells are skipped inline without disqualifying ASCII rows.
   - **Slow Path & Weights**: `cluster_cache` (`HashMap<String, [Option<CachedCluster>; 4]>`, queried via `&str` `scratch_cluster`) provides zero-alloc lookup and `ClusterQuads` emission for repeated non-ASCII grapheme clusters; cache misses, `OSC 66` scaled runs, and contextual runs (`ContextualRunScratch` / `flush_contextual_run`) construct local `LayoutJob` values passed to `fonts.layout_job(...)` (while plain ASCII cells in mixed rows still hit `AsciiGlyphCache`). Bold weight rendering selects `"monospace_bold"` `FontFamily`, variable `"wght"`, or `FauxBold` offset stamping; `OSC 5` `special_colors` resolve default foreground overrides; SGR blink cycles every 500 ms only when blinking cells are visible.
3. **Stage 3 — Cached `overlay_mesh: Arc<Mesh>` & Inline Images**:
   - Per-slot `overlay_vertices` / `overlay_indices` cache straight/double/dotted/dashed underlines, strikethrough, overline, and `vector-glyphs` (`Box Drawing`, `Block Elements`, `Braille`, `Powerline`); `SGR 4:3` curly underlines emit cached per-dirty-row `PathShape`s; `prompt_gutter` queries `ShellZonesView::in_line_range` in `O(log Z)`.
   - `prepare_inline_images` caches `egui::TextureHandle`s by `(image_id, StoredImage::revision)` (re-uploading via `TextureHandle::set` on animation frame ticks or same-size replacements), supports `sub_x`/`sub_y` offsets and `z_index` layering (`< 0` behind text, `>= 0` above text), and coalesces horizontal `U+10EEEE` Kitty virtual placeholder runs into clipped tile UV quads.
4. **Stage 4 — Deterministic Batched Submission & Overlays**:
   - Submits `bg_mesh`, behind-text images, decoupled `Shape::rect_filled` block cursor, `text_mesh`, `overlay_mesh` + curly `PathShape`s, above-text images, underline/bar/hollow cursor, configurable visual bell flash (`visual_bell_color` / `visual_bell_opacity` / `visual_bell_duration_ms`), IME preedit (highlighting `active_range_chars`), `OSC 9;4` top-edge progress bar (`widgets/progress_bar.rs`), scrollbar, and floating search bar.

### Input, IME & Clipboard Semantics

- **Keyboard**: Precedence is Kitty Keyboard (`CSI >/</=/ ? u`, with tracked `CapsLock`/`NumLock` and `Key::Clear` -> `KpBegin`) > `modifyOtherKeys` (`CSI > 4 ; Pv m` modes 1 & 2) > legacy VT sequences, encoded with zero heap allocations into `outgoing_scratch`.
- **Copy / Paste & `OSC 52`**: Supports `CopyBehavior`, bracketed paste (`?2004`) with `ESC[201~` sanitization, `osc52_write_enabled`, and `Osc52ReadPolicy::{Deny, Allow, Prompt}` (`TerminalEvent::ClipboardReadRequested` / `ClipboardReadPrompt`).
- **IME & Mouse**: Reports `IMEOutput { rect, cursor_rect }`; maps `OSC 22` cursor names with zero allocations (yielded to hyperlink `PointingHand` when activatable); and encodes X10/`1000`/`1002`/`1003`/`1005`/`1006`/`1007` plus `DEC ?1016` SGR-pixels directly into `outgoing_scratch`.

---

## 6. Feature Matrix & WASM32 Compatibility Policy

### Feature Matrix

| Feature | Crate(s) | Default | WASM32 Safe | Purpose |
|---|---|---|---|---|
| Core engine | `egui_xterm_core`, `egui_xterm` | Always on | Yes | Split `Parser`/`TerminalState`, `TerminalInput`, 20B `Cell`, 16/256/24-bit color, `OSC 5/6/9;4/22/52/66/99`, ring scrollback, `O(Z log Z + L)` reflow, selection, mouse (`?1016`), alt screen, DEC 2026, rect/protection/DCS protocols |
| `search` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | `memchr` literal, `regex`, and `whole_word` scrollback/incremental search + floating `search_bar` |
| `hyperlinks` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | `OSC 8` `HyperlinkPool`, lazy `ImplicitLinkStore`, hover URI tooltip/underline, click activation |
| `shell-integration` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | `OSC 133` A/B/C/D semantic zones, `ShellZonesView`, `jump_to_prompt`, binary-searched `prompt_gutter` |
| `kitty-keyboard` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | `CSI >/</=/ ? u` per-screen stacks, all 5 progressive enhancement flags, `modifyOtherKeys` |
| `kitty-graphics` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | Kitty Graphics (`t=d/f/t/s`, animations, `U+10EEEE`, `P/Q/H/V`, pure-Rust CRC32/Adam7 PNG; `t=f/t/s` disabled with `EBADF` on `wasm32`) |
| `sixel` | `egui_xterm_core`, `egui_xterm` | **Yes** | Yes | Sixel inline graphics (`P1/P2/Pan;Pad;Ph;Pv`, `DECSDM` `?80`, `?1070`, `?8452`, row canvas) |
| `vector-glyphs` | `egui_xterm` | **Yes** | Yes | Crisp pixel-snapped vector Box Drawing, Block Elements, Braille, and Powerline glyphs |
| `serde` | `egui_xterm_core`, `egui_xterm` | No | Yes | `Serialize`/`Deserialize` on sizes, colors, cells, config, themes, and `TerminalSnapshot` |
| `metrics` | `egui_xterm_core`, `egui_xterm` | No | Yes | Parser throughput & per-frame timing counters (`TerminalMetrics`) |
| `pty` | `egui_xterm` (`egui_xterm_pty`) | No | **No (Desktop)** | `portable-pty 0.9` `PtyBackend` with recycled buffer pools & background writer |
| `native-clipboard` | `egui_xterm` (`arboard`) | No | **No (Desktop)** | OS clipboard fallback outside `egui` event context |

### WASM32 & Safety Invariants

1. All workspace crates enforce `#![forbid(unsafe_code)]`; `egui_xterm_core` contains zero `std::thread`, `std::process`, socket, or `std::time::Instant` usage, and `t=f/t/s` filesystem/shm transmission returns `EBADF` on `wasm32`.
2. `egui_xterm` uses `ui.input(|i| i.time)` for all monotonic frame timing (cursor/SGR blink, visual bell decay, animation playback, resize debounce, and DEC Mode 2026 watchdog).
3. Desktop-only dependencies (`egui_xterm_pty`, `arboard`) are strictly opt-in and excluded from `default` features.

---

## 7. Verification Strategy & Benchmark References

### Milestones

| Milestone | Scope | Status |
|---|---|---|
| **M1** | Baseline 37-benchmark Criterion suite (`Apple M4 Max`), workspace `#![forbid(unsafe_code)]` & clippy denies, `wasm32` verification | Complete |
| **M2** | Core VT/CSI/OSC/DCS protocol completeness, `search`/`selection`/`RenderSnapshot` APIs, split `Parser`+`TerminalState`, core ingestion/reflow perf (`B1`, `B4`, `B5`, `B7`, `B12`, `B14`–`B18`) | Complete |
| **M3** | `PtyBackend` recycled read/write buffer pools, background writer thread, decoupled `try_wait()`, `pty_transport` benchmarks (`B11`) | Complete |
| **M4** | Kitty Graphics completeness (`t=f/t/s`, chunking, animations, `U+10EEEE`, `P/Q/H/V`, CRC32/Adam7 PNG, `revision`, `abs_line` reflow), Sixel fixes & `O(1)` row canvas, dynamic `cell_pixel_size` | Complete |
| **M5** | `egui` coalesced mesh pipeline (`bg_mesh`/`text_mesh`/`overlay_mesh`, `B2`, `B3`, `B6`, `B8`–`B11`, `B13`), bold/visual bell/`OSC 9;4`/`OSC 22`/`OSC 52`/`modifyOtherKeys`, demo, runnable doc-tests, gap analysis & Criterion perf report | Complete |
| **E2E** | Opaque-box E2E test suite (`crates/egui_xterm_e2e`, Tiers 1–5) | Complete |

### Verification Commands

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace` & `cargo test --workspace --all-features`
- `cargo test --doc --workspace`, `cargo test --doc --workspace --all-features`, `cargo test --doc -p egui_xterm_core --no-default-features`
- `cargo check -p egui_xterm_core --no-default-features` & `cargo check -p egui_xterm --no-default-features`
- `cargo +1.95.0 check --target wasm32-unknown-unknown -p egui_xterm_core -p egui_xterm` (plus `--no-default-features`, `--all-features` on core, `egui_xterm_pty`, and `egui_xterm_demo`)

### Benchmark & Analysis Reports

- **Comparative Feature Gap Analysis**: [`MODERN_TERMINAL_GAP_ANALYSIS.md`](./MODERN_TERMINAL_GAP_ANALYSIS.md)
- **Pre-Optimization Baseline (M1, `Apple M4 Max`)**: [`BENCHMARK_BASELINE.md`](./BENCHMARK_BASELINE.md)
- **Criterion `--release` Measurements (`vt_throughput`, `frame_render`, `pty_transport`)**: [`BENCHMARKS.md`](./BENCHMARKS.md)
- **End-to-End Bottleneck & Before/After Performance Report (`B1`–`B18`)**: [`PERFORMANCE_REPORT.md`](./PERFORMANCE_REPORT.md)
