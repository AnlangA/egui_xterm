# `egui_xterm` Pre-Optimization Criterion Baseline (Milestone M1)

> **Scope**: Untouched pre-optimization performance baseline captured in Milestone M1 (`worker_m1_1`) after enabling workspace safety/clippy lints and non-behavioral panic/silent-drop cleanups, prior to any M2–M5 algorithmic or transport optimizations.

---

## 1. Host, Toolchain & Run Metadata

| Field | Value |
|---|---|
| **Timestamp (UTC)** | `2026-11-21T01:42:23Z` |
| **Base Git Commit** | `387887e4846dbf9c516881f52d690fb407a10693` |
| **OS** | `macOS 27.2.1` (`BuildVersion: 26B131`, `arm64`) |
| **CPU** | `Apple M4 Max` (`16` logical CPUs) |
| **Memory** | `48 GiB` (`51,539,607,552` bytes) |
| **Compiler (`rustc`)** | `rustc 1.99.0-nightly (375b1431b 2026-07-10)` |
| **Cargo** | `cargo 1.99.0-nightly (59800466c 2026-07-07)` |
| **Benchmark Harness** | `criterion 0.8` (`--bench` / `--release`, default `RUSTFLAGS`) |

### Exact Reproduction Commands

```bash
cargo bench -p egui_xterm_core --all-features --bench vt_throughput
cargo bench -p egui_xterm --all-features --bench frame_render
```

---

## 2. Core Pipeline Baseline (`vt_throughput`) — 22 Benchmarks

### 2.1 `vt_stream_throughput` (`512 KiB` single-shot streams, `120x40` terminal)

| Benchmark | Time `[Low, Est, High]` | Throughput | Outliers | Notes / Bottleneck Tag |
|---|---|---|---|---|
| `parser_only/ascii_plain` | `[71.769 µs, 72.237 µs, 72.781 µs]` | `[6.7092, 6.7598, 6.8039] GiB/s` | `1 (1.00%)` | SWAR `print_ascii_run` ceiling |
| `parser_only/utf8_mixed` | `[1.5145 ms, 1.5585 ms, 1.5999 ms]` | `[312.53, 320.83, 330.15] MiB/s` | `4 (4.00%)` | **B7**: byte-by-byte UTF-8 decode + `print()` |
| `parser_only/truecolor_sgr` | `[1.5980 ms, 1.6088 ms, 1.6216 ms]` | `[308.39, 310.84, 312.93] MiB/s` | `0 (0.00%)` | **B7**: CSI param state machine transitions |
| `parser_only/osc133_dense` | `[354.63 µs, 365.16 µs, 378.39 µs]` | `[882.46, 914.45, 941.61] MiB/s` | `5 (5.00%)` | **B7**: byte-by-byte `OscString` push |
| `terminal/ascii_plain` | `[2.2026 ms, 2.2128 ms, 2.2239 ms]` | `[224.85, 225.97, 227.02] MiB/s` | `14 (14.00%)` | **B1**: colons trigger `ImplicitLinkStore` scan |
| `terminal/utf8_mixed` | `[4.9204 ms, 4.9337 ms, 4.9472 ms]` | `[101.07, 101.35, 101.62] MiB/s` | `0 (0.00%)` | **B7** + **B17**: byte-wise UTF-8 + `CellExtraTable` combining marks |
| `terminal/truecolor_sgr` | `[2.5811 ms, 2.5869 ms, 2.5926 ms]` | `[192.88, 193.31, 193.74] MiB/s` | `1 (1.00%)` | **B7** + **B17**: CSI/SGR parser + SGR `58:2` `CellExtra` alloc |
| `terminal/osc133_dense` | `[2.4134 ms, 2.4175 ms, 2.4218 ms]` | `[137.88, 138.12, 138.36] MiB/s` | `5 (5.00%)` | **B5** + **B7** + **B12**: `ShellIntegrationState` + `OscString` + `mem::take` |

### 2.2 `terminal_saturated_10k` (`10,000` pre-filled scrollback lines + `4,096` lines in `16 KiB` chunks)

