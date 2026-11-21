# `egui_xterm` Release Benchmark Report

> **Profile**: Cargo `bench` (`--release` `[optimized]`, `RUSTFLAGS` unset, executed sequentially on an idle machine)  
> **M1 Pre-Optimization Baseline**: `2026-11-21T01:42:23Z` at commit `387887e4846dbf9c516881f52d690fb407a10693` (see [`BENCHMARK_BASELINE.md`](./BENCHMARK_BASELINE.md))  
> **Post-M5 Final Run**: `[PENDING_FINAL_BENCH]`  
> **Benchmark Suite Scope**: `44` Criterion benchmarks (`22` `vt_throughput` + `15` baseline `frame_render` + `3` `frame_render_200x60_m5` + `4` `pty_transport`)

---

## 1. Host, Toolchain & Run Metadata

| Field | Pre-Optimization Baseline (M1) | Post-M5 Final Run |
|---|---|---|
| **Timestamp (UTC)** | `2026-11-21T01:42:23Z` | `[PENDING_FINAL_BENCH]` |
| **Git Commit** | `387887e4846dbf9c516881f52d690fb407a10693` | `[PENDING_FINAL_BENCH]` |
| **OS** | `macOS 27.2.1` (`BuildVersion: 26B131`, `arm64`) | `macOS 27.2.1` (`BuildVersion: 26B131`, `Darwin 27.2.0 RELEASE_ARM64_T6041 arm64`) |
| **CPU** | `Apple M4 Max` (`16` logical CPUs) | `Apple M4 Max` (`16` physical / `16` logical CPUs) |
| **Memory** | `48 GiB` (`51,539,607,552` bytes) | `48 GiB` (`51,539,607,552` bytes) |
| **Compiler (`rustc`)** | `rustc 1.99.0-nightly (375b1431b 2026-07-10)` | `[PENDING_FINAL_BENCH]` *(host toolchains: `rustc 1.95.0 (59807616e 2026-04-14)` MSRV; default `rustc 1.102.0-nightly (c7d5ba59d 2026-11-20)`)* |
| **Cargo** | `cargo 1.99.0-nightly (59800466c 2026-07-07)` | `[PENDING_FINAL_BENCH]` *(host toolchains: `cargo 1.95.0 (f2d3ce0bd 2026-03-21)` MSRV; default `cargo 1.102.0-nightly (f93c676e3 2026-11-15)`)* |
| **Benchmark Harness** | `criterion 0.8` (`--bench` / `--release`, default `RUSTFLAGS`) | `criterion 0.8.2` (`--bench` / `--release`, `RUSTFLAGS` unset) |
| **Benchmark Count** | `37` (`22` `vt_throughput` + `15` `frame_render`) | `44` (`22` `vt_throughput` + `15` baseline `frame_render` + `3` `frame_render_200x60_m5` + `4` `pty_transport`) |

---

## 2. Benchmark Inventory & Exact Reproduction Commands

Run all three Criterion targets **sequentially** from the workspace root with `RUSTFLAGS` unset on an idle machine so background compilation or CPU contention does not skew microbenchmark latency or outlier counts:

```bash
unset RUSTFLAGS
rustc --version
cargo bench -p egui_xterm_core --all-features --bench vt_throughput
cargo bench -p egui_xterm --all-features --bench frame_render
cargo bench -p egui_xterm_pty --bench pty_transport
```

> **Notes**:
> - Prefix `cargo +1.95.0 bench ...` to pin the workspace MSRV toolchain (`rust-version = "1.95"` in `Cargo.toml`).
> - Append `-- --test` to any command above to execute the Criterion harness smoke-check (`1` iteration per benchmark, no statistical sampling).
> - `--all-features` is required on `egui_xterm_core` and `egui_xterm` so feature-gated benchmarks (`shell-integration`, `hyperlinks`, `vector-glyphs`, `search`, `kitty-graphics`) compile and execute. `egui_xterm_pty` defines no optional Cargo features.

### Target Breakdown (`44` Total Benchmarks)

