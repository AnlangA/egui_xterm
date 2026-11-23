# `AGENTS.md` — `egui_xterm` Workspace Guide

> **Repository**: `https://github.com/AnlangA/egui_xterm` (`v0.3.0`)
> **Toolchain & Ecosystem**: Rust Edition `2024`, MSRV `1.95` (`rust-version = "1.95"`), `egui` / `eframe` / `epaint` `0.37`
> **Targets**: Native desktops (`Windows`, `macOS`, `Linux`) and `wasm32-unknown-unknown`

`egui_xterm` is a clean-room, high-performance, multi-crate modern terminal emulator built for effortless embedding into `egui` applications. Consumers implement only non-blocking `TerminalInput::poll_read` for their byte source and invoke `TerminalRuntime::show(ui)` (or `show_with_config` / `ui.add(runtime.widget())`) inside any `egui::Frame` or panel; container sizing, DPI pixel snapping, resize reflow or font auto-scaling, modern VT500+ protocols, inline graphics, selection/clipboard/IME, and `< 16 ms` coalesced mesh rendering work automatically.

---

## Workspace Map

| Member | Path | Role & WASM32 Policy |
|---|---|---|
| `egui_xterm_core` | `crates/egui_xterm_core` | Published. Pure-Rust, always WASM32-safe VT500/VT520 parser, 20B `Cell`, ring scrollback `Grid` + lossless reflow, split `Parser` + `TerminalState`, zero-alloc `RenderSnapshot`, `TerminalInput` + built-in adapters (`FnInput`, `ChannelInput`, `RingBufferInput`), `ImageStore` (pure-Rust base64/zlib/PNG/Sixel), and feature-gated protocol extensions. |
| `egui_xterm` | `crates/egui_xterm` | Published. `egui 0.37` `TerminalRuntime<I>` & `TerminalWidget`, `TerminalConfig`, `ScaleMode`, pixel-snapped auto-layout, 4-stage coalesced mesh renderer (`bg_mesh`, `text_mesh`, `overlay_mesh`), IME/clipboard (desktop `arboard` + `wasm32` Web Clipboard API)/mouse/keyboard mapping, and overlays (`search_bar`, `scrollbar`, `context_menu`, `prompt_gutter`, `hyperlink`, `progress_bar`). WASM32-safe with all features. |
| `egui_xterm_pty` | `crates/egui_xterm_pty` | Published. Cross-platform PTY crate implementing `TerminalInput` (re-exported as `egui_xterm::pty` under `pty`): desktop `portable-pty 0.9` `PtyBackend` (recycled reader/writer buffer pools, non-blocking background writer, decoupled exit watcher, default shell resolution, Windows ConPTY handshake), `wasm32` browser `WebSocket` `WebPtyBackend`, and cross-platform `protocol` + `WebPtySession` (`TaggedBinary`, `Ttyd`, `JsonResize`, `Raw`) plus compile-safe `wasm_stub` for `PtyBackend`/`PtyConfig`. |
| `egui_xterm_e2e` | `crates/egui_xterm_e2e` | `publish = false`. Opaque-box E2E test harness (`ScriptedInput` + headless `egui::Context::run_ui`) covering Tiers 1–5 and Waves 1–3 via public APIs only. |
| `egui_xterm_demo` | `examples/egui_xterm_demo` | `publish = false`. Interactive `eframe 0.37` demo app (desktop PTY tab, `wasm32` `eframe::WebRunner` + `index.html`/`Trunk.toml` WebSocket PTY tab, cross-platform `DemoShellInput` trait tab, split-pane auto-layout, `ScaleMode`/theme/cursor/search/prompt/clipboard controls). Builds and runs cleanly on desktop and `wasm32-unknown-unknown`. |
| `egui_xterm_web_gateway` | `examples/egui_xterm_web_gateway` | `publish = false`. Clean-room native RFC 6455 WebSocket PTY bridge (`ws://127.0.0.1:7681/ws`, zero extra deps) pairing `PtyBackend` with browser `WebPtyBackend` (`TaggedBinary`, `Ttyd`, `JsonResize`, `Raw`). |

---

## Non-Negotiable Invariants

### 1. Clean-Room Boundary
- Every line of VT parsing, grid/reflow, PNG/zlib/Sixel/Kitty decoding, rendering, and PTY orchestration must be **clean-room original Rust**.
- Never copy code from `vte`, `alacritty_terminal`, `wezterm`, `ghostty`, `foot`, `xterm.js`, or other emulators. Do not delegate core logic to external scripts/binaries, and avoid introducing unnecessary or C/FFI dependencies.

### 2. Safety & Lint Policy (`Cargo.toml`, `clippy.toml`)
- `#![forbid(unsafe_code)]` is enforced across every workspace crate.
- Workspace lints enable `missing_docs = "warn"` and `clippy::all = "warn"`, and **deny** `unwrap_used`, `expect_used`, `todo`, `unimplemented`, `dbg_macro`, `let_underscore_must_use`, and `allow_attributes_without_reason`.
- `clippy.toml` sets `allow-unwrap-in-tests = true` and `allow-expect-in-tests = true`. In non-test code, never use `.unwrap()` or `.expect()` — propagate errors via `?`, handle gracefully, or justify rare lint suppressions with `#[expect(..., reason = "...")]`.