| Benchmark | Time `[Low, Est, High]` | Throughput | Outliers | Notes / Bottleneck Tag |
|---|---|---|---|---|
| `ascii_with_colons` | `[1.7483 ms, 1.7519 ms, 1.7552 ms]` | `[173.59, 173.92, 174.27] MiB/s` | `2 (10.00%)` | **B1** + **B14** + **B18**: `ImplicitLinkStore` full 10k scan + `:` extraction |
| `ascii_no_colons` | `[1.1115 ms, 1.1129 ms, 1.1144 ms]` | `[273.42, 273.79, 274.11] MiB/s` | `1 (5.00%)` | **B1** + **B18**: baseline without `:` extraction (`+57.4%` vs colons) |
| `osc133_dense` | `[3.5116 ms, 3.5189 ms, 3.5266 ms]` | `[77.513, 77.682, 77.844] MiB/s` | `1 (5.00%)` | **B5** + **B7**: `ShellIntegrationState` eviction rebuild (`Vec` shift) |
| `utf8_mixed` | `[3.1244 ms, 3.1418 ms, 3.1597 ms]` | `[86.230, 86.721, 87.204] MiB/s` | `0 (0.00%)` | **B1** + **B7** + **B17**: saturated 10k + UTF-8/graphemes |

### 2.3 `chunked_16k_advance` (Fresh `120x40` terminal, `16 KiB` read-chunk simulation)

| Benchmark | Time `[Low, Est, High]` | Throughput | Outliers | Notes / Bottleneck Tag |
|---|---|---|---|---|
| `ascii_plain` | `[2.4313 ms, 2.4361 ms, 2.4421 ms]` | `[204.75, 205.26, 205.66] MiB/s` | `3 (6.00%)` | **B1** + **B12**: 33× `advance` calls (`+10.1%` vs 1-shot) |
| `truecolor_sgr` | `[2.7455 ms, 2.7506 ms, 2.7567 ms]` | `[181.40, 181.81, 182.14] MiB/s` | `4 (8.00%)` | **B7** + **B12**: chunked CSI/SGR state |
| `osc133_dense` | `[2.4540 ms, 2.4590 ms, 2.4643 ms]` | `[135.50, 135.80, 136.07] MiB/s` | `2 (4.00%)` | **B5** + **B7** + **B12**: chunked OSC 133 |

### 2.4 `sync_output_dec2026_200x60` & `osc8_and_graphemes`

| Benchmark | Time `[Low, Est, High]` | Throughput | Outliers | Notes / Bottleneck Tag |
|---|---|---|---|---|
| `sync_output_dec2026_200x60/truecolor_combining_frames` | `[3.7272 ms, 3.7410 ms, 3.7551 ms]` | `[226.20, 227.06, 227.90] MiB/s` | `0 (0.00%)` | **B15** + **B17**: `LatchedFrame` snapshot + `CellExtra` clone |
| `osc8_and_graphemes/512k_distinct_links` | `[37.839 ms, 37.915 ms, 38.002 ms]` | `[13.157, 13.187, 13.214] MiB/s` | `1 (2.00%)` | **B12** + **B17**: `HyperlinkPool` GC + `OSC 8` dispatch + `CellExtraTable` churn |

### 2.5 `reflow_10k_scrollback` & `reflow_10k_with_2500_osc133_zones`

| Benchmark | Time `[Low, Est, High]` | Throughput | Outliers | Notes / Bottleneck Tag |
|---|---|---|---|---|
| `reflow_10k_scrollback/narrow_120x40_to_80x40` | `[6.3153 ms, 6.3791 ms, 6.4754 ms]` | `[1.5443, 1.5676, 1.5835] Melem/s` | `0 (0.00%)` | **B4** + **B14** + **B18**: per-row `Vec<Cell>` allocs + `trimmed_len()` scans during reflow |
| `reflow_10k_scrollback/widen_80x40_to_160x40` | `[3.3846 ms, 3.4123 ms, 3.4418 ms]` | `[2.9055, 2.9306, 2.9546] Melem/s` | `1 (5.00%)` | **B4** + **B14** + **B18**: line-joining reflow + `trimmed_len()` scans |
| `reflow_10k_with_2500_osc133_zones/narrow_120x40_to_80x40` | `[141.49 ms, 141.61 ms, 141.72 ms]` | `[70.562, 70.616, 70.679] Kelem/s` | `2 (20.00%)` | **B4**: `O(L × Z)` anchor scan (`22.2×` slower than plain reflow) |

### 2.6 `input_adapters` (`512 KiB` in `16 KiB` chunks, `64 KiB` scratch read)

