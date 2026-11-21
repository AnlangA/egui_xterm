# `egui_xterm` Release Benchmark Report

> **Profile**: Cargo `bench` (`--release` `[optimized]`, `RUSTFLAGS` unset, executed sequentially on an idle machine)  
> **M1 Pre-Optimization Baseline**: `2026-11-21T01:42:23Z` at commit `387887e4846dbf9c516881f52d690fb407a10693` (see [`BENCHMARK_BASELINE.md`](./BENCHMARK_BASELINE.md))  
> **Post-M5 Final Run**: `2026-11-21T06:25:07Z` (`06:25:07Z`–`06:31:38Z`) at commit `0e256a793116bd1238bf084d954f02566e0a3fc8` (`0e256a7`)  
> **Benchmark Suite Scope**: `44` Criterion benchmarks (`22` `vt_throughput` + `15` baseline `frame_render` + `3` `frame_render_200x60_m5` + `4` `pty_transport`)

---

## 1. Host, Toolchain & Run Metadata

| Field | Pre-Optimization Baseline (M1) | Post-M5 Final Run |
|---|---|---|
| **Timestamp (UTC)** | `2026-11-21T01:42:23Z` | `2026-11-21T06:25:07Z` (finished `06:31:38Z`) |
| **Git Commit** | `387887e4846dbf9c516881f52d690fb407a10693` | `0e256a793116bd1238bf084d954f02566e0a3fc8` (`0e256a7`) |
| **OS** | `macOS 27.2.1` (`BuildVersion: 26B131`, `arm64`) | `macOS 27.2.1` (`BuildVersion: 26B131`, `Darwin 27.2.0 RELEASE_ARM64_T6041 arm64`) |
| **CPU** | `Apple M4 Max` (`16` logical CPUs) | `Apple M4 Max` (`16` physical / `16` logical CPUs) |
| **Memory** | `48 GiB` (`51,539,607,552` bytes) | `48 GiB` (`51,539,607,552` bytes) |
| **Compiler (`rustc`)** | `rustc 1.99.0-nightly (375b1431b 2026-07-10)` | `rustc 1.102.0-nightly (c7d5ba59d 2026-11-20)` *(host also has `rustc 1.95.0 (59807616e 2026-04-14)` MSRV)* |
| **Cargo** | `cargo 1.99.0-nightly (59800466c 2026-07-07)` | `cargo 1.102.0-nightly (f93c676e3 2026-11-15)` *(host also has `cargo 1.95.0 (f2d3ce0bd 2026-03-21)` MSRV)* |
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
| `vt_stream_throughput/parser_only/ascii_plain` | `524,316` B | `[71.769 µs, 72.237 µs, 72.781 µs]` | `[6.7092, 6.7598, 6.8039] GiB/s` | `1 (1.00%)` | `[77.984 µs, 78.312 µs, 78.675 µs]` | `[6.2066, 6.2354, 6.2616] GiB/s` | `3 (3.00%)` | `0.92×` (`+8.4%` time) |
| `vt_stream_throughput/parser_only/utf8_mixed` | `524,308` B | `[1.5145 ms, 1.5585 ms, 1.5999 ms]` | `[312.53, 320.83, 330.15] MiB/s` | `4 (4.00%)` | `[415.42 µs, 416.37 µs, 417.34 µs]` | `[1.1700, 1.1728, 1.1754] GiB/s` | `0 (0.00%)` | `3.74×` (`-73.3%`) |
| `vt_stream_throughput/parser_only/truecolor_sgr` | `524,367` B | `[1.5980 ms, 1.6088 ms, 1.6216 ms]` | `[308.39, 310.84, 312.93] MiB/s` | `0 (0.00%)` | `[682.98 µs, 684.72 µs, 686.38 µs]` | `[728.56, 730.34, 732.20] MiB/s` | `4 (4.00%)` | `2.35×` (`-57.4%`) |
| `vt_stream_throughput/parser_only/osc133_dense` | `350,138` B (`2,500` cmds) | `[354.63 µs, 365.16 µs, 378.39 µs]` | `[882.46, 914.45, 941.61] MiB/s` | `5 (5.00%)` | `[223.69 µs, 224.39 µs, 225.07 µs]` | `[1.4488, 1.4533, 1.4578] GiB/s` | `4 (4.00%)` | `1.63×` (`-38.6%`) |
| `vt_stream_throughput/terminal/ascii_plain` | `524,316` B (`120x40`) | `[2.2026 ms, 2.2128 ms, 2.2239 ms]` | `[224.85, 225.97, 227.02] MiB/s` | `14 (14.00%)` | `[1.0769 ms, 1.0790 ms, 1.0811 ms]` | `[462.50, 463.41, 464.33] MiB/s` | `2 (2.00%)` | `2.05×` (`-51.2%`) |
| `vt_stream_throughput/terminal/utf8_mixed` | `524,308` B (`120x40`) | `[4.9204 ms, 4.9337 ms, 4.9472 ms]` | `[101.07, 101.35, 101.62] MiB/s` | `0 (0.00%)` | `[5.8262 ms, 5.8357 ms, 5.8461 ms]` | `[85.531, 85.683, 85.823] MiB/s` | `2 (2.00%)` | `0.85×` (`+18.3%` time) |
| `vt_stream_throughput/terminal/truecolor_sgr` | `524,367` B (`120x40`) | `[2.5811 ms, 2.5869 ms, 2.5926 ms]` | `[192.88, 193.31, 193.74] MiB/s` | `1 (1.00%)` | `[1.3657 ms, 1.3677 ms, 1.3699 ms]` | `[365.06, 365.64, 366.16] MiB/s` | `17 (17.00%)` | `1.89×` (`-47.1%`) |
| `vt_stream_throughput/terminal/osc133_dense` | `350,138` B (`120x40`, `shell-integration`) | `[2.4134 ms, 2.4175 ms, 2.4218 ms]` | `[137.88, 138.12, 138.36] MiB/s` | `5 (5.00%)` | `[1.1273 ms, 1.1289 ms, 1.1305 ms]` | `[295.36, 295.80, 296.21] MiB/s` | `11 (11.00%)` | `2.14×` (`-53.3%`) |

### 3.2 `terminal_saturated_10k` (`10,000` pre-filled scrollback lines + `16 KiB` chunked stream)