1. **`crates/egui_xterm_core/Cargo.toml` — `[[bench]] name = "vt_throughput"` (`22` benchmarks)**
   - **Source**: [`crates/egui_xterm_core/benches/vt_throughput.rs`](../crates/egui_xterm_core/benches/vt_throughput.rs) (`#![forbid(unsafe_code)]`, `harness = false`)
   - **Groups & Parameters**:
     - `vt_stream_throughput` (`8` benchmarks; Criterion default `100` samples, `3s` warm-up, `5s` measurement, `Throughput::Bytes`, `BatchSize::LargeInput`): compares zero-alloc pure `Parser::advance` (`CountingHandler` overriding `print_ascii_run`) vs. full `120x40` `Terminal::advance` (`CoreConfig::default()`, `10,000`-line scrollback) across `ascii_plain` (`524,316` B), `utf8_mixed` (`524,308` B), `truecolor_sgr` (`524,367` B), and `osc133_dense` (`2,500` commands / `350,138` B; `#[cfg(feature = "shell-integration")]` on `terminal/`).
     - `terminal_saturated_10k` (`4` benchmarks; `sample_size = 20`, `1s` warm-up, `4s` measurement, `Throughput::Bytes`, `BatchSize::PerIteration`): pre-fills `10,000` scrollback lines (`10,500` input lines or `5,200` `OSC 133` commands), then feeds `4,096` lines (or `2,048` `OSC 133` commands) in `16 KiB` chunks across `ascii_with_colons`, `ascii_no_colons`, `osc133_dense` (`shell-integration`), and `utf8_mixed`.
     - `chunked_16k_advance` (`3` benchmarks; `sample_size = 50`, `2s` warm-up, `4s` measurement, `Throughput::Bytes`, `BatchSize::LargeInput`): streams `512 KiB` (`ascii_plain`, `truecolor_sgr`) or `2,500` `OSC 133` commands (`osc133_dense`, `shell-integration`) in `16 KiB` chunks into a fresh `120x40` `Terminal`.
     - `sync_output_dec2026_200x60` (`1` benchmark; `sample_size = 50`, `2s` warm-up, `4s` measurement, `Throughput::Bytes`, `BatchSize::LargeInput`): ingests `4` full `200x60` TrueColor + combining-mark (`e\u{0301}`) frames wrapped in `DEC ?2026h` / `?2026l` via `advance_with_time` and snapshots `active_snapshot()`.
     - `osc8_and_graphemes` (`1` benchmark; `#[cfg(feature = "hyperlinks")]`, `sample_size = 50`, `2s` warm-up, `4s` measurement, `Throughput::Bytes`, `BatchSize::LargeInput`): streams `512 KiB` of `768` rotating distinct `OSC 8` URIs (exceeding the `256`-entry `HyperlinkPool` GC threshold) combined with combining accents and ZWJ emoji clusters.
     - `reflow_10k_scrollback` (`2` benchmarks; `sample_size = 20`, `Throughput::Elements(10_000)`, `BatchSize::PerIteration`): lossless primary + saturated `10,000`-line scrollback reflow (`narrow_120x40_to_80x40` and `widen_80x40_to_160x40`).
     - `reflow_10k_with_2500_osc133_zones` (`1` benchmark; `#[cfg(feature = "shell-integration")]`, `sample_size = 10`, `1s` warm-up, `5s` measurement, `Throughput::Elements(10_000)`, `BatchSize::PerIteration`): `narrow_120x40_to_80x40` across `10,000` scrollback lines carrying `>= 2,500` `OSC 133` zones (`3,400` commands).
     - `input_adapters` (`2` benchmarks; `sample_size = 100`, `1s` warm-up, `3s` measurement, `Throughput::Bytes(524_288)`, `BatchSize::SmallInput`): pushes `512 KiB` in `16 KiB` chunks and drains via `TerminalInput::poll_read` with a `64 KiB` scratch buffer for `RingBufferInput` and `ChannelInput`.