| Benchmark | Time `[Low, Est, High]` | Throughput | Outliers | Notes / Bottleneck Tag |
|---|---|---|---|---|
| `input_adapters/ring_buffer` | `[11.018 µs, 11.034 µs, 11.050 µs]` | `[44.187, 44.251, 44.316] GiB/s` | `12 (12.00%)` | **B11** baseline: in-memory ring copy (live PTY evaluated in M3) |
| `input_adapters/channel` | `[10.663 µs, 10.690 µs, 10.715 µs]` | `[45.570, 45.678, 45.790] GiB/s` | `4 (4.00%)` | **B11** baseline: `mpsc::channel` drain (live PTY evaluated in M3) |

---

## 3. Frame Rendering Baseline (`frame_render`, `200x60` = `12,000` cells) — 15 Benchmarks

### 3.1 `frame_render_stages` (Show vs Show+Tessellate stage isolation)

| Benchmark | Time `[Low, Est, High]` | Throughput | 60fps (`16.0 ms`) Share | Outliers | Notes / Bottleneck Tag |
|---|---|---|---|---|---|
| `show_only_idle` | `[4.9637 µs, 4.9716 µs, 4.9797 µs]` | `2.4137 Gelem/s` | `0.03%` | `2 (4.00%)` | **B9** + **B10**: idle hashing/`title().to_owned()`/`active_snapshot()` + `overlay_mesh` rebuild |
| `show_and_tessellate_idle` | `[144.68 µs, 145.12 µs, 145.70 µs]` | `82.692 Melem/s` | `0.91%` | `2 (4.00%)` | **B2**: `epaint::Tessellator` re-tessellating 60 row shapes (`~140 µs`) |
| `show_only_full_redraw` | `[1.8716 ms, 1.8777 ms, 1.8840 ms]` | `6.3910 Melem/s` | `11.74%` | `3 (6.00%)` | **B1** + **B3** + **B8** + **B10**: VT advance + `bg_mesh` + `LayoutJob` + `overlay_mesh` |
| `show_and_tessellate_full_redraw` | `[2.0022 ms, 2.0072 ms, 2.0121 ms]` | `5.9786 Melem/s` | `12.55%` | `1 (2.00%)` | **B2** + **B3** + **B8**: full frame + 60-shape tessellation |

### 3.2 `frame_render_200x60_truecolor`

| Benchmark | Time `[Low, Est, High]` | Throughput | 60fps (`16.0 ms`) Share | Outliers | Notes / Bottleneck Tag |
|---|---|---|---|---|---|
| `idle` | `[143.78 µs, 144.26 µs, 144.83 µs]` | `83.185 Melem/s` | `0.90%` | `2 (4.00%)` | **B2**: 60-shape tessellation overhead |
| `1_dirty_row` | `[232.50 µs, 232.70 µs, 232.94 µs]` | `51.568 Melem/s` | `1.45%` | `4 (8.00%)` | **B3** + **B8**: single dirty row invalidates full `BgCacheKey` + dirty-row `LayoutJob` |
| `scroll_1_line` | `[256.22 µs, 256.91 µs, 257.50 µs]` | `46.709 Melem/s` | `1.61%` | `0 (0.00%)` | **B3** + **B8**: `BgCacheKey` rebuild + shifted-row `LayoutJob` |
| `full_redraw` | `[1.9874 ms, 1.9922 ms, 1.9974 ms]` | `6.0236 Melem/s` | `12.45%` | `1 (2.00%)` | **B2** + **B3** + **B8**: 12k-cell `bg_mesh` + `LayoutJob` + 60-shape tessellation |
| `cursor_blink_toggle` | `[178.97 µs, 179.13 µs, 179.29 µs]` | `66.992 Melem/s` | `1.12%` | `2 (4.00%)` | **B3**: blink toggle invalidates entire `BgCacheKey` (`+34.9 µs`) |
| `active_selection_1k_lines` | `[229.84 µs, 230.84 µs, 232.05 µs]` | `51.984 Melem/s` | `1.44%` | `3 (6.00%)` | **B6**: `context_menu` calls `selection_text()` (`~212 KB`) every idle frame (`+86.6 µs`) |
| `osc133_2500_zones_idle` | `[149.26 µs, 149.86 µs, 150.70 µs]` | `80.074 Melem/s` | `0.94%` | `0 (0.00%)` | **B10**: linear `paint_prompt_gutter()` zone scan (`+5.6 µs` idle) |
| `scrollback_scrub_10k` | `[115.84 µs, 116.00 µs, 116.19 µs]` | `103.45 Melem/s` | `0.73%` | `7 (14.00%)` | **B3** + **B10**: `BgCacheKey` rebuild + `overlay_mesh`/gutter scan on scrollback scrub |

### 3.3 `frame_render_200x60_utf8` & `frame_render_200x60_tui`