| Benchmark | Workload Scope | M1 Time `[Low, Est, High]` | M1 Throughput | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|
| `terminal_saturated_10k/ascii_with_colons` | `10,500` prefill + `4,096` lines (`16 KiB` chunks) | `[1.7483 ms, 1.7519 ms, 1.7552 ms]` | `[173.59, 173.92, 174.27] MiB/s` | `2 (10.00%)` | `[650.09 µs, 651.51 µs, 652.58 µs]` | `[466.89, 467.66, 468.69] MiB/s` | `3 (15.00%)` | `2.69×` (`-62.8%`) |
| `terminal_saturated_10k/ascii_no_colons` | `10,500` prefill + `4,096` lines (`16 KiB` chunks) | `[1.1115 ms, 1.1129 ms, 1.1144 ms]` | `[273.42, 273.79, 274.11] MiB/s` | `1 (5.00%)` | `[648.27 µs, 651.24 µs, 655.73 µs]` | `[464.65, 467.86, 470.00] MiB/s` | `1 (5.00%)` | `1.71×` (`-41.5%`) |
| `terminal_saturated_10k/osc133_dense` | `5,200` prefill + `2,048` cmds (`shell-integration`) | `[3.5116 ms, 3.5189 ms, 3.5266 ms]` | `[77.513, 77.682, 77.844] MiB/s` | `1 (5.00%)` | `[917.99 µs, 923.10 µs, 930.91 µs]` | `[293.64, 296.13, 297.78] MiB/s` | `1 (5.00%)` | `3.81×` (`-73.8%`) |
| `terminal_saturated_10k/utf8_mixed` | `10,500` prefill + `4,096` lines (`16 KiB` chunks) | `[3.1244 ms, 3.1418 ms, 3.1597 ms]` | `[86.230, 86.721, 87.204] MiB/s` | `0 (0.00%)` | `[3.1805 ms, 3.1948 ms, 3.2087 ms]` | `[84.913, 85.284, 85.667] MiB/s` | `2 (10.00%)` | `0.98×` (`+1.7%` time) |

### 3.3 `chunked_16k_advance` (Fresh `120x40` terminal, `16 KiB` read-chunk simulation)

| Benchmark | Workload Scope | M1 Time `[Low, Est, High]` | M1 Throughput | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|
| `chunked_16k_advance/ascii_plain` | `524,316` B in `16 KiB` chunks (`33` calls) | `[2.4313 ms, 2.4361 ms, 2.4421 ms]` | `[204.75, 205.26, 205.66] MiB/s` | `3 (6.00%)` | `[1.1116 ms, 1.1132 ms, 1.1149 ms]` | `[448.51, 449.17, 449.84] MiB/s` | `2 (4.00%)` | `2.19×` (`-54.3%`) |
| `chunked_16k_advance/truecolor_sgr` | `524,367` B in `16 KiB` chunks | `[2.7455 ms, 2.7506 ms, 2.7567 ms]` | `[181.40, 181.81, 182.14] MiB/s` | `4 (8.00%)` | `[1.3882 ms, 1.3908 ms, 1.3936 ms]` | `[358.84, 359.56, 360.24] MiB/s` | `1 (2.00%)` | `1.98×` (`-49.4%`) |
| `chunked_16k_advance/osc133_dense` | `350,138` B in `16 KiB` chunks (`shell-integration`) | `[2.4540 ms, 2.4590 ms, 2.4643 ms]` | `[135.50, 135.80, 136.07] MiB/s` | `2 (4.00%)` | `[1.1584 ms, 1.1631 ms, 1.1686 ms]` | `[285.75, 287.09, 288.25] MiB/s` | `0 (0.00%)` | `2.11×` (`-52.7%`) |

### 3.4 `sync_output_dec2026_200x60` & `osc8_and_graphemes`

| Benchmark | Workload Scope | M1 Time `[Low, Est, High]` | M1 Throughput | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|
| `sync_output_dec2026_200x60/truecolor_combining_frames` | `4` × `200x60` DEC `?2026` TrueColor + `e\u{0301}` frames | `[3.7272 ms, 3.7410 ms, 3.7551 ms]` | `[226.20, 227.06, 227.90] MiB/s` | `0 (0.00%)` | `[2.0408 ms, 2.0529 ms, 2.0686 ms]` | `[410.63, 413.78, 416.22] MiB/s` | `1 (2.00%)` | `1.82×` (`-45.1%`) |
| `osc8_and_graphemes/512k_distinct_links` | `512 KiB`, `768` rotating `OSC 8` URIs (`hyperlinks`) | `[37.839 ms, 37.915 ms, 38.002 ms]` | `[13.157, 13.187, 13.214] MiB/s` | `1 (2.00%)` | `[9.0552 ms, 9.0659 ms, 9.0764 ms]` | `[55.088, 55.152, 55.217] MiB/s` | `0 (0.00%)` | `4.18×` (`-76.1%`) |

### 3.5 `reflow_10k_scrollback` & `reflow_10k_with_2500_osc133_zones`

| Benchmark | Workload Scope | M1 Time `[Low, Est, High]` | M1 Throughput | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|
| `reflow_10k_scrollback/narrow_120x40_to_80x40` | `10,000` scrollback lines | `[6.3153 ms, 6.3791 ms, 6.4754 ms]` | `[1.5443, 1.5676, 1.5835] Melem/s` | `0 (0.00%)` | `[4.6584 ms, 4.6707 ms, 4.6892 ms]` | `[2.1326, 2.1410, 2.1467] Melem/s` | `0 (0.00%)` | `1.37×` (`-26.8%`) |
| `reflow_10k_scrollback/widen_80x40_to_160x40` | `10,000` scrollback lines | `[3.3846 ms, 3.4123 ms, 3.4418 ms]` | `[2.9055, 2.9306, 2.9546] Melem/s` | `1 (5.00%)` | `[3.0594 ms, 3.0739 ms, 3.0874 ms]` | `[3.2390, 3.2532, 3.2687] Melem/s` | `0 (0.00%)` | `1.11×` (`-9.9%`) |
| `reflow_10k_with_2500_osc133_zones/narrow_120x40_to_80x40` | `10,000` lines + `>= 2,500` `OSC 133` zones (`shell-integration`) | `[141.49 ms, 141.61 ms, 141.72 ms]` | `[70.562, 70.616, 70.679] Kelem/s` | `2 (20.00%)` | `[4.1684 ms, 4.1919 ms, 4.2076 ms]` | `[2.3766, 2.3855, 2.3990] Melem/s` | `2 (20.00%)` | `33.78×` (`-97.0%`) |

### 3.6 `input_adapters` (`512 KiB` in `16 KiB` chunks, `64 KiB` scratch read)

| Benchmark | Workload Scope | M1 Time `[Low, Est, High]` | M1 Throughput | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|
| `input_adapters/ring_buffer` | `512 KiB` (`32 × 16 KiB`) -> `RingBufferInput::poll_read` | `[11.018 µs, 11.034 µs, 11.050 µs]` | `[44.187, 44.251, 44.316] GiB/s` | `12 (12.00%)` | `[11.089 µs, 11.138 µs, 11.218 µs]` | `[43.526, 43.840, 44.032] GiB/s` | `5 (5.00%)` | `0.99×` (`+0.9%` time) |
| `input_adapters/channel` | `512 KiB` (`32 × 16 KiB`) -> `ChannelInput::poll_read` | `[10.663 µs, 10.690 µs, 10.715 µs]` | `[45.570, 45.678, 45.790] GiB/s` | `4 (4.00%)` | `[10.708 µs, 10.729 µs, 10.750 µs]` | `[45.420, 45.510, 45.598] GiB/s` | `15 (15.00%)` | `1.00×` (`+0.4%` time) |