2. **`crates/egui_xterm/Cargo.toml` — `[[bench]] name = "frame_render"` (`18` benchmarks: `15` baseline + `3` M5)**
   - **Source**: [`crates/egui_xterm/benches/frame_render.rs`](../crates/egui_xterm/benches/frame_render.rs) (`#![forbid(unsafe_code)]`, `harness = false`)
   - **Workload**: Single-frame headless `egui::Context::run_ui` + `TerminalRuntime<RingBufferInput>::show` + (except `show_only_*`) `egui::Context::tessellate` for a `200x60` (`12,000` cells) terminal at `native_pixels_per_point = 1.0`, `ScaleMode::ReflowGrid`, `resize_debounce_secs = 0.0`, `read_budget_bytes_per_frame = usize::MAX`, `Throughput::Elements(12_000)`.
   - **Groups & Parameters**:
     - `frame_render_stages` (`4` baseline benchmarks; `sample_size = 50`, `2s` warm-up, `4s` measurement): isolates `show` vs. `show + tessellate` on `idle` and `full_redraw`.
     - `frame_render_200x60_truecolor` (`8` baseline benchmarks; `sample_size = 50`, `2s` warm-up, `4s` measurement): `idle`, `1_dirty_row`, `scroll_1_line`, `full_redraw`, `cursor_blink_toggle` (`0.5s` steps with focus), `active_selection_1k_lines` (`> 100 KB` stream selection on idle frames), `osc133_2500_zones_idle` (`shell-integration`), and `scrollback_scrub_10k` (`+/- 5` lines/frame across `10,000` scrollback lines).
     - `frame_render_200x60_utf8` (`2` baseline benchmarks; `2s` warm-up, `4s` measurement): `utf8_wide_full_redraw` (`sample_size = 30`) and `utf8_wide_1_dirty_row` (`sample_size = 50`) mixing CJK wide characters, combining accents, and ZWJ emoji.
     - `frame_render_200x60_tui` (`1` baseline benchmark; `#[cfg(feature = "vector-glyphs")]`, `sample_size = 50`, `2s` warm-up, `4s` measurement): `vector_glyphs_dashboard` with box-drawing, block elements, Braille, Powerline glyphs, and `SGR 4:3;58:2` curly underlines.
     - `frame_render_200x60_m5` (`3` new M5 benchmarks; `sample_size = 50`, `2s` warm-up, `4s` measurement): `bold_and_italic_full_redraw` (`SGR 1`/`3`/`1;3`), `search_2500_matches_redraw` (`#[cfg(feature = "search")]`, `>= 2,500` `"HIT"` matches), and `kitty_placeholder_grid` (`#[cfg(feature = "kitty-graphics")]`, `U+10EEEE` virtual placement `200x60` grid).

3. **`crates/egui_xterm_pty/Cargo.toml` — `[[bench]] name = "pty_transport"` (`4` benchmarks)**
   - **Source**: [`crates/egui_xterm_pty/benches/pty_transport.rs`](../crates/egui_xterm_pty/benches/pty_transport.rs) (`#[cfg(not(target_arch = "wasm32"))]`, `harness = false`, default `Criterion` settings: `100` samples, `3s` warm-up, `5s` measurement)
   - **Groups & Parameters**:
     - `pty_transport_read` (`2` benchmarks; `iter_custom`): measures end-to-end `BenchReader` -> `PtyBackend` background reader thread -> `TerminalInput::poll_read` (`64 KiB` scratch buffer) across `512k_16k_chunks` (`32 × 16 KiB` = `524,288` B) and `4m_16k_chunks` (`256 × 16 KiB` = `4,194,304` B).
     - `pty_transport_idle` (`1` benchmark; `Throughput::Elements(10_000)`): `10k_poll_read_calls` measuring empty non-blocking `poll_read` overhead.
     - `pty_transport_write` (`1` benchmark; `Throughput::Bytes(16_384)`, `iter_custom`): `256x64b_burst` timing `256 × 64 B` `PtyBackend::write_bytes` calls while synchronizing background `BenchWriter` flush outside the timed window.

---

## 3. Core Pipeline Results (`vt_throughput` — `22` Benchmarks)

### 3.1 `vt_stream_throughput` (`512 KiB` single-shot streams, `120x40` terminal)