### 3. WASM32 Portability
- `egui_xterm_core` (`--all-features`), `egui_xterm` (`--all-features`, default, and `--no-default-features`), `egui_xterm_pty`, and `egui_xterm_demo` must compile cleanly for `wasm32-unknown-unknown`.
- Never use `std::thread`, `std::process`, sockets, or `std::time::Instant` in `egui_xterm_core` or `egui_xterm`. Derive frame timestamps from `ui.input(|i| i.time)` and pass `now_secs: f64` into `Terminal::advance_with_time` / `tick_animations`.
- Keep platform-specific dependencies cleanly target-gated (`portable-pty` and `arboard` behind `#[cfg(not(target_arch = "wasm32"))]`; `web-sys`, `js-sys`, `wasm-bindgen`, and `wasm-bindgen-futures` behind `#[cfg(target_arch = "wasm32")]`), and keep `pty` and `native-clipboard` opt-in (never in `default`).
- Keep `egui_xterm_pty`'s `wasm_stub` exports in sync with native `PtyBackend`, keep `WebPtyBackend` / `WebPtySession` / `protocol` cleanly tested, and keep Kitty `t=f/t/s` file/shm transport policy-gated so it returns `EBADF` on `wasm32`.

### 4. `TerminalInput` & Auto-Layout Contract
- `TerminalInput` must remain object-safe, require only `poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError>` (all other methods defaulted), and impose **no `Send + Sync` bound** so `Rc<RefCell<_>>`, WebSockets, and JS bridges work directly.
- `TerminalRuntime::show(ui)` / `show_with_config` / `widget()` must read `ui.available_rect_before_wrap()`, snap origin and cell metrics to `ctx.pixels_per_point()`, sync `set_pixel_size` and `set_cell_pixel_size`, support `ScaleMode::ReflowGrid` (lossless primary-screen reflow + debounced `TerminalInput::resize`) and `ScaleMode::AutoScaleToFit { cols, rows }`, and surface non-fatal I/O errors via `last_io_error()` / `take_io_error()` / `TerminalEvent::IoError`.

### 5. Memory Layout & `< 16 ms` (60 fps) Performance Invariants
- Preserve compile-time size assertions (`crates/egui_xterm_core/src/cell.rs`, `color.rs`, `grid/row.rs`): `size_of::<Cell>() == 20`, `size_of::<CellFlags>() == 4`, `size_of::<PackedColor>() == 4`, `size_of::<Option<PackedColor>>() == 4`, and `size_of::<Row>() == 40` (on 64-bit). Never store heap types (`String`, `Vec`, `Box`) inside `Cell`; place sparse per-cell data in `CellExtraTable`.
- Keep `Terminal` split into `parser: Parser` + `state: TerminalState` (no per-`advance` `mem::take(&mut self.parser)`), and keep `Terminal::active_snapshot() -> RenderSnapshot<'_>` zero-allocation.
- Update `Row::trimmed_len_cache` on every `Row` mutation and call `Grid::mark_dirty_abs` on mutations so `ImplicitLinkStore::refresh_dirty` stays incremental.
- Preserve the 4-stage coalesced mesh renderer (`bg_mesh`, `text_mesh`, `overlay_mesh` as `Arc<Mesh>`, `CachedRowSlot` 1-row patching, `AsciiGlyphCache` direct ASCII emission, `&str`-indexed `cluster_cache`, and cursor/blink decoupled from `BgLayoutKey` / `GlobalRenderKey`).
- Keep keyboard/mouse/paste encoders zero-allocation (`*_into` into `outgoing_scratch`) and coalesce `poll_read` / `write_bytes` per frame.

---

## Cargo Feature Matrix & Hygiene

| Feature | Crates | Default | WASM32 Safe | Notes |
|---|---|---|---|---|
| `search`, `hyperlinks`, `shell-integration`, `kitty-keyboard`, `kitty-graphics`, `sixel` | `egui_xterm_core`, `egui_xterm` | Yes | Yes | Forwarded through `egui_xterm`, `egui_xterm_e2e`, and `egui_xterm_demo` |
| `vector-glyphs` | `egui_xterm` | Yes | Yes | Pixel-snapped analytical Box Drawing, Block Elements, Braille, and Powerline glyphs |
| `serde`, `metrics` | `egui_xterm_core`, `egui_xterm` | No | Yes | Opt-in serialization derives and telemetry counters |
| `pty` | `egui_xterm` | No | Yes (Desktop + Web) | Re-exports `egui_xterm_pty` as `egui_xterm::pty`: desktop `portable-pty` `PtyBackend`, `wasm32` WebSocket `WebPtyBackend`, and cross-platform `protocol` + `WebPtySession` |
| `native-clipboard` | `egui_xterm` | No | Yes (Desktop + Web) | Platform clipboard fallback outside `egui::Event::Paste`: sync `arboard` on desktop, async Web Clipboard API (`navigator.clipboard`) on `wasm32` |
| `inspection` | `egui_xterm_demo` | No | — | Enables `eframe/inspection` for live widget inspection |