---

## 4. Frame Rendering Results (`frame_render` — `18` Benchmarks: `15` Baseline + `3` M5)

Target frame budget for smooth 60 fps rendering: **`< 16.0000 ms`** (`200x60` = `12,000` cells).

### 4.1 `frame_render_stages` (Show vs. Show + Tessellate stage isolation)

| Benchmark | M1 Time `[Low, Est, High]` | M1 Throughput | M1 60fps Share | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 60fps Share | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|---|
| `frame_render_stages/show_only_idle` | `[4.9637 µs, 4.9716 µs, 4.9797 µs]` | `[2.4098, 2.4137, 2.4176] Gelem/s` | `0.03%` | `2 (4.00%)` | `[2.5565 µs, 2.5575 µs, 2.5586 µs]` | `[4.6900, 4.6921, 4.6939] Gelem/s` | `0.02%` | `3 (6.00%)` | `1.94×` (`-48.6%`) |
| `frame_render_stages/show_and_tessellate_idle` | `[144.68 µs, 145.12 µs, 145.70 µs]` | `[82.360, 82.692, 82.943] Melem/s` | `0.91%` | `2 (4.00%)` | `[104.48 µs, 105.52 µs, 106.24 µs]` | `[112.95, 113.72, 114.85] Melem/s` | `0.66%` | `0 (0.00%)` | `1.38×` (`-27.3%`) |
| `frame_render_stages/show_only_full_redraw` | `[1.8716 ms, 1.8777 ms, 1.8840 ms]` | `[6.3694, 6.3910, 6.4117] Melem/s` | `11.74%` | `3 (6.00%)` | `[1.0477 ms, 1.0488 ms, 1.0499 ms]` | `[11.430, 11.442, 11.453] Melem/s` | `6.55%` | `2 (4.00%)` | `1.79×` (`-44.1%`) |
| `frame_render_stages/show_and_tessellate_full_redraw` | `[2.0022 ms, 2.0072 ms, 2.0121 ms]` | `[5.9638, 5.9786, 5.9933] Melem/s` | `12.55%` | `1 (2.00%)` | `[1.1458 ms, 1.1470 ms, 1.1484 ms]` | `[10.449, 10.462, 10.473] Melem/s` | `7.17%` | `2 (4.00%)` | `1.75×` (`-42.9%`) |

### 4.2 `frame_render_200x60_truecolor`

| Benchmark | M1 Time `[Low, Est, High]` | M1 Throughput | M1 60fps Share | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 60fps Share | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|---|
| `frame_render_200x60_truecolor/idle` | `[143.78 µs, 144.26 µs, 144.83 µs]` | `[82.854, 83.185, 83.461] Melem/s` | `0.90%` | `2 (4.00%)` | `[101.76 µs, 104.09 µs, 106.61 µs]` | `[112.56, 115.28, 117.92] Melem/s` | `0.65%` | `7 (14.00%)` | `1.39×` (`-27.8%`) |
| `frame_render_200x60_truecolor/1_dirty_row` | `[232.50 µs, 232.70 µs, 232.94 µs]` | `[51.514, 51.568, 51.613] Melem/s` | `1.45%` | `4 (8.00%)` | `[128.24 µs, 128.57 µs, 128.83 µs]` | `[93.147, 93.332, 93.574] Melem/s` | `0.80%` | `0 (0.00%)` | `1.81×` (`-44.7%`) |
| `frame_render_200x60_truecolor/scroll_1_line` | `[256.22 µs, 256.91 µs, 257.50 µs]` | `[46.602, 46.709, 46.835] Melem/s` | `1.61%` | `0 (0.00%)` | `[194.48 µs, 194.77 µs, 195.04 µs]` | `[61.525, 61.610, 61.704] Melem/s` | `1.22%` | `4 (8.00%)` | `1.32×` (`-24.2%`) |
| `frame_render_200x60_truecolor/full_redraw` | `[1.9874 ms, 1.9922 ms, 1.9974 ms]` | `[6.0077, 6.0236, 6.0381] Melem/s` | `12.45%` | `1 (2.00%)` | `[1.1440 ms, 1.1454 ms, 1.1466 ms]` | `[10.465, 10.477, 10.489] Melem/s` | `7.16%` | `0 (0.00%)` | `1.74×` (`-42.5%`) |
| `frame_render_200x60_truecolor/cursor_blink_toggle` | `[178.97 µs, 179.13 µs, 179.29 µs]` | `[66.930, 66.992, 67.050] Melem/s` | `1.12%` | `2 (4.00%)` | `[92.356 µs, 93.586 µs, 94.441 µs]` | `[127.06, 128.22, 129.93] Melem/s` | `0.58%` | `0 (0.00%)` | `1.91×` (`-47.8%`) |
| `frame_render_200x60_truecolor/active_selection_1k_lines` | `[229.84 µs, 230.84 µs, 232.05 µs]` | `[51.714, 51.984, 52.209] Melem/s` | `1.44%` | `3 (6.00%)` | `[45.991 µs, 46.042 µs, 46.094 µs]` | `[260.34, 260.63, 260.92] Melem/s` | `0.29%` | `2 (4.00%)` | `5.01×` (`-80.1%`) |
| `frame_render_200x60_truecolor/osc133_2500_zones_idle` | `[149.26 µs, 149.86 µs, 150.70 µs]` | `[79.629, 80.074, 80.398] Melem/s` | `0.94%` | `0 (0.00%)` | `[110.30 µs, 110.55 µs, 110.79 µs]` | `[108.31, 108.55, 108.79] Melem/s` | `0.69%` | `3 (6.00%)` | `1.36×` (`-26.2%`) |
| `frame_render_200x60_truecolor/scrollback_scrub_10k` | `[115.84 µs, 116.00 µs, 116.19 µs]` | `[103.28, 103.45, 103.59] Melem/s` | `0.73%` | `7 (14.00%)` | `[89.686 µs, 89.798 µs, 89.910 µs]` | `[133.47, 133.63, 133.80] Melem/s` | `0.56%` | `3 (6.00%)` | `1.29×` (`-22.6%`) |

### 4.3 `frame_render_200x60_utf8` & `frame_render_200x60_tui`