| Benchmark | Payload Scope | M1 Time `[Low, Est, High]` | M1 Throughput | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|
| `vt_stream_throughput/parser_only/ascii_plain` | `524,316` B | `[71.769 µs, 72.237 µs, 72.781 µs]` | `[6.7092, 6.7598, 6.8039] GiB/s` | `1 (1.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `vt_stream_throughput/parser_only/utf8_mixed` | `524,308` B | `[1.5145 ms, 1.5585 ms, 1.5999 ms]` | `[312.53, 320.83, 330.15] MiB/s` | `4 (4.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `vt_stream_throughput/parser_only/truecolor_sgr` | `524,367` B | `[1.5980 ms, 1.6088 ms, 1.6216 ms]` | `[308.39, 310.84, 312.93] MiB/s` | `0 (0.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `vt_stream_throughput/parser_only/osc133_dense` | `350,138` B (`2,500` cmds) | `[354.63 µs, 365.16 µs, 378.39 µs]` | `[882.46, 914.45, 941.61] MiB/s` | `5 (5.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `vt_stream_throughput/terminal/ascii_plain` | `524,316` B (`120x40`) | `[2.2026 ms, 2.2128 ms, 2.2239 ms]` | `[224.85, 225.97, 227.02] MiB/s` | `14 (14.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `vt_stream_throughput/terminal/utf8_mixed` | `524,308` B (`120x40`) | `[4.9204 ms, 4.9337 ms, 4.9472 ms]` | `[101.07, 101.35, 101.62] MiB/s` | `0 (0.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `vt_stream_throughput/terminal/truecolor_sgr` | `524,367` B (`120x40`) | `[2.5811 ms, 2.5869 ms, 2.5926 ms]` | `[192.88, 193.31, 193.74] MiB/s` | `1 (1.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `vt_stream_throughput/terminal/osc133_dense` | `350,138` B (`120x40`, `shell-integration`) | `[2.4134 ms, 2.4175 ms, 2.4218 ms]` | `[137.88, 138.12, 138.36] MiB/s` | `5 (5.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |

### 3.2 `terminal_saturated_10k` (`10,000` pre-filled scrollback lines + `16 KiB` chunked stream)

| Benchmark | Workload Scope | M1 Time `[Low, Est, High]` | M1 Throughput | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|
| `terminal_saturated_10k/ascii_with_colons` | `10,500` prefill + `4,096` lines (`16 KiB` chunks) | `[1.7483 ms, 1.7519 ms, 1.7552 ms]` | `[173.59, 173.92, 174.27] MiB/s` | `2 (10.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `terminal_saturated_10k/ascii_no_colons` | `10,500` prefill + `4,096` lines (`16 KiB` chunks) | `[1.1115 ms, 1.1129 ms, 1.1144 ms]` | `[273.42, 273.79, 274.11] MiB/s` | `1 (5.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `terminal_saturated_10k/osc133_dense` | `5,200` prefill + `2,048` cmds (`shell-integration`) | `[3.5116 ms, 3.5189 ms, 3.5266 ms]` | `[77.513, 77.682, 77.844] MiB/s` | `1 (5.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `terminal_saturated_10k/utf8_mixed` | `10,500` prefill + `4,096` lines (`16 KiB` chunks) | `[3.1244 ms, 3.1418 ms, 3.1597 ms]` | `[86.230, 86.721, 87.204] MiB/s` | `0 (0.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |

### 3.3 `chunked_16k_advance` (Fresh `120x40` terminal, `16 KiB` read-chunk simulation)

| Benchmark | Workload Scope | M1 Time `[Low, Est, High]` | M1 Throughput | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|
| `chunked_16k_advance/ascii_plain` | `524,316` B in `16 KiB` chunks (`33` calls) | `[2.4313 ms, 2.4361 ms, 2.4421 ms]` | `[204.75, 205.26, 205.66] MiB/s` | `3 (6.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `chunked_16k_advance/truecolor_sgr` | `524,367` B in `16 KiB` chunks | `[2.7455 ms, 2.7506 ms, 2.7567 ms]` | `[181.40, 181.81, 182.14] MiB/s` | `4 (8.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `chunked_16k_advance/osc133_dense` | `350,138` B in `16 KiB` chunks (`shell-integration`) | `[2.4540 ms, 2.4590 ms, 2.4643 ms]` | `[135.50, 135.80, 136.07] MiB/s` | `2 (4.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |

### 3.4 `sync_output_dec2026_200x60` & `osc8_and_graphemes`

| Benchmark | Workload Scope | M1 Time `[Low, Est, High]` | M1 Throughput | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|
| `sync_output_dec2026_200x60/truecolor_combining_frames` | `4` × `200x60` DEC `?2026` TrueColor + `e\u{0301}` frames | `[3.7272 ms, 3.7410 ms, 3.7551 ms]` | `[226.20, 227.06, 227.90] MiB/s` | `0 (0.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `osc8_and_graphemes/512k_distinct_links` | `512 KiB`, `768` rotating `OSC 8` URIs (`hyperlinks`) | `[37.839 ms, 37.915 ms, 38.002 ms]` | `[13.157, 13.187, 13.214] MiB/s` | `1 (2.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |

### 3.5 `reflow_10k_scrollback` & `reflow_10k_with_2500_osc133_zones`

| Benchmark | Workload Scope | M1 Time `[Low, Est, High]` | M1 Throughput | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|
| `reflow_10k_scrollback/narrow_120x40_to_80x40` | `10,000` scrollback lines | `[6.3153 ms, 6.3791 ms, 6.4754 ms]` | `[1.5443, 1.5676, 1.5835] Melem/s` | `0 (0.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `reflow_10k_scrollback/widen_80x40_to_160x40` | `10,000` scrollback lines | `[3.3846 ms, 3.4123 ms, 3.4418 ms]` | `[2.9055, 2.9306, 2.9546] Melem/s` | `1 (5.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `reflow_10k_with_2500_osc133_zones/narrow_120x40_to_80x40` | `10,000` lines + `>= 2,500` `OSC 133` zones (`shell-integration`) | `[141.49 ms, 141.61 ms, 141.72 ms]` | `[70.562, 70.616, 70.679] Kelem/s` | `2 (20.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |

### 3.6 `input_adapters` (`512 KiB` in `16 KiB` chunks, `64 KiB` scratch read)

| Benchmark | Workload Scope | M1 Time `[Low, Est, High]` | M1 Throughput | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|
| `input_adapters/ring_buffer` | `512 KiB` (`32 × 16 KiB`) -> `RingBufferInput::poll_read` | `[11.018 µs, 11.034 µs, 11.050 µs]` | `[44.187, 44.251, 44.316] GiB/s` | `12 (12.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `input_adapters/channel` | `512 KiB` (`32 × 16 KiB`) -> `ChannelInput::poll_read` | `[10.663 µs, 10.690 µs, 10.715 µs]` | `[45.570, 45.678, 45.790] GiB/s` | `4 (4.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |

---

## 4. Frame Rendering Results (`frame_render` — `18` Benchmarks: `15` Baseline + `3` M5)

Target frame budget for smooth 60 fps rendering: **`< 16.0000 ms`** (`200x60` = `12,000` cells).

### 4.1 `frame_render_stages` (Show vs. Show + Tessellate stage isolation)

| Benchmark | M1 Time `[Low, Est, High]` | M1 Throughput | M1 60fps Share | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 60fps Share | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|---|
| `frame_render_stages/show_only_idle` | `[4.9637 µs, 4.9716 µs, 4.9797 µs]` | `[2.4098, 2.4137, 2.4176] Gelem/s` | `0.03%` | `2 (4.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_stages/show_and_tessellate_idle` | `[144.68 µs, 145.12 µs, 145.70 µs]` | `[82.360, 82.692, 82.943] Melem/s` | `0.91%` | `2 (4.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_stages/show_only_full_redraw` | `[1.8716 ms, 1.8777 ms, 1.8840 ms]` | `[6.3694, 6.3910, 6.4117] Melem/s` | `11.74%` | `3 (6.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_stages/show_and_tessellate_full_redraw` | `[2.0022 ms, 2.0072 ms, 2.0121 ms]` | `[5.9638, 5.9786, 5.9933] Melem/s` | `12.55%` | `1 (2.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |

### 4.2 `frame_render_200x60_truecolor`

| Benchmark | M1 Time `[Low, Est, High]` | M1 Throughput | M1 60fps Share | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 60fps Share | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|---|
| `frame_render_200x60_truecolor/idle` | `[143.78 µs, 144.26 µs, 144.83 µs]` | `[82.854, 83.185, 83.461] Melem/s` | `0.90%` | `2 (4.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_200x60_truecolor/1_dirty_row` | `[232.50 µs, 232.70 µs, 232.94 µs]` | `[51.514, 51.568, 51.613] Melem/s` | `1.45%` | `4 (8.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_200x60_truecolor/scroll_1_line` | `[256.22 µs, 256.91 µs, 257.50 µs]` | `[46.602, 46.709, 46.835] Melem/s` | `1.61%` | `0 (0.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_200x60_truecolor/full_redraw` | `[1.9874 ms, 1.9922 ms, 1.9974 ms]` | `[6.0077, 6.0236, 6.0381] Melem/s` | `12.45%` | `1 (2.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_200x60_truecolor/cursor_blink_toggle` | `[178.97 µs, 179.13 µs, 179.29 µs]` | `[66.930, 66.992, 67.050] Melem/s` | `1.12%` | `2 (4.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_200x60_truecolor/active_selection_1k_lines` | `[229.84 µs, 230.84 µs, 232.05 µs]` | `[51.714, 51.984, 52.209] Melem/s` | `1.44%` | `3 (6.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_200x60_truecolor/osc133_2500_zones_idle` | `[149.26 µs, 149.86 µs, 150.70 µs]` | `[79.629, 80.074, 80.398] Melem/s` | `0.94%` | `0 (0.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_200x60_truecolor/scrollback_scrub_10k` | `[115.84 µs, 116.00 µs, 116.19 µs]` | `[103.28, 103.45, 103.59] Melem/s` | `0.73%` | `7 (14.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |

### 4.3 `frame_render_200x60_utf8` & `frame_render_200x60_tui`

| Benchmark | M1 Time `[Low, Est, High]` | M1 Throughput | M1 60fps Share | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 60fps Share | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|---|
| `frame_render_200x60_utf8/utf8_wide_full_redraw` | `[921.18 µs, 926.20 µs, 934.17 µs]` | `[12.846, 12.956, 13.027] Melem/s` | `5.79%` | `0 (0.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_200x60_utf8/utf8_wide_1_dirty_row` | `[175.04 µs, 176.10 µs, 177.71 µs]` | `[67.525, 68.144, 68.554] Melem/s` | `1.10%` | `0 (0.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_200x60_tui/vector_glyphs_dashboard` | `[8.4188 ms, 8.8714 ms, 9.3805 ms]` | `[1.2792, 1.3527, 1.4254] Melem/s` | `55.45%` | `0 (0.00%)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |

### 4.4 `frame_render_200x60_m5` (`3` M5 Modern Rendering Benchmarks)

| Benchmark | Feature / Workload Scope | M1 Baseline | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 60fps Share | Post-M5 Outliers | `< 16.0 ms` Budget |
|---|---|---|---|---|---|---|---|
| `frame_render_200x60_m5/bold_and_italic_full_redraw` | `SGR 1` bold + `SGR 3` italic full `200x60` redraw | `N/A (added in M5)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_200x60_m5/search_2500_matches_redraw` | `search` feature, `>= 2,500` active `"HIT"` matches | `N/A (added in M5)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |
| `frame_render_200x60_m5/kitty_placeholder_grid` | `kitty-graphics` feature, `U+10EEEE` virtual placement `200x60` grid | `N/A (added in M5)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` |

---

## 5. PTY Transport Results (`pty_transport` — `4` Benchmarks)

| Benchmark | Group / Scope | M1 Baseline | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 Outliers | Notes |
|---|---|---|---|---|---|---|
| `pty_transport_read/512k_16k_chunks` | `32 × 16 KiB` (`524,288` B) via `PtyBackend` reader -> `poll_read` | `N/A (added in M3)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | Recycled read buffer pool + `Condvar` wakeup |
| `pty_transport_read/4m_16k_chunks` | `256 × 16 KiB` (`4,194,304` B) via `PtyBackend` reader -> `poll_read` | `N/A (added in M3)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | Sustained multi-MiB transport throughput |
| `pty_transport_idle/10k_poll_read_calls` | `10,000` non-blocking empty `poll_read` calls | `N/A (added in M3)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | Decoupled `child.try_wait()` off UI thread |
| `pty_transport_write/256x64b_burst` | `256 × 64 B` (`16,384` B) `write_bytes` burst | `N/A (added in M3)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | Dedicated non-blocking writer thread + buffer pool |

---

## 6. Verbatim Post-M5 Criterion Output

<details>
<summary>vt_throughput (cargo bench -p egui_xterm_core --all-features --bench vt_throughput) — 22 benchmarks</summary>

```text
[PENDING_FINAL_BENCH]
```

</details>

<details>
<summary>frame_render (cargo bench -p egui_xterm --all-features --bench frame_render) — 18 benchmarks</summary>

```text
[PENDING_FINAL_BENCH]
```

</details>

<details>
<summary>pty_transport (cargo bench -p egui_xterm_pty --bench pty_transport) — 4 benchmarks</summary>

```text
[PENDING_FINAL_BENCH]
```

</details>