Features must remain strictly additive. Whenever modifying feature-gated APIs, propagate `#[cfg(feature = "...")]` cleanly across `egui_xterm_core`, `egui_xterm`, `egui_xterm_e2e`, and `examples/egui_xterm_demo`.

---

## Codebase & `egui 0.37` Pitfalls

- **`eframe 0.37` App Trait**: Implement `eframe::App::ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame)`, not legacy `update(&mut self, ctx: &egui::Context, ...)`.
- **`egui 0.37` Layout Idioms**: Use `egui::Frame::NONE` (not `Frame::none()`), and pass `&mut Ui` to `CentralPanel::default().show(ui, ...)`.
- **Headless UI Tests**: Use `ctx.run_ui(raw, |ui| ...)` with explicit `RawInput::time` and `raw.viewports.entry(ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp)`, and **always** call `out.drop_without_applying_deltas()` (or apply texture deltas).
- **Opaque-Box E2E Boundary**: `crates/egui_xterm_e2e` tests must use public APIs and `ScriptedInput` only; place white-box unit tests inside the owning crate.
- **PTY Doc-Tests & Windows ConPTY**: Mark doc-tests that spawn an OS PTY with ```` ```no_run ```` (headless CI may lack a TTY); all other doc-tests must pass under default, `--all-features`, and `--no-default-features`. Account for asynchronous startup/resize handshakes in `egui_xterm_pty` Windows ConPTY tests—avoid brittle sub-10 ms sleeps.

---

## Verification & Quality Gates

Run the relevant subset of checks after changes, and run the full affected gate set before completing work:

```bash
# Formatting & strict workspace Clippy
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Tests & feature matrix
cargo test --workspace
cargo test --workspace --all-features
cargo test -p egui_xterm_core --no-default-features
cargo test -p egui_xterm --no-default-features
cargo test -p egui_xterm_e2e --all-features

# Runnable doc-tests & docs build
cargo test --doc --workspace
cargo test --doc --workspace --all-features
cargo test --doc -p egui_xterm_core --no-default-features
cargo doc --workspace --no-deps --all-features

# WASM32 compilation checks
cargo check --target wasm32-unknown-unknown -p egui_xterm_core -p egui_xterm
cargo check --target wasm32-unknown-unknown -p egui_xterm_core --all-features
cargo check --target wasm32-unknown-unknown -p egui_xterm --all-features
cargo check --target wasm32-unknown-unknown -p egui_xterm_core --no-default-features
cargo check --target wasm32-unknown-unknown -p egui_xterm --no-default-features
cargo check --target wasm32-unknown-unknown -p egui_xterm_pty -p egui_xterm_demo

# Interactive demo app (desktop; add `--features inspection` for live widget-tree inspection)
cargo run -p egui_xterm_demo
# Built-in local WebSocket PTY gateway + browser bundle via Trunk
# cargo run -p egui_xterm_web_gateway
# (in examples/egui_xterm_demo) trunk serve   (or `cargo build --target wasm32-unknown-unknown -p egui_xterm_demo`)
```

When modifying parser, grid, reflow, renderer, or PTY hot paths, run Criterion benchmarks sequentially on an idle machine with `RUSTFLAGS` unset:

```bash
cargo bench -p egui_xterm_core --all-features --bench vt_throughput
cargo bench -p egui_xterm --all-features --bench frame_render
cargo bench -p egui_xterm_pty --bench pty_transport
```

---

## Workflow, Git & Reference Docs

- **Discipline**: Follow **Inspect -> Plan -> Implement -> Verify**—read relevant invariants before editing, keep changes tightly scoped, and verify affected targets before concluding.
- **Git & Packaging**: Use Conventional Commits (`feat(scope):`, `fix(scope):`, `perf(scope):`, `test(scope):`, `docs(scope):`, `chore(scope):`). Never delete or gitignore `AGENTS.md` (`.gitignore` enforces `!AGENTS.md`).
- **Docs Sync**: Keep rustdoc (`missing_docs`) and docs updated when public APIs, features, or architecture change. Consult deeper documentation on demand rather than duplicating it here:
  - [`README.md`](README.md) — Public quickstart, feature summary, and usage examples
  - [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — End-to-end architecture, data flow, and renderer stages
  - [`docs/MODERN_TERMINAL_GAP_ANALYSIS.md`](docs/MODERN_TERMINAL_GAP_ANALYSIS.md) — Comparison vs. Ghostty, Kitty, WezTerm, Alacritty, and Foot
  - [`docs/PERFORMANCE_REPORT.md`](docs/PERFORMANCE_REPORT.md) — Bottleneck analysis (`B1`–`B18`) and optimization details
  - [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md) & [`docs/BENCHMARK_BASELINE.md`](docs/BENCHMARK_BASELINE.md) — Criterion `--release` results and M1 baseline