| Benchmark | M1 Time `[Low, Est, High]` | M1 Throughput | M1 60fps Share | M1 Outliers | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 60fps Share | Post-M5 Outliers | Speedup / Change |
|---|---|---|---|---|---|---|---|---|---|
| `frame_render_200x60_utf8/utf8_wide_full_redraw` | `[921.18 µs, 926.20 µs, 934.17 µs]` | `[12.846, 12.956, 13.027] Melem/s` | `5.79%` | `0 (0.00%)` | `[489.25 µs, 489.82 µs, 490.37 µs]` | `[24.471, 24.499, 24.527] Melem/s` | `3.06%` | `3 (10.00%)` | `1.89×` (`-47.1%`) |
| `frame_render_200x60_utf8/utf8_wide_1_dirty_row` | `[175.04 µs, 176.10 µs, 177.71 µs]` | `[67.525, 68.144, 68.554] Melem/s` | `1.10%` | `0 (0.00%)` | `[47.238 µs, 47.323 µs, 47.403 µs]` | `[253.15, 253.58, 254.03] Melem/s` | `0.30%` | `0 (0.00%)` | `3.72×` (`-73.1%`) |
| `frame_render_200x60_tui/vector_glyphs_dashboard` | `[8.4188 ms, 8.8714 ms, 9.3805 ms]` | `[1.2792, 1.3527, 1.4254] Melem/s` | `55.45%` | `0 (0.00%)` | `[488.96 µs, 489.51 µs, 490.17 µs]` | `[24.481, 24.514, 24.542] Melem/s` | `3.06%` | `4 (8.00%)` | `18.12×` (`-94.5%`) |

### 4.4 `frame_render_200x60_m5` (`3` M5 Modern Rendering Benchmarks)

| Benchmark | Feature / Workload Scope | M1 Baseline | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 60fps Share | Post-M5 Outliers | `< 16.0 ms` Budget |
|---|---|---|---|---|---|---|---|
| `frame_render_200x60_m5/bold_and_italic_full_redraw` | `SGR 1` bold + `SGR 3` italic full `200x60` redraw | `N/A (added in M5)` | `[758.14 µs, 759.10 µs, 760.18 µs]` | `[15.786, 15.808, 15.828] Melem/s` | `4.74%` | `6 (12.00%)` | `PASS` (`0.76 ms` / `4.74%`) |
| `frame_render_200x60_m5/search_2500_matches_redraw` | `search` feature, `>= 2,500` active `"HIT"` matches | `N/A (added in M5)` | `[245.69 µs, 245.89 µs, 246.11 µs]` | `[48.760, 48.802, 48.842] Melem/s` | `1.54%` | `3 (6.00%)` | `PASS` (`0.25 ms` / `1.54%`) |
| `frame_render_200x60_m5/kitty_placeholder_grid` | `kitty-graphics` feature, `U+10EEEE` virtual placement `200x60` grid | `N/A (added in M5)` | `[104.04 µs, 104.14 µs, 104.25 µs]` | `[115.11, 115.23, 115.34] Melem/s` | `0.65%` | `1 (2.00%)` | `PASS` (`0.10 ms` / `0.65%`) |

---

## 5. PTY Transport Results (`pty_transport` — `4` Benchmarks)

| Benchmark | Group / Scope | M1 Baseline | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Post-M5 Outliers | Notes |
|---|---|---|---|---|---|---|
| `pty_transport_read/512k_16k_chunks` | `32 × 16 KiB` (`524,288` B) via `PtyBackend` reader -> `poll_read` | `N/A (added in M3)` | `[50.142 µs, 54.929 µs, 59.943 µs]` | `[8.1457, 8.8893, 9.7380] GiB/s` | `1 (1.00%)` | Recycled read buffer pool + `Condvar` wakeup |
| `pty_transport_read/4m_16k_chunks` | `256 × 16 KiB` (`4,194,304` B) via `PtyBackend` reader -> `poll_read` | `N/A (added in M3)` | `[80.073 µs, 80.197 µs, 80.317 µs]` | `[48.635, 48.708, 48.784] GiB/s` | `7 (7.00%)` | Sustained multi-MiB transport throughput |
| `pty_transport_idle/10k_poll_read_calls` | `10,000` non-blocking empty `poll_read` calls | `N/A (added in M3)` | `[39.981 µs, 40.334 µs, 40.744 µs]` | `[245.43, 247.93, 250.12] Melem/s` | `15 (15.00%)` | Decoupled `child.try_wait()` off UI thread (`~4.03 ns/call`) |
| `pty_transport_write/256x64b_burst` | `256 × 64 B` (`16,384` B) `write_bytes` burst | `N/A (added in M3)` | `[9.2580 µs, 9.2702 µs, 9.2828 µs]` | `[1.6438, 1.6460, 1.6482] GiB/s` | `5 (5.00%)` | Dedicated non-blocking writer thread + buffer pool |

---

## 6. Verbatim Post-M5 Criterion Output

<details>
<summary>vt_throughput (env -u RUSTFLAGS cargo bench -p egui_xterm_core --all-features --bench vt_throughput) — 22 benchmarks</summary>