| Benchmark | Time `[Low, Est, High]` | Throughput | 60fps (`16.0 ms`) Share | Outliers | Notes / Bottleneck Tag |
|---|---|---|---|---|---|
| `utf8_wide_full_redraw` | `[921.18 µs, 926.20 µs, 934.17 µs]` | `12.956 Melem/s` | `5.79%` | `0 (0.00%)` | **B13** + **B3** + **B2**: slow-path `fonts.layout_job` & `ClusterWidthKey` across CJK/ZWJ/combining |
| `utf8_wide_1_dirty_row` | `[175.04 µs, 176.10 µs, 177.71 µs]` | `68.144 Melem/s` | `1.10%` | `0 (0.00%)` | **B13** + **B3**: single slow-path row + `BgCacheKey` rebuild |
| `vector_glyphs_dashboard` | `[8.4188 ms, 8.8714 ms, 9.3805 ms]` | `1.3527 Melem/s` | `55.45%` | `0 (0.00%)` | **B10** + **B13** + **B3**: `vector-glyphs` `overlay_mesh` + curly underline `PathShape` + slow-path rows (`8.87 ms`!) |

---

## 4. Key Pre-Optimization Diagnostic Observations (`B1`–`B18`)

All IDs below match the canonical bottleneck catalog (`PROJECT.md` items #6, #7, #10):

1. **`B1` (`ImplicitLinkStore::refresh` full 10k scan on `advance`)**: Under saturated `10,000`-line scrollback, `terminal_saturated_10k/ascii_with_colons` (`1.7519 ms`) is **`57.4%` slower** than `ascii_no_colons` (`1.1129 ms`), and `terminal/ascii_plain` drops from `6.76 GiB/s` (parser-only) to `225.97 MiB/s` because `ImplicitLinkStore` scans all rows per `advance` and extracts URLs whenever `:` appears.
2. **`B2` (60 separate `Shape::Text` re-tessellation in `renderer.rs`)**: `show_only_idle` (`4.97 µs`) vs `show_and_tessellate_idle` (`145.12 µs`) shows **`96.6%` of idle frame CPU time** is spent in `epaint::Tessellator` re-copying and translating 60 separate `Shape::Text` galleys every frame.
3. **`B3` (`BgCacheKey` full-grid invalidation on `damage_epoch` / `block_cursor`)**: `cursor_blink_toggle` adds **`+34.9 µs`** over `idle` (`179.13 µs` vs `144.26 µs`), and `1_dirty_row` (`232.70 µs`) rebuilds all `12,000` background cells because `BgCacheKey` includes `damage_epoch` and `block_cursor`.
4. **`B4` (`reflow.rs` `Vec<Cell>` allocs & `O(L × Z)` anchor scan)**: Plain `10k` narrow reflow takes `6.38 ms`, whereas `reflow_10k_with_2500_osc133_zones` jumps to **`141.61 ms`** (`22.2×` slower) due to scanning shell zone anchors linearly per logical line (`O(L × Z)`) and allocating `cur_cells = vec![Cell::BLANK; new_cols]` per output row.
5. **`B5` (`ShellIntegrationState::sync_after_eviction` rebuild)**: Under saturated `10k` scrollback, `terminal_saturated_10k/osc133_dense` drops to **`77.68 MiB/s`** (`3.52 ms` for 4,096 lines vs `1.11 ms` plain ASCII) due to `O(zones)` eviction rebuilds and `Vec` shifts.
6. **`B6` (`context_menu` per-frame `selection_text()`)**: `active_selection_1k_lines` adds **`+86.6 µs`** over `idle` (`230.84 µs` vs `144.26 µs`) from allocating and extracting `selection_text()` (`~212 KB`) on every idle frame while a selection exists.
7. **`B7` (Parser UTF-8, `OscString`, `DcsPassthrough`/`ApcString`, and CSI fast-paths)**: Pure ASCII `parser_only` reaches `6.76 GiB/s`, while `utf8_mixed` (`320.83 MiB/s`), `truecolor_sgr` (`310.84 MiB/s`), and `osc133_dense` (`914.45 MiB/s`) drop sharply due to byte-wise UTF-8 decode, byte-wise CSI parameter steps, and missing `memchr` skipping in `OscString`.
8. **`B8` (`build_ascii_row_galley` `LayoutJob` / `LayoutSection` overhead)**: TrueColor dirty rows construct `job.text` and up to `200` `LayoutSection`s with `FontId` clones per row (`12,000` clones on `full_redraw` at `1.9922 ms`), even when `row.visuals.mesh` is directly built.
9. **`B9` (Idle per-frame hashing, `LayoutJob::default()` sentinel, `measure_cell`, `title().to_owned()`, `active_snapshot()`, `events.clone()`)**: Contributes to `show_only_idle` (`4.97 µs`) via unconditional per-frame SipHash over theme/256-color palette, `fonts_mut` mutex locks, and `Vec<&Row>` / `String` allocations.
10. **`B10` (`paint_prompt_gutter` linear scan + unconditional `overlay_mesh` rebuild + curly underline `PathShape`)**: `osc133_2500_zones_idle` adds **`+5.6 µs`** idle overhead from scanning all `ShellZone`s linearly, Stage 3 rebuilds `overlay_mesh` every idle frame, and `vector_glyphs_dashboard` consumes **`8.87 ms`** (`55.45%` of a `16.0 ms` 60fps frame budget) across vector-glyph/curly-underline `PathShape` tessellation and slow-path rows.
11. **`B11` (Native `PtyBackend` + `TerminalRuntime` chunk loop)**: `input_adapters/ring_buffer` (`44.25 GiB/s`) and `input_adapters/channel` (`45.68 GiB/s`) in Section 2.6 measure **in-memory** `RingBufferInput`/`ChannelInput` adapter baselines only; live `PtyBackend` pipe allocation/syscall overhead (`reader_loop` `to_vec()` per 16 KiB read, per-frame `child.try_wait()` in `poll_read`, synchronous `write_bytes` on the UI thread) plus `TerminalRuntime` per-chunk `advance_with_time` hook invocation are evaluated and optimized in M3 and M5.
12. **`B12` (`mem::take(&mut self.parser)`, `print_ascii_run` `row_has_live_eol` fallback, `osc_dispatch` `ColorScheme` clone)**: Splitting `512 KiB` into `16 KiB` chunks (`chunked_16k_advance/ascii_plain` at `2.4361 ms`) adds **`+10.1%`** over 1-shot `terminal/ascii_plain` (`2.2128 ms`), compounded by per-`advance` `mem::take` and `osc_dispatch` scheme clones on `OSC 8`/`133` streams.
13. **`B13` (Slow-path `LayoutJob` row fallback + `ClusterWidthKey` allocation/hashing)**: A single non-ASCII char forces the entire row onto `fonts.layout_job` (`utf8_wide_full_redraw` `926.20 µs`, `utf8_wide_1_dirty_row` `176.10 µs`) and allocates `ClusterWidthKey` strings even on cache hits.
14. **`B14` (`Row::trimmed_len()` backward scan)**: `Row::trimmed_len()` scans backward across `cells` on every call from the renderer, `ImplicitLinkStore`, `ShellIntegrationState`, and `reflow` (contributing to `reflow_10k_scrollback` and `terminal_saturated_10k` overhead).
15. **`B15` (`enter_sync_mode()` `LatchedFrame` snapshot clone & sort)**: `sync_output_dec2026_200x60/truecolor_combining_frames` takes **`3.7410 ms`** (`227.06 MiB/s`) due to cloning all visible rows and sorting referenced `CellExtra`s on `DEC ?2026`.
16. **`B16` (`format!(...).into_bytes()` mouse/key encoding + `arboard::Clipboard::new()` per copy/paste)**: Heap-allocating byte strings on every key/mouse event and recreating `arboard::Clipboard` per clipboard operation live in the interactive input/clipboard path (outside static VT/render Criterion streams) and are eliminated in M2/M5.
17. **`B17` (`CellExtra::combining` `Vec<char>` + per-cell `CellExtra` allocation)**: `osc8_and_graphemes/512k_distinct_links` drops to **`13.19 MiB/s` (`37.92 ms`)** and `terminal/utf8_mixed` drops to `101.35 MiB/s` (`4.93 ms`) due to per-grapheme `Vec<char>` and `CellExtraTable` churn across combining marks, `SGR 58:2` underline colors, and `OSC 8` links.
18. **`B18` (Per-row `Vec<Cell>` heap allocations)**: Each `Row` owns an independent `Vec<Cell>` (`10,000+` separate heap allocations at saturated 10k scrollback), reducing spatial cache locality and compounding allocation churn in `reflow_10k_scrollback`, `terminal_saturated_10k`, and scrollback eviction.

---

## 5. Verbatim Criterion Output

<details>
<summary>vt_throughput (cargo bench -p egui_xterm_core --all-features --bench vt_throughput)</summary>

```text
Benchmarking vt_stream_throughput/parser_only/ascii_plain: Analyzing
vt_stream_throughput/parser_only/ascii_plain
                        time:   [71.769 µs 72.237 µs 72.781 µs]
                        thrpt:  [6.7092 GiB/s 6.7598 GiB/s 6.8039 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.1s, enable flat sampling, or reduce sample count to 50.
Benchmarking vt_stream_throughput/parser_only/utf8_mixed: Analyzing
vt_stream_throughput/parser_only/utf8_mixed
                        time:   [1.5145 ms 1.5585 ms 1.5999 ms]
                        thrpt:  [312.53 MiB/s 320.83 MiB/s 330.15 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.7s, enable flat sampling, or reduce sample count to 50.
Benchmarking vt_stream_throughput/parser_only/truecolor_sgr: Analyzing
vt_stream_throughput/parser_only/truecolor_sgr
                        time:   [1.5980 ms 1.6088 ms 1.6216 ms]
                        thrpt:  [308.39 MiB/s 310.84 MiB/s 312.93 MiB/s]

Benchmarking vt_stream_throughput/parser_only/osc133_dense: Analyzing
vt_stream_throughput/parser_only/osc133_dense
                        time:   [354.63 µs 365.16 µs 378.39 µs]
                        thrpt:  [882.46 MiB/s 914.45 MiB/s 941.61 MiB/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe

Benchmarking vt_stream_throughput/terminal/ascii_plain: Analyzing
vt_stream_throughput/terminal/ascii_plain
                        time:   [2.2026 ms 2.2128 ms 2.2239 ms]
                        thrpt:  [224.85 MiB/s 225.97 MiB/s 227.02 MiB/s]
Found 14 outliers among 100 measurements (14.00%)
  5 (5.00%) high mild
  9 (9.00%) high severe

Benchmarking vt_stream_throughput/terminal/utf8_mixed: Analyzing
vt_stream_throughput/terminal/utf8_mixed
                        time:   [4.9204 ms 4.9337 ms 4.9472 ms]
                        thrpt:  [101.07 MiB/s 101.35 MiB/s 101.62 MiB/s]

Benchmarking vt_stream_throughput/terminal/truecolor_sgr: Analyzing
vt_stream_throughput/terminal/truecolor_sgr
                        time:   [2.5811 ms 2.5869 ms 2.5926 ms]
                        thrpt:  [192.88 MiB/s 193.31 MiB/s 193.74 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking vt_stream_throughput/terminal/osc133_dense: Analyzing
vt_stream_throughput/terminal/osc133_dense
                        time:   [2.4134 ms 2.4175 ms 2.4218 ms]
                        thrpt:  [137.88 MiB/s 138.12 MiB/s 138.36 MiB/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking terminal_saturated_10k/ascii_with_colons: Analyzing
terminal_saturated_10k/ascii_with_colons
                        time:   [1.7483 ms 1.7519 ms 1.7552 ms]
                        thrpt:  [173.59 MiB/s 173.92 MiB/s 174.27 MiB/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe

Benchmarking terminal_saturated_10k/ascii_no_colons: Analyzing
terminal_saturated_10k/ascii_no_colons
                        time:   [1.1115 ms 1.1129 ms 1.1144 ms]
                        thrpt:  [273.42 MiB/s 273.79 MiB/s 274.11 MiB/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe

Benchmarking terminal_saturated_10k/osc133_dense: Analyzing
terminal_saturated_10k/osc133_dense
                        time:   [3.5116 ms 3.5189 ms 3.5266 ms]
                        thrpt:  [77.513 MiB/s 77.682 MiB/s 77.844 MiB/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe

Benchmarking terminal_saturated_10k/utf8_mixed: Analyzing
terminal_saturated_10k/utf8_mixed
                        time:   [3.1244 ms 3.1418 ms 3.1597 ms]
                        thrpt:  [86.230 MiB/s 86.721 MiB/s 87.204 MiB/s]

Benchmarking chunked_16k_advance/ascii_plain: Analyzing
chunked_16k_advance/ascii_plain
                        time:   [2.4313 ms 2.4361 ms 2.4421 ms]
                        thrpt:  [204.75 MiB/s 205.26 MiB/s 205.66 MiB/s]
Found 3 outliers among 50 measurements (6.00%)
  1 (2.00%) high mild
  2 (4.00%) high severe

Benchmarking chunked_16k_advance/truecolor_sgr: Analyzing
chunked_16k_advance/truecolor_sgr
                        time:   [2.7455 ms 2.7506 ms 2.7567 ms]
                        thrpt:  [181.40 MiB/s 181.81 MiB/s 182.14 MiB/s]
Found 4 outliers among 50 measurements (8.00%)
  2 (4.00%) high mild
  2 (4.00%) high severe

Benchmarking chunked_16k_advance/osc133_dense: Analyzing
chunked_16k_advance/osc133_dense
                        time:   [2.4540 ms 2.4590 ms 2.4643 ms]
                        thrpt:  [135.50 MiB/s 135.80 MiB/s 136.07 MiB/s]
Found 2 outliers among 50 measurements (4.00%)
  2 (4.00%) high severe

Warning: Unable to complete 50 samples in 4.0s. You may wish to increase target time to 4.9s, enable flat sampling, or reduce sample count to 30.
Benchmarking sync_output_dec2026_200x60/truecolor_combining_frames: Analyzing
sync_output_dec2026_200x60/truecolor_combining_frames
                        time:   [3.7272 ms 3.7410 ms 3.7551 ms]
                        thrpt:  [226.20 MiB/s 227.06 MiB/s 227.90 MiB/s]

Benchmarking osc8_and_graphemes/512k_distinct_links: Analyzing
osc8_and_graphemes/512k_distinct_links
                        time:   [37.839 ms 37.915 ms 38.002 ms]
                        thrpt:  [13.157 MiB/s 13.187 MiB/s 13.214 MiB/s]
Found 1 outliers among 50 measurements (2.00%)
  1 (2.00%) high severe

Benchmarking reflow_10k_scrollback/narrow_120x40_to_80x40: Analyzing
reflow_10k_scrollback/narrow_120x40_to_80x40
                        time:   [6.3153 ms 6.3791 ms 6.4754 ms]
                        thrpt:  [1.5443 Melem/s 1.5676 Melem/s 1.5835 Melem/s]

Benchmarking reflow_10k_scrollback/widen_80x40_to_160x40: Analyzing
reflow_10k_scrollback/widen_80x40_to_160x40
                        time:   [3.3846 ms 3.4123 ms 3.4418 ms]
                        thrpt:  [2.9055 Melem/s 2.9306 Melem/s 2.9546 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.1s or enable flat sampling.
Benchmarking reflow_10k_with_2500_osc133_zones/narrow_120x40_to_80x40: Analyzing
reflow_10k_with_2500_osc133_zones/narrow_120x40_to_80x40
                        time:   [141.49 ms 141.61 ms 141.72 ms]
                        thrpt:  [70.562 Kelem/s 70.616 Kelem/s 70.679 Kelem/s]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild

Benchmarking input_adapters/ring_buffer: Analyzing
input_adapters/ring_buffer
                        time:   [11.018 µs 11.034 µs 11.050 µs]
                        thrpt:  [44.187 GiB/s 44.251 GiB/s 44.316 GiB/s]
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low severe
  2 (2.00%) low mild
  4 (4.00%) high mild
  5 (5.00%) high severe

Benchmarking input_adapters/channel: Analyzing
input_adapters/channel  time:   [10.663 µs 10.690 µs 10.715 µs]
                        thrpt:  [45.570 GiB/s 45.678 GiB/s 45.790 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) low mild
```

</details>

<details>
<summary>frame_render (cargo bench -p egui_xterm --all-features --bench frame_render)</summary>

```text
Benchmarking frame_render_stages/show_only_idle: Analyzing
frame_render_stages/show_only_idle
                        time:   [4.9637 µs 4.9716 µs 4.9797 µs]
                        thrpt:  [2.4098 Gelem/s 2.4137 Gelem/s 2.4176 Gelem/s]
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) high mild
  1 (2.00%) high severe

Benchmarking frame_render_stages/show_and_tessellate_idle: Analyzing
frame_render_stages/show_and_tessellate_idle
                        time:   [144.68 µs 145.12 µs 145.70 µs]
                        thrpt:  [82.360 Melem/s 82.692 Melem/s 82.943 Melem/s]
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) high mild
  1 (2.00%) high severe

Benchmarking frame_render_stages/show_only_full_redraw: Analyzing
frame_render_stages/show_only_full_redraw
                        time:   [1.8716 ms 1.8777 ms 1.8840 ms]
                        thrpt:  [6.3694 Melem/s 6.3910 Melem/s 6.4117 Melem/s]
Found 3 outliers among 50 measurements (6.00%)
  3 (6.00%) high mild

Benchmarking frame_render_stages/show_and_tessellate_full_redraw: Analyzing
frame_render_stages/show_and_tessellate_full_redraw
                        time:   [2.0022 ms 2.0072 ms 2.0121 ms]
                        thrpt:  [5.9638 Melem/s 5.9786 Melem/s 5.9933 Melem/s]
Found 1 outliers among 50 measurements (2.00%)
  1 (2.00%) high mild

Benchmarking frame_render_200x60_truecolor/idle: Analyzing
frame_render_200x60_truecolor/idle
                        time:   [143.78 µs 144.26 µs 144.83 µs]
                        thrpt:  [82.854 Melem/s 83.185 Melem/s 83.461 Melem/s]
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) high mild
  1 (2.00%) high severe

Benchmarking frame_render_200x60_truecolor/1_dirty_row: Analyzing
frame_render_200x60_truecolor/1_dirty_row
                        time:   [232.50 µs 232.70 µs 232.94 µs]
                        thrpt:  [51.514 Melem/s 51.568 Melem/s 51.613 Melem/s]
Found 4 outliers among 50 measurements (8.00%)
  2 (4.00%) low mild
  2 (4.00%) high severe

Benchmarking frame_render_200x60_truecolor/scroll_1_line: Analyzing
frame_render_200x60_truecolor/scroll_1_line
                        time:   [256.22 µs 256.91 µs 257.50 µs]
                        thrpt:  [46.602 Melem/s 46.709 Melem/s 46.835 Melem/s]

Benchmarking frame_render_200x60_truecolor/full_redraw: Analyzing
frame_render_200x60_truecolor/full_redraw
                        time:   [1.9874 ms 1.9922 ms 1.9974 ms]
                        thrpt:  [6.0077 Melem/s 6.0236 Melem/s 6.0381 Melem/s]
Found 1 outliers among 50 measurements (2.00%)
  1 (2.00%) high mild

Benchmarking frame_render_200x60_truecolor/cursor_blink_toggle: Analyzing
frame_render_200x60_truecolor/cursor_blink_toggle
                        time:   [178.97 µs 179.13 µs 179.29 µs]
                        thrpt:  [66.930 Melem/s 66.992 Melem/s 67.050 Melem/s]
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) high mild
  1 (2.00%) high severe

Benchmarking frame_render_200x60_truecolor/active_selection_1k_lines: Analyzing
frame_render_200x60_truecolor/active_selection_1k_lines
                        time:   [229.84 µs 230.84 µs 232.05 µs]
                        thrpt:  [51.714 Melem/s 51.984 Melem/s 52.209 Melem/s]
Found 3 outliers among 50 measurements (6.00%)
  1 (2.00%) low mild
  2 (4.00%) high mild

Benchmarking frame_render_200x60_truecolor/osc133_2500_zones_idle: Analyzing
frame_render_200x60_truecolor/osc133_2500_zones_idle
                        time:   [149.26 µs 149.86 µs 150.70 µs]
                        thrpt:  [79.629 Melem/s 80.074 Melem/s 80.398 Melem/s]

Benchmarking frame_render_200x60_truecolor/scrollback_scrub_10k: Analyzing
frame_render_200x60_truecolor/scrollback_scrub_10k
                        time:   [115.84 µs 116.00 µs 116.19 µs]
                        thrpt:  [103.28 Melem/s 103.45 Melem/s 103.59 Melem/s]
Found 7 outliers among 50 measurements (14.00%)
  5 (10.00%) high mild
  2 (4.00%) high severe

Benchmarking frame_render_200x60_utf8/utf8_wide_full_redraw: Analyzing
frame_render_200x60_utf8/utf8_wide_full_redraw
                        time:   [921.18 µs 926.20 µs 934.17 µs]
                        thrpt:  [12.846 Melem/s 12.956 Melem/s 13.027 Melem/s]

Benchmarking frame_render_200x60_utf8/utf8_wide_1_dirty_row: Analyzing
frame_render_200x60_utf8/utf8_wide_1_dirty_row
                        time:   [175.04 µs 176.10 µs 177.71 µs]
                        thrpt:  [67.525 Melem/s 68.144 Melem/s 68.554 Melem/s]

Benchmarking frame_render_200x60_tui/vector_glyphs_dashboard: Analyzing
frame_render_200x60_tui/vector_glyphs_dashboard
                        time:   [8.4188 ms 8.8714 ms 9.3805 ms]
                        thrpt:  [1.2792 Melem/s 1.3527 Melem/s 1.4254 Melem/s]
```

</details>