```text
    Finished `bench` profile [optimized] target(s) in 7.53s
     Running benches/vt_throughput.rs (target/release/build/egui_xterm_core/887e37ed2d33dfd8/out/vt_throughput-887e37ed2d33dfd8)
Gnuplot not found, using plotters backend
Benchmarking vt_stream_throughput/parser_only/ascii_plain
Benchmarking vt_stream_throughput/parser_only/ascii_plain: Warming up for 3.0000 s
Benchmarking vt_stream_throughput/parser_only/ascii_plain: Collecting 100 samples in estimated 5.1205 s (66k iterations)
Benchmarking vt_stream_throughput/parser_only/ascii_plain: Analyzing
vt_stream_throughput/parser_only/ascii_plain
                        time:   [77.984 µs 78.312 µs 78.675 µs]
                        thrpt:  [6.2066 GiB/s 6.2354 GiB/s 6.2616 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking vt_stream_throughput/parser_only/utf8_mixed
Benchmarking vt_stream_throughput/parser_only/utf8_mixed: Warming up for 3.0000 s
Benchmarking vt_stream_throughput/parser_only/utf8_mixed: Collecting 100 samples in estimated 6.3346 s (15k iterations)
Benchmarking vt_stream_throughput/parser_only/utf8_mixed: Analyzing
vt_stream_throughput/parser_only/utf8_mixed
                        time:   [415.42 µs 416.37 µs 417.34 µs]
                        thrpt:  [1.1700 GiB/s 1.1728 GiB/s 1.1754 GiB/s]
Benchmarking vt_stream_throughput/parser_only/truecolor_sgr
Benchmarking vt_stream_throughput/parser_only/truecolor_sgr: Warming up for 3.0000 s
Benchmarking vt_stream_throughput/parser_only/truecolor_sgr: Collecting 100 samples in estimated 6.8878 s (10k iterations)
Benchmarking vt_stream_throughput/parser_only/truecolor_sgr: Analyzing
vt_stream_throughput/parser_only/truecolor_sgr
                        time:   [682.98 µs 684.72 µs 686.38 µs]
                        thrpt:  [728.56 MiB/s 730.34 MiB/s 732.20 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
Benchmarking vt_stream_throughput/parser_only/osc133_dense
Benchmarking vt_stream_throughput/parser_only/osc133_dense: Warming up for 3.0000 s
Benchmarking vt_stream_throughput/parser_only/osc133_dense: Collecting 100 samples in estimated 5.7152 s (25k iterations)
Benchmarking vt_stream_throughput/parser_only/osc133_dense: Analyzing
vt_stream_throughput/parser_only/osc133_dense
                        time:   [223.69 µs 224.39 µs 225.07 µs]
                        thrpt:  [1.4488 GiB/s 1.4533 GiB/s 1.4578 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
Benchmarking vt_stream_throughput/terminal/ascii_plain
Benchmarking vt_stream_throughput/terminal/ascii_plain: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.0s, enable flat sampling, or reduce sample count to 60.
Benchmarking vt_stream_throughput/terminal/ascii_plain: Collecting 100 samples in estimated 6.0143 s (5050 iterations)
Benchmarking vt_stream_throughput/terminal/ascii_plain: Analyzing
vt_stream_throughput/terminal/ascii_plain
                        time:   [1.0769 ms 1.0790 ms 1.0811 ms]
                        thrpt:  [462.50 MiB/s 463.41 MiB/s 464.33 MiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
Benchmarking vt_stream_throughput/terminal/utf8_mixed
Benchmarking vt_stream_throughput/terminal/utf8_mixed: Warming up for 3.0000 s
Benchmarking vt_stream_throughput/terminal/utf8_mixed: Collecting 100 samples in estimated 5.3689 s (900 iterations)
Benchmarking vt_stream_throughput/terminal/utf8_mixed: Analyzing
vt_stream_throughput/terminal/utf8_mixed
                        time:   [5.8262 ms 5.8357 ms 5.8461 ms]
                        thrpt:  [85.531 MiB/s 85.683 MiB/s 85.823 MiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking vt_stream_throughput/terminal/truecolor_sgr
Benchmarking vt_stream_throughput/terminal/truecolor_sgr: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.3s, enable flat sampling, or reduce sample count to 50.
Benchmarking vt_stream_throughput/terminal/truecolor_sgr: Collecting 100 samples in estimated 7.3252 s (5050 iterations)
Benchmarking vt_stream_throughput/terminal/truecolor_sgr: Analyzing
vt_stream_throughput/terminal/truecolor_sgr
                        time:   [1.3657 ms 1.3677 ms 1.3699 ms]
                        thrpt:  [365.06 MiB/s 365.64 MiB/s 366.16 MiB/s]
Found 17 outliers among 100 measurements (17.00%)
  8 (8.00%) low mild
  3 (3.00%) high mild
  6 (6.00%) high severe
Benchmarking vt_stream_throughput/terminal/osc133_dense
Benchmarking vt_stream_throughput/terminal/osc133_dense: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.2s, enable flat sampling, or reduce sample count to 60.
Benchmarking vt_stream_throughput/terminal/osc133_dense: Collecting 100 samples in estimated 6.2490 s (5050 iterations)
Benchmarking vt_stream_throughput/terminal/osc133_dense: Analyzing
vt_stream_throughput/terminal/osc133_dense
                        time:   [1.1273 ms 1.1289 ms 1.1305 ms]
                        thrpt:  [295.36 MiB/s 295.80 MiB/s 296.21 MiB/s]
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) low mild
  6 (6.00%) high mild
  2 (2.00%) high severe

Benchmarking terminal_saturated_10k/ascii_with_colons
Benchmarking terminal_saturated_10k/ascii_with_colons: Warming up for 1.0000 s
Benchmarking terminal_saturated_10k/ascii_with_colons: Collecting 20 samples in estimated 4.2108 s (1680 iterations)
Benchmarking terminal_saturated_10k/ascii_with_colons: Analyzing
terminal_saturated_10k/ascii_with_colons
                        time:   [650.09 µs 651.51 µs 652.58 µs]
                        thrpt:  [466.89 MiB/s 467.66 MiB/s 468.69 MiB/s]
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low severe
  2 (10.00%) low mild
Benchmarking terminal_saturated_10k/ascii_no_colons
Benchmarking terminal_saturated_10k/ascii_no_colons: Warming up for 1.0000 s
Benchmarking terminal_saturated_10k/ascii_no_colons: Collecting 20 samples in estimated 4.1862 s (1680 iterations)
Benchmarking terminal_saturated_10k/ascii_no_colons: Analyzing
terminal_saturated_10k/ascii_no_colons
                        time:   [648.27 µs 651.24 µs 655.73 µs]
                        thrpt:  [464.65 MiB/s 467.86 MiB/s 470.00 MiB/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking terminal_saturated_10k/osc133_dense
Benchmarking terminal_saturated_10k/osc133_dense: Warming up for 1.0000 s
Benchmarking terminal_saturated_10k/osc133_dense: Collecting 20 samples in estimated 4.1365 s (1050 iterations)
Benchmarking terminal_saturated_10k/osc133_dense: Analyzing
terminal_saturated_10k/osc133_dense
                        time:   [917.99 µs 923.10 µs 930.91 µs]
                        thrpt:  [293.64 MiB/s 296.13 MiB/s 297.78 MiB/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking terminal_saturated_10k/utf8_mixed
Benchmarking terminal_saturated_10k/utf8_mixed: Warming up for 1.0000 s
Benchmarking terminal_saturated_10k/utf8_mixed: Collecting 20 samples in estimated 4.8946 s (420 iterations)
Benchmarking terminal_saturated_10k/utf8_mixed: Analyzing
terminal_saturated_10k/utf8_mixed
                        time:   [3.1805 ms 3.1948 ms 3.2087 ms]
                        thrpt:  [84.913 MiB/s 85.284 MiB/s 85.667 MiB/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) low mild
  1 (5.00%) high mild

Benchmarking chunked_16k_advance/ascii_plain
Benchmarking chunked_16k_advance/ascii_plain: Warming up for 2.0000 s
Benchmarking chunked_16k_advance/ascii_plain: Collecting 50 samples in estimated 4.6335 s (3825 iterations)
Benchmarking chunked_16k_advance/ascii_plain: Analyzing
chunked_16k_advance/ascii_plain
                        time:   [1.1116 ms 1.1132 ms 1.1149 ms]
                        thrpt:  [448.51 MiB/s 449.17 MiB/s 449.84 MiB/s]
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) low severe
  1 (2.00%) high mild
Benchmarking chunked_16k_advance/truecolor_sgr
Benchmarking chunked_16k_advance/truecolor_sgr: Warming up for 2.0000 s
Benchmarking chunked_16k_advance/truecolor_sgr: Collecting 50 samples in estimated 5.5946 s (3825 iterations)
Benchmarking chunked_16k_advance/truecolor_sgr: Analyzing
chunked_16k_advance/truecolor_sgr
                        time:   [1.3882 ms 1.3908 ms 1.3936 ms]
                        thrpt:  [358.84 MiB/s 359.56 MiB/s 360.24 MiB/s]
Found 1 outliers among 50 measurements (2.00%)
  1 (2.00%) high mild
Benchmarking chunked_16k_advance/osc133_dense
Benchmarking chunked_16k_advance/osc133_dense: Warming up for 2.0000 s
Benchmarking chunked_16k_advance/osc133_dense: Collecting 50 samples in estimated 4.6872 s (3825 iterations)
Benchmarking chunked_16k_advance/osc133_dense: Analyzing
chunked_16k_advance/osc133_dense
                        time:   [1.1584 ms 1.1631 ms 1.1686 ms]
                        thrpt:  [285.75 MiB/s 287.09 MiB/s 288.25 MiB/s]

Benchmarking sync_output_dec2026_200x60/truecolor_combining_frames
Benchmarking sync_output_dec2026_200x60/truecolor_combining_frames: Warming up for 2.0000 s
Benchmarking sync_output_dec2026_200x60/truecolor_combining_frames: Collecting 50 samples in estimated 5.3468 s (2550 iterations)
Benchmarking sync_output_dec2026_200x60/truecolor_combining_frames: Analyzing
sync_output_dec2026_200x60/truecolor_combining_frames
                        time:   [2.0408 ms 2.0529 ms 2.0686 ms]
                        thrpt:  [410.63 MiB/s 413.78 MiB/s 416.22 MiB/s]
Found 1 outliers among 50 measurements (2.00%)
  1 (2.00%) high mild

Benchmarking osc8_and_graphemes/512k_distinct_links
Benchmarking osc8_and_graphemes/512k_distinct_links: Warming up for 2.0000 s
Benchmarking osc8_and_graphemes/512k_distinct_links: Collecting 50 samples in estimated 4.2416 s (450 iterations)
Benchmarking osc8_and_graphemes/512k_distinct_links: Analyzing
osc8_and_graphemes/512k_distinct_links
                        time:   [9.0552 ms 9.0659 ms 9.0764 ms]
                        thrpt:  [55.088 MiB/s 55.152 MiB/s 55.217 MiB/s]

Benchmarking reflow_10k_scrollback/narrow_120x40_to_80x40
Benchmarking reflow_10k_scrollback/narrow_120x40_to_80x40: Warming up for 3.0000 s
Benchmarking reflow_10k_scrollback/narrow_120x40_to_80x40: Collecting 20 samples in estimated 5.0385 s (630 iterations)
Benchmarking reflow_10k_scrollback/narrow_120x40_to_80x40: Analyzing
reflow_10k_scrollback/narrow_120x40_to_80x40
                        time:   [4.6584 ms 4.6707 ms 4.6892 ms]
                        thrpt:  [2.1326 Melem/s 2.1410 Melem/s 2.1467 Melem/s]
Benchmarking reflow_10k_scrollback/widen_80x40_to_160x40
Benchmarking reflow_10k_scrollback/widen_80x40_to_160x40: Warming up for 3.0000 s
Benchmarking reflow_10k_scrollback/widen_80x40_to_160x40: Collecting 20 samples in estimated 5.2513 s (1050 iterations)
Benchmarking reflow_10k_scrollback/widen_80x40_to_160x40: Analyzing
reflow_10k_scrollback/widen_80x40_to_160x40
                        time:   [3.0594 ms 3.0739 ms 3.0874 ms]
                        thrpt:  [3.2390 Melem/s 3.2532 Melem/s 3.2687 Melem/s]

Benchmarking reflow_10k_with_2500_osc133_zones/narrow_120x40_to_80x40
Benchmarking reflow_10k_with_2500_osc133_zones/narrow_120x40_to_80x40: Warming up for 1.0000 s
Benchmarking reflow_10k_with_2500_osc133_zones/narrow_120x40_to_80x40: Collecting 10 samples in estimated 5.3745 s (770 iterations)
Benchmarking reflow_10k_with_2500_osc133_zones/narrow_120x40_to_80x40: Analyzing
reflow_10k_with_2500_osc133_zones/narrow_120x40_to_80x40
                        time:   [4.1684 ms 4.1919 ms 4.2076 ms]
                        thrpt:  [2.3766 Melem/s 2.3855 Melem/s 2.3990 Melem/s]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high mild

Benchmarking input_adapters/ring_buffer
Benchmarking input_adapters/ring_buffer: Warming up for 1.0000 s
Benchmarking input_adapters/ring_buffer: Collecting 100 samples in estimated 3.0577 s (232k iterations)
Benchmarking input_adapters/ring_buffer: Analyzing
input_adapters/ring_buffer
                        time:   [11.089 µs 11.138 µs 11.218 µs]
                        thrpt:  [43.526 GiB/s 43.840 GiB/s 44.032 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low severe
  2 (2.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking input_adapters/channel
Benchmarking input_adapters/channel: Warming up for 1.0000 s
Benchmarking input_adapters/channel: Collecting 100 samples in estimated 3.0548 s (141k iterations)
Benchmarking input_adapters/channel: Analyzing
input_adapters/channel  time:   [10.708 µs 10.729 µs 10.750 µs]
                        thrpt:  [45.420 GiB/s 45.510 GiB/s 45.598 GiB/s]
Found 15 outliers among 100 measurements (15.00%)
  9 (9.00%) low severe
  6 (6.00%) low mild
```

</details>

<details>
<summary>frame_render (env -u RUSTFLAGS cargo bench -p egui_xterm --all-features --bench frame_render) — 18 benchmarks</summary>

```text
    Finished `bench` profile [optimized] target(s) in 14.52s
     Running benches/frame_render.rs (target/release/build/egui_xterm/37dacbf5a823250e/out/frame_render-37dacbf5a823250e)
Gnuplot not found, using plotters backend
Benchmarking frame_render_stages/show_only_idle
Benchmarking frame_render_stages/show_only_idle: Warming up for 2.0000 s
Benchmarking frame_render_stages/show_only_idle: Collecting 50 samples in estimated 4.0020 s (1.6M iterations)
Benchmarking frame_render_stages/show_only_idle: Analyzing
frame_render_stages/show_only_idle
                        time:   [2.5565 µs 2.5575 µs 2.5586 µs]
                        thrpt:  [4.6900 Gelem/s 4.6921 Gelem/s 4.6939 Gelem/s]
Found 3 outliers among 50 measurements (6.00%)
  2 (4.00%) high mild
  1 (2.00%) high severe
Benchmarking frame_render_stages/show_and_tessellate_idle
Benchmarking frame_render_stages/show_and_tessellate_idle: Warming up for 2.0000 s
Benchmarking frame_render_stages/show_and_tessellate_idle: Collecting 50 samples in estimated 4.1245 s (42k iterations)
Benchmarking frame_render_stages/show_and_tessellate_idle: Analyzing
frame_render_stages/show_and_tessellate_idle
                        time:   [104.48 µs 105.52 µs 106.24 µs]
                        thrpt:  [112.95 Melem/s 113.72 Melem/s 114.85 Melem/s]
Benchmarking frame_render_stages/show_only_full_redraw
Benchmarking frame_render_stages/show_only_full_redraw: Warming up for 2.0000 s
Benchmarking frame_render_stages/show_only_full_redraw: Collecting 50 samples in estimated 4.0149 s (3825 iterations)
Benchmarking frame_render_stages/show_only_full_redraw: Analyzing
frame_render_stages/show_only_full_redraw
                        time:   [1.0477 ms 1.0488 ms 1.0499 ms]
                        thrpt:  [11.430 Melem/s 11.442 Melem/s 11.453 Melem/s]
Found 2 outliers among 50 measurements (4.00%)
  2 (4.00%) high mild
Benchmarking frame_render_stages/show_and_tessellate_full_redraw
Benchmarking frame_render_stages/show_and_tessellate_full_redraw: Warming up for 2.0000 s
Benchmarking frame_render_stages/show_and_tessellate_full_redraw: Collecting 50 samples in estimated 4.4070 s (3825 iterations)
Benchmarking frame_render_stages/show_and_tessellate_full_redraw: Analyzing
frame_render_stages/show_and_tessellate_full_redraw
                        time:   [1.1458 ms 1.1470 ms 1.1484 ms]
                        thrpt:  [10.449 Melem/s 10.462 Melem/s 10.473 Melem/s]
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) low mild
  1 (2.00%) high mild

Benchmarking frame_render_200x60_truecolor/idle
Benchmarking frame_render_200x60_truecolor/idle: Warming up for 2.0000 s
Benchmarking frame_render_200x60_truecolor/idle: Collecting 50 samples in estimated 4.0918 s (38k iterations)
Benchmarking frame_render_200x60_truecolor/idle: Analyzing
frame_render_200x60_truecolor/idle
                        time:   [101.76 µs 104.09 µs 106.61 µs]
                        thrpt:  [112.56 Melem/s 115.28 Melem/s 117.92 Melem/s]
Found 7 outliers among 50 measurements (14.00%)
  6 (12.00%) low severe
  1 (2.00%) low mild
Benchmarking frame_render_200x60_truecolor/1_dirty_row
Benchmarking frame_render_200x60_truecolor/1_dirty_row: Warming up for 2.0000 s
Benchmarking frame_render_200x60_truecolor/1_dirty_row: Collecting 50 samples in estimated 4.1034 s (34k iterations)
Benchmarking frame_render_200x60_truecolor/1_dirty_row: Analyzing
frame_render_200x60_truecolor/1_dirty_row
                        time:   [128.24 µs 128.57 µs 128.83 µs]
                        thrpt:  [93.147 Melem/s 93.332 Melem/s 93.574 Melem/s]
Benchmarking frame_render_200x60_truecolor/scroll_1_line
Benchmarking frame_render_200x60_truecolor/scroll_1_line: Warming up for 2.0000 s
Benchmarking frame_render_200x60_truecolor/scroll_1_line: Collecting 50 samples in estimated 4.1735 s (22k iterations)
Benchmarking frame_render_200x60_truecolor/scroll_1_line: Analyzing
frame_render_200x60_truecolor/scroll_1_line
                        time:   [194.48 µs 194.77 µs 195.04 µs]
                        thrpt:  [61.525 Melem/s 61.610 Melem/s 61.704 Melem/s]
Found 4 outliers among 50 measurements (8.00%)
  1 (2.00%) low severe
  1 (2.00%) low mild
  2 (4.00%) high mild
Benchmarking frame_render_200x60_truecolor/full_redraw
Benchmarking frame_render_200x60_truecolor/full_redraw: Warming up for 2.0000 s
Benchmarking frame_render_200x60_truecolor/full_redraw: Collecting 50 samples in estimated 4.3890 s (3825 iterations)
Benchmarking frame_render_200x60_truecolor/full_redraw: Analyzing
frame_render_200x60_truecolor/full_redraw
                        time:   [1.1440 ms 1.1454 ms 1.1466 ms]
                        thrpt:  [10.465 Melem/s 10.477 Melem/s 10.489 Melem/s]
Benchmarking frame_render_200x60_truecolor/cursor_blink_toggle
Benchmarking frame_render_200x60_truecolor/cursor_blink_toggle: Warming up for 2.0000 s
Benchmarking frame_render_200x60_truecolor/cursor_blink_toggle: Collecting 50 samples in estimated 4.0363 s (47k iterations)
Benchmarking frame_render_200x60_truecolor/cursor_blink_toggle: Analyzing
frame_render_200x60_truecolor/cursor_blink_toggle
                        time:   [92.356 µs 93.586 µs 94.441 µs]
                        thrpt:  [127.06 Melem/s 128.22 Melem/s 129.93 Melem/s]
Benchmarking frame_render_200x60_truecolor/active_selection_1k_lines
Benchmarking frame_render_200x60_truecolor/active_selection_1k_lines: Warming up for 2.0000 s
Benchmarking frame_render_200x60_truecolor/active_selection_1k_lines: Collecting 50 samples in estimated 4.0402 s (125k iterations)
Benchmarking frame_render_200x60_truecolor/active_selection_1k_lines: Analyzing
frame_render_200x60_truecolor/active_selection_1k_lines
                        time:   [45.991 µs 46.042 µs 46.094 µs]
                        thrpt:  [260.34 Melem/s 260.63 Melem/s 260.92 Melem/s]
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) low severe
  1 (2.00%) low mild
Benchmarking frame_render_200x60_truecolor/osc133_2500_zones_idle
Benchmarking frame_render_200x60_truecolor/osc133_2500_zones_idle: Warming up for 2.0000 s
Benchmarking frame_render_200x60_truecolor/osc133_2500_zones_idle: Collecting 50 samples in estimated 4.1074 s (37k iterations)
Benchmarking frame_render_200x60_truecolor/osc133_2500_zones_idle: Analyzing
frame_render_200x60_truecolor/osc133_2500_zones_idle
                        time:   [110.30 µs 110.55 µs 110.79 µs]
                        thrpt:  [108.31 Melem/s 108.55 Melem/s 108.79 Melem/s]
Found 3 outliers among 50 measurements (6.00%)
  1 (2.00%) low mild
  2 (4.00%) high severe
Benchmarking frame_render_200x60_truecolor/scrollback_scrub_10k
Benchmarking frame_render_200x60_truecolor/scrollback_scrub_10k: Warming up for 2.0000 s
Benchmarking frame_render_200x60_truecolor/scrollback_scrub_10k: Collecting 50 samples in estimated 4.0242 s (45k iterations)
Benchmarking frame_render_200x60_truecolor/scrollback_scrub_10k: Analyzing
frame_render_200x60_truecolor/scrollback_scrub_10k
                        time:   [89.686 µs 89.798 µs 89.910 µs]
                        thrpt:  [133.47 Melem/s 133.63 Melem/s 133.80 Melem/s]
Found 3 outliers among 50 measurements (6.00%)
  1 (2.00%) low severe
  1 (2.00%) low mild
  1 (2.00%) high mild

Benchmarking frame_render_200x60_utf8/utf8_wide_full_redraw
Benchmarking frame_render_200x60_utf8/utf8_wide_full_redraw: Warming up for 2.0000 s
Benchmarking frame_render_200x60_utf8/utf8_wide_full_redraw: Collecting 30 samples in estimated 4.0965 s (8370 iterations)
Benchmarking frame_render_200x60_utf8/utf8_wide_full_redraw: Analyzing
frame_render_200x60_utf8/utf8_wide_full_redraw
                        time:   [489.25 µs 489.82 µs 490.37 µs]
                        thrpt:  [24.471 Melem/s 24.499 Melem/s 24.527 Melem/s]
Found 3 outliers among 30 measurements (10.00%)
  3 (10.00%) low mild
Benchmarking frame_render_200x60_utf8/utf8_wide_1_dirty_row
Benchmarking frame_render_200x60_utf8/utf8_wide_1_dirty_row: Warming up for 2.0000 s
Benchmarking frame_render_200x60_utf8/utf8_wide_1_dirty_row: Collecting 50 samples in estimated 4.0393 s (85k iterations)
Benchmarking frame_render_200x60_utf8/utf8_wide_1_dirty_row: Analyzing
frame_render_200x60_utf8/utf8_wide_1_dirty_row
                        time:   [47.238 µs 47.323 µs 47.403 µs]
                        thrpt:  [253.15 Melem/s 253.58 Melem/s 254.03 Melem/s]

Benchmarking frame_render_200x60_tui/vector_glyphs_dashboard
Benchmarking frame_render_200x60_tui/vector_glyphs_dashboard: Warming up for 2.0000 s
Benchmarking frame_render_200x60_tui/vector_glyphs_dashboard: Collecting 50 samples in estimated 4.3917 s (8925 iterations)
Benchmarking frame_render_200x60_tui/vector_glyphs_dashboard: Analyzing
frame_render_200x60_tui/vector_glyphs_dashboard
                        time:   [488.96 µs 489.51 µs 490.17 µs]
                        thrpt:  [24.481 Melem/s 24.514 Melem/s 24.542 Melem/s]
Found 4 outliers among 50 measurements (8.00%)
  2 (4.00%) high mild
  2 (4.00%) high severe

Benchmarking frame_render_200x60_m5/bold_and_italic_full_redraw
Benchmarking frame_render_200x60_m5/bold_and_italic_full_redraw: Warming up for 2.0000 s
Benchmarking frame_render_200x60_m5/bold_and_italic_full_redraw: Collecting 50 samples in estimated 4.8018 s (6375 iterations)
Benchmarking frame_render_200x60_m5/bold_and_italic_full_redraw: Analyzing
frame_render_200x60_m5/bold_and_italic_full_redraw
                        time:   [758.14 µs 759.10 µs 760.18 µs]
                        thrpt:  [15.786 Melem/s 15.808 Melem/s 15.828 Melem/s]
Found 6 outliers among 50 measurements (12.00%)
  1 (2.00%) low severe
  2 (4.00%) low mild
  3 (6.00%) high mild
Benchmarking frame_render_200x60_m5/search_2500_matches_redraw
Benchmarking frame_render_200x60_m5/search_2500_matches_redraw: Warming up for 2.0000 s
Benchmarking frame_render_200x60_m5/search_2500_matches_redraw: Collecting 50 samples in estimated 4.0557 s (17k iterations)
Benchmarking frame_render_200x60_m5/search_2500_matches_redraw: Analyzing
frame_render_200x60_m5/search_2500_matches_redraw
                        time:   [245.69 µs 245.89 µs 246.11 µs]
                        thrpt:  [48.760 Melem/s 48.802 Melem/s 48.842 Melem/s]
Found 3 outliers among 50 measurements (6.00%)
  2 (4.00%) low severe
  1 (2.00%) high mild
Benchmarking frame_render_200x60_m5/kitty_placeholder_grid
Benchmarking frame_render_200x60_m5/kitty_placeholder_grid: Warming up for 2.0000 s
Benchmarking frame_render_200x60_m5/kitty_placeholder_grid: Collecting 50 samples in estimated 4.1163 s (40k iterations)
Benchmarking frame_render_200x60_m5/kitty_placeholder_grid: Analyzing
frame_render_200x60_m5/kitty_placeholder_grid
                        time:   [104.04 µs 104.14 µs 104.25 µs]
                        thrpt:  [115.11 Melem/s 115.23 Melem/s 115.34 Melem/s]
Found 1 outliers among 50 measurements (2.00%)
  1 (2.00%) low mild
```

</details>

<details>
<summary>pty_transport (env -u RUSTFLAGS cargo bench -p egui_xterm_pty --bench pty_transport) — 4 benchmarks</summary>

```text
    Finished `bench` profile [optimized] target(s) in 3.32s
     Running benches/pty_transport.rs (target/release/build/egui_xterm_pty/c35fcf0e3d3e0b2c/out/pty_transport-c35fcf0e3d3e0b2c)
Gnuplot not found, using plotters backend
Benchmarking pty_transport_read/512k_16k_chunks
Benchmarking pty_transport_read/512k_16k_chunks: Warming up for 3.0000 s
Benchmarking pty_transport_read/512k_16k_chunks: Collecting 100 samples in estimated 5.2713 s (91k iterations)
Benchmarking pty_transport_read/512k_16k_chunks: Analyzing
pty_transport_read/512k_16k_chunks
                        time:   [50.142 µs 54.929 µs 59.943 µs]
                        thrpt:  [8.1457 GiB/s 8.8893 GiB/s 9.7380 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking pty_transport_read/4m_16k_chunks
Benchmarking pty_transport_read/4m_16k_chunks: Warming up for 3.0000 s
Benchmarking pty_transport_read/4m_16k_chunks: Collecting 100 samples in estimated 5.2676 s (66k iterations)
Benchmarking pty_transport_read/4m_16k_chunks: Analyzing
pty_transport_read/4m_16k_chunks
                        time:   [80.073 µs 80.197 µs 80.317 µs]
                        thrpt:  [48.635 GiB/s 48.708 GiB/s 48.784 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  6 (6.00%) low mild
  1 (1.00%) high mild

Benchmarking pty_transport_idle/10k_poll_read_calls
Benchmarking pty_transport_idle/10k_poll_read_calls: Warming up for 3.0000 s
Benchmarking pty_transport_idle/10k_poll_read_calls: Collecting 100 samples in estimated 5.0899 s (126k iterations)
Benchmarking pty_transport_idle/10k_poll_read_calls: Analyzing
pty_transport_idle/10k_poll_read_calls
                        time:   [39.981 µs 40.334 µs 40.744 µs]
                        thrpt:  [245.43 Melem/s 247.93 Melem/s 250.12 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  2 (2.00%) low mild
  13 (13.00%) high severe

Benchmarking pty_transport_write/256x64b_burst
Benchmarking pty_transport_write/256x64b_burst: Warming up for 3.0000 s
Benchmarking pty_transport_write/256x64b_burst: Collecting 100 samples in estimated 5.0008 s (348k iterations)
Benchmarking pty_transport_write/256x64b_burst: Analyzing
pty_transport_write/256x64b_burst
                        time:   [9.2580 µs 9.2702 µs 9.2828 µs]
                        thrpt:  [1.6438 GiB/s 1.6460 GiB/s 1.6482 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
```

</details>
