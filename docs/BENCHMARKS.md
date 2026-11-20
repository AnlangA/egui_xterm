# `egui_xterm` Release Benchmark Report

> **Profile**: Cargo `bench` (`--release` `[optimized]`, `RUSTFLAGS` unset, executed sequentially)  
> **Recorded**: `frame_render` recorded `2026-11-20T16:02:23Z`; `vt_throughput` recorded `2026-11-20T16:04:18Z` (post-M6 Phase 2 hardening)  
> **Workspace**: `E:\workspace\egui_xterm`

---

## 1. Environment Metadata

| Field | Value |
|---|---|
| **rustc** | `rustc 1.99.0-nightly (7608eb7b0 2026-08-05)` |
| **cargo** | `cargo 1.99.0-nightly (7c83d4cc0 2026-07-29)` |
| **OS** | `Microsoft Windows 11 专业版` (`Version 10.0.26200`, `BuildNumber 26200`, `64 位`) |
| **CPU** | `Intel(R) Core(TM) Ultra 9 285K` (`24` cores / `24` logical processors, `3700` MHz) |
| **Criterion** | `0.8` (`Gnuplot not found, using plotters backend`) |

---

## 2. Benchmark Inventory & Executed Commands

1. `crates/egui_xterm/Cargo.toml`:
   - **Command**: `cargo bench -p egui_xterm --bench frame_render`
   - **Inspected `[[bench]]` target**: `name = "frame_render"`, `harness = false` (`crates/egui_xterm/benches/frame_render.rs`).
   - **Workload**: Single-frame `egui::Context::run_ui` + `TerminalRuntime::show` (input polling, VT parsing, auto-layout, 4-stage `Mesh`/`RowRenderCache` rendering) + `egui::Context::tessellate` (`epaint::Tessellator`) for a `200 x 60` (`12,000` cells) TrueColor terminal at `native_pixels_per_point = 1.0`, `ScaleMode::ReflowGrid`, `resize_debounce_secs = 0.0`, `cursor_blink = false`.
   - **Observed result**: Exit `0`; all 4 benchmarks (`1.4623 ms`–`3.3178 ms`) well below the `16.0 ms` (60 fps) frame budget.

2. `crates/egui_xterm_core/Cargo.toml`:
   - **Command**: `cargo bench -p egui_xterm_core --bench vt_throughput` (default features: `search`, `hyperlinks`, `shell-integration`, `kitty-keyboard`)
   - **Inspected `[[bench]]` target**: `name = "vt_throughput"`, `harness = false` (`crates/egui_xterm_core/benches/vt_throughput.rs`).
   - **Workload**:
     - `vt_stream_throughput`: `Throughput::Bytes` benchmarks comparing zero-alloc pure `Parser::advance` (`CountingHandler` overriding `print_ascii_run`) vs. full `120x40` `Terminal::advance` (`CoreConfig::default()`, `10,000`-line scrollback) across 4 payloads:
       - `ascii_plain` (`524,316` bytes structured ASCII log lines)
       - `utf8_mixed` (`524,308` bytes mixing ASCII, CJK, emoji, VS16, ZWJ, and combining accents)
       - `truecolor_sgr` (`524,367` bytes alternating `;` and `:` 24-bit TrueColor `38`/`48` + `4:3` curly underline)
       - `osc133_dense` (`350,138` bytes containing `2,500` `OSC 133` A/B/C/D commands = `7,500` semantic zones, `BEL` & `ST`)
     - `reflow_10k_scrollback`: `Throughput::Elements(10_000)` (`sample_size = 20`, `BatchSize::PerIteration`) measuring lossless resize reflow of a saturated `10,000`-line scrollback grid (`narrow_120x40_to_80x40` and `widen_80x40_to_160x40`).
   - **Observed result**: Exit `0` — all 10 Criterion benchmarks completed (`7.0640 GiB/s` pure SWAR ASCII parsing; `80.383`–`178.70 MiB/s` full terminal ingestion; `12.720 ms` narrow / `6.1975 ms` widen 10k-line reflow).

---

## 3. Criterion Results (`frame_render_200x60_truecolor`, recorded `2026-11-20T16:02:23Z`)

Target frame budget for smooth 60 fps rendering: **`< 16.0000 ms`**.

| Benchmark (`200x60` TrueColor, `12,000` cells) | Time Low | Time Estimate | Time High | Throughput (`Melem/s`) | Outliers (100 samples) | `< 16.0 ms` Budget |
|---|---|---|---|---|---|---|
| `frame_render_200x60_truecolor/idle` | `1.4416 ms` | **`1.4623 ms`** | `1.4852 ms` | `[8.0798, 8.2061, 8.3242]` | `4 / 100` (`4.00%`: 3 high mild, 1 high severe) | **PASS** (`10.94x` headroom) |
| `frame_render_200x60_truecolor/1_dirty_row` | `1.4521 ms` | **`1.4645 ms`** | `1.4779 ms` | `[8.1195, 8.1942, 8.2640]` | `5 / 100` (`5.00%`: 5 high mild) | **PASS** (`10.93x` headroom) |
| `frame_render_200x60_truecolor/scroll_1_line` | `1.6966 ms` | **`1.7502 ms`** | `1.8054 ms` | `[6.6469, 6.8564, 7.0730]` | `18 / 100` (`18.00%`: 6 low severe, 2 low mild, 3 high mild, 7 high severe) | **PASS** (`9.14x` headroom) |
| `frame_render_200x60_truecolor/full_redraw` | `3.2572 ms` | **`3.3178 ms`** | `3.3825 ms` | `[3.5477, 3.6168, 3.6842]` | `1 / 100` (`1.00%`: 1 high mild) | **PASS** (`4.82x` headroom) |

### Raw Criterion Output (`cargo bench -p egui_xterm --bench frame_render`)

```text
   Compiling egui_xterm_core v0.1.0 (E:\workspace\egui_xterm\crates\egui_xterm_core)
   Compiling egui_xterm v0.1.0 (E:\workspace\egui_xterm\crates\egui_xterm)
    Finished `bench` profile [optimized] target(s) in 3.75s
     Running benches\frame_render.rs (target\release\build\egui_xterm/d121981c6f3385fe\out\frame_render-d121981c6f3385fe.exe)
Gnuplot not found, using plotters backend
Benchmarking frame_render_200x60_truecolor/idle
Benchmarking frame_render_200x60_truecolor/idle: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.4s, enable flat sampling, or reduce sample count to 50.
Benchmarking frame_render_200x60_truecolor/idle: Collecting 100 samples in estimated 7.4457 s (5050 iterations)
Benchmarking frame_render_200x60_truecolor/idle: Analyzing
frame_render_200x60_truecolor/idle
                        time:   [1.4416 ms 1.4623 ms 1.4852 ms]
                        thrpt:  [8.0798 Melem/s 8.2061 Melem/s 8.3242 Melem/s]
                 change:
                        time:   [+2.6550% +4.1423% +5.6255%] (p = 0.00 < 0.05)
                        thrpt:  [−5.3259% −3.9776% −2.5863%]
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking frame_render_200x60_truecolor/1_dirty_row
Benchmarking frame_render_200x60_truecolor/1_dirty_row: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.6s, enable flat sampling, or reduce sample count to 50.
Benchmarking frame_render_200x60_truecolor/1_dirty_row: Collecting 100 samples in estimated 7.6234 s (5050 iterations)
Benchmarking frame_render_200x60_truecolor/1_dirty_row: Analyzing
frame_render_200x60_truecolor/1_dirty_row
                        time:   [1.4521 ms 1.4645 ms 1.4779 ms]
                        thrpt:  [8.1195 Melem/s 8.1942 Melem/s 8.2640 Melem/s]
                 change:
                        time:   [+1.6184% +3.0209% +4.3502%] (p = 0.00 < 0.05)
                        thrpt:  [−4.1688% −2.9323% −1.5926%]
                        Performance has regressed.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
Benchmarking frame_render_200x60_truecolor/scroll_1_line
Benchmarking frame_render_200x60_truecolor/scroll_1_line: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.6s, enable flat sampling, or reduce sample count to 50.
Benchmarking frame_render_200x60_truecolor/scroll_1_line: Collecting 100 samples in estimated 7.6234 s (5050 iterations)
Benchmarking frame_render_200x60_truecolor/scroll_1_line: Analyzing
frame_render_200x60_truecolor/scroll_1_line
                        time:   [1.6966 ms 1.7502 ms 1.8054 ms]
                        thrpt:  [6.6469 Melem/s 6.8564 Melem/s 7.0730 Melem/s]
                 change:
                        time:   [−2.3052% −0.3798% +1.7260%] (p = 0.72 > 0.05)
                        thrpt:  [−1.6968% +0.3813% +2.3596%]
                        No change in performance detected.
Found 18 outliers among 100 measurements (18.00%)
  6 (6.00%) low severe
  2 (2.00%) low mild
  3 (3.00%) high mild
  7 (7.00%) high severe
Benchmarking frame_render_200x60_truecolor/full_redraw
Benchmarking frame_render_200x60_truecolor/full_redraw: Warming up for 3.0000 s
Benchmarking frame_render_200x60_truecolor/full_redraw: Collecting 100 samples in estimated 5.2237 s (1700 iterations)
Benchmarking frame_render_200x60_truecolor/full_redraw: Analyzing
frame_render_200x60_truecolor/full_redraw
                        time:   [3.2572 ms 3.3178 ms 3.3825 ms]
                        thrpt:  [3.5477 Melem/s 3.6168 Melem/s 3.6842 Melem/s]
                 change:
                        time:   [+8.8269% +11.019% +13.544%] (p = 0.00 < 0.05)
                        thrpt:  [−11.929% −9.9254% −8.1109%]
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
```

---

## 4. Criterion Results (`vt_throughput`, recorded `2026-11-20T16:04:18Z`)

| Benchmark | Input Size / Scope | Time Low | Time Estimate | Time High | Throughput | Outliers |
|---|---|---|---|---|---|---|
| `vt_stream_throughput/parser_only/ascii_plain` | `524,316` B | `68.939 µs` | **`69.126 µs`** | `69.329 µs` | `[7.0433, 7.0640, 7.0832] GiB/s` | `6 / 100` (`6.00%`: 2 low mild, 2 high mild, 2 high severe) |
| `vt_stream_throughput/parser_only/utf8_mixed` | `524,308` B | `1.0454 ms` | **`1.0482 ms`** | `1.0511 ms` | `[475.73, 477.05, 478.31] MiB/s` | `2 / 100` (`2.00%`: 1 high mild, 1 high severe) |
| `vt_stream_throughput/parser_only/truecolor_sgr` | `524,367` B | `1.2350 ms` | **`1.2384 ms`** | `1.2421 ms` | `[402.60, 403.79, 404.92] MiB/s` | `1 / 100` (`1.00%`: 1 high mild) |
| `vt_stream_throughput/parser_only/osc133_dense` | `350,138` B | `335.03 µs` | **`335.49 µs`** | `336.01 µs` | `[993.77, 995.32, 996.69] MiB/s` | `5 / 100` (`5.00%`: 1 high mild, 4 high severe) |
| `vt_stream_throughput/terminal/ascii_plain` | `524,316` B (`120x40`) | `3.7199 ms` | **`3.7853 ms`** | `3.8521 ms` | `[129.81, 132.10, 134.42] MiB/s` | `0 / 100` (`0.00%`) |
| `vt_stream_throughput/terminal/utf8_mixed` | `524,308` B (`120x40`) | `6.1185 ms` | **`6.2204 ms`** | `6.3253 ms` | `[79.051, 80.383, 81.723] MiB/s` | `1 / 100` (`1.00%`: 1 high mild) |
| `vt_stream_throughput/terminal/truecolor_sgr` | `524,367` B (`120x40`) | `2.7594 ms` | **`2.7985 ms`** | `2.8386 ms` | `[176.17, 178.70, 181.23] MiB/s` | `0 / 100` (`0.00%`) |
| `vt_stream_throughput/terminal/osc133_dense` | `350,138` B (`120x40`) | `3.2740 ms` | **`3.3156 ms`** | `3.3589 ms` | `[99.414, 100.71, 101.99] MiB/s` | `0 / 100` (`0.00%`) |
| `reflow_10k_scrollback/narrow_120x40_to_80x40` | `10,000` scrollback lines | `12.551 ms` | **`12.720 ms`** | `12.886 ms` | `[776.06, 786.18, 796.75] Kelem/s` | `1 / 20` (`5.00%`: 1 high severe) |
| `reflow_10k_scrollback/widen_80x40_to_160x40` | `10,000` scrollback lines | `6.0781 ms` | **`6.1975 ms`** | `6.3708 ms` | `[1.5697, 1.6136, 1.6452] Melem/s` | `2 / 20` (`10.00%`: 1 high mild, 1 high severe) |

### Raw Criterion Output (`cargo bench -p egui_xterm_core --bench vt_throughput`)

```text
   Compiling egui_xterm_core v0.1.0 (E:\workspace\egui_xterm\crates\egui_xterm_core)
    Finished `bench` profile [optimized] target(s) in 3.07s
     Running benches\vt_throughput.rs (target\release\build\egui_xterm_core/044ea6b764ce9237\out\vt_throughput-044ea6b764ce9237.exe)
Gnuplot not found, using plotters backend
Benchmarking vt_stream_throughput/parser_only/ascii_plain
Benchmarking vt_stream_throughput/parser_only/ascii_plain: Warming up for 3.0000 s
Benchmarking vt_stream_throughput/parser_only/ascii_plain: Collecting 100 samples in estimated 5.2364 s (76k iterations)
Benchmarking vt_stream_throughput/parser_only/ascii_plain: Analyzing
vt_stream_throughput/parser_only/ascii_plain
                        time:   [68.939 µs 69.126 µs 69.329 µs]
                        thrpt:  [7.0433 GiB/s 7.0640 GiB/s 7.0832 GiB/s]
                 change:
                        time:   [−13.085% −11.445% −9.8412%] (p = 0.00 < 0.05)
                        thrpt:  [+10.915% +12.924% +15.055%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking vt_stream_throughput/parser_only/utf8_mixed
Benchmarking vt_stream_throughput/parser_only/utf8_mixed: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.3s, enable flat sampling, or reduce sample count to 60.
Benchmarking vt_stream_throughput/parser_only/utf8_mixed: Collecting 100 samples in estimated 5.2931 s (5050 iterations)
Benchmarking vt_stream_throughput/parser_only/utf8_mixed: Analyzing
vt_stream_throughput/parser_only/utf8_mixed
                        time:   [1.0454 ms 1.0482 ms 1.0511 ms]
                        thrpt:  [475.73 MiB/s 477.05 MiB/s 478.31 MiB/s]
                 change:
                        time:   [−16.934% −14.217% −11.417%] (p = 0.00 < 0.05)
                        thrpt:  [+12.889% +16.573% +20.387%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking vt_stream_throughput/parser_only/truecolor_sgr
Benchmarking vt_stream_throughput/parser_only/truecolor_sgr: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.2s, enable flat sampling, or reduce sample count to 60.
Benchmarking vt_stream_throughput/parser_only/truecolor_sgr: Collecting 100 samples in estimated 6.2270 s (5050 iterations)
Benchmarking vt_stream_throughput/parser_only/truecolor_sgr: Analyzing
vt_stream_throughput/parser_only/truecolor_sgr
                        time:   [1.2350 ms 1.2384 ms 1.2421 ms]
                        thrpt:  [402.60 MiB/s 403.79 MiB/s 404.92 MiB/s]
                 change:
                        time:   [−17.920% −13.862% −9.6990%] (p = 0.00 < 0.05)
                        thrpt:  [+10.741% +16.093% +21.832%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking vt_stream_throughput/parser_only/osc133_dense
Benchmarking vt_stream_throughput/parser_only/osc133_dense: Warming up for 3.0000 s
Benchmarking vt_stream_throughput/parser_only/osc133_dense: Collecting 100 samples in estimated 5.0848 s (15k iterations)
Benchmarking vt_stream_throughput/parser_only/osc133_dense: Analyzing
vt_stream_throughput/parser_only/osc133_dense
                        time:   [335.03 µs 335.49 µs 336.01 µs]
                        thrpt:  [993.77 MiB/s 995.32 MiB/s 996.69 MiB/s]
                 change:
                        time:   [−3.6702% −2.7655% −2.0007%] (p = 0.00 < 0.05)
                        thrpt:  [+2.0416% +2.8441% +3.8101%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking vt_stream_throughput/terminal/ascii_plain
Benchmarking vt_stream_throughput/terminal/ascii_plain: Warming up for 3.0000 s
Benchmarking vt_stream_throughput/terminal/ascii_plain: Collecting 100 samples in estimated 5.6325 s (800 iterations)
Benchmarking vt_stream_throughput/terminal/ascii_plain: Analyzing
vt_stream_throughput/terminal/ascii_plain
                        time:   [3.7199 ms 3.7853 ms 3.8521 ms]
                        thrpt:  [129.81 MiB/s 132.10 MiB/s 134.42 MiB/s]
                 change:
                        time:   [−13.958% −11.733% −9.2774%] (p = 0.00 < 0.05)
                        thrpt:  [+10.226% +13.293% +16.223%]
                        Performance has improved.
Benchmarking vt_stream_throughput/terminal/utf8_mixed
Benchmarking vt_stream_throughput/terminal/utf8_mixed: Warming up for 3.0000 s
Benchmarking vt_stream_throughput/terminal/utf8_mixed: Collecting 100 samples in estimated 5.5642 s (800 iterations)
Benchmarking vt_stream_throughput/terminal/utf8_mixed: Analyzing
vt_stream_throughput/terminal/utf8_mixed
                        time:   [6.1185 ms 6.2204 ms 6.3253 ms]
                        thrpt:  [79.051 MiB/s 80.383 MiB/s 81.723 MiB/s]
                 change:
                        time:   [−5.9386% −3.3729% −0.8061%] (p = 0.01 < 0.05)
                        thrpt:  [+0.8126% +3.4906% +6.3135%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking vt_stream_throughput/terminal/truecolor_sgr
Benchmarking vt_stream_throughput/terminal/truecolor_sgr: Warming up for 3.0000 s
Benchmarking vt_stream_throughput/terminal/truecolor_sgr: Collecting 100 samples in estimated 5.1269 s (1500 iterations)
Benchmarking vt_stream_throughput/terminal/truecolor_sgr: Analyzing
vt_stream_throughput/terminal/truecolor_sgr
                        time:   [2.7594 ms 2.7985 ms 2.8386 ms]
                        thrpt:  [176.17 MiB/s 178.70 MiB/s 181.23 MiB/s]
                 change:
                        time:   [−11.609% −8.7856% −5.8568%] (p = 0.00 < 0.05)
                        thrpt:  [+6.2212% +9.6319% +13.134%]
                        Performance has improved.
Benchmarking vt_stream_throughput/terminal/osc133_dense
Benchmarking vt_stream_throughput/terminal/osc133_dense: Warming up for 3.0000 s
Benchmarking vt_stream_throughput/terminal/osc133_dense: Collecting 100 samples in estimated 5.3045 s (1500 iterations)
Benchmarking vt_stream_throughput/terminal/osc133_dense: Analyzing
vt_stream_throughput/terminal/osc133_dense
                        time:   [3.2740 ms 3.3156 ms 3.3589 ms]
                        thrpt:  [99.414 MiB/s 100.71 MiB/s 101.99 MiB/s]
                 change:
                        time:   [−15.987% −13.281% −10.565%] (p = 0.00 < 0.05)
                        thrpt:  [+11.813% +15.315% +19.029%]
                        Performance has improved.

Benchmarking reflow_10k_scrollback/narrow_120x40_to_80x40
Benchmarking reflow_10k_scrollback/narrow_120x40_to_80x40: Warming up for 3.0000 s
Benchmarking reflow_10k_scrollback/narrow_120x40_to_80x40: Collecting 20 samples in estimated 9.1080 s (420 iterations)
Benchmarking reflow_10k_scrollback/narrow_120x40_to_80x40: Analyzing
reflow_10k_scrollback/narrow_120x40_to_80x40
                        time:   [12.551 ms 12.720 ms 12.886 ms]
                        thrpt:  [776.06 Kelem/s 786.18 Kelem/s 796.75 Kelem/s]
                 change:
                        time:   [−17.637% −15.075% −12.423%] (p = 0.00 < 0.05)
                        thrpt:  [+14.186% +17.751% +21.414%]
                        Performance has improved.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
Benchmarking reflow_10k_scrollback/widen_80x40_to_160x40
Benchmarking reflow_10k_scrollback/widen_80x40_to_160x40: Warming up for 3.0000 s
Benchmarking reflow_10k_scrollback/widen_80x40_to_160x40: Collecting 20 samples in estimated 5.8604 s (630 iterations)
Benchmarking reflow_10k_scrollback/widen_80x40_to_160x40: Analyzing
reflow_10k_scrollback/widen_80x40_to_160x40
                        time:   [6.0781 ms 6.1975 ms 6.3708 ms]
                        thrpt:  [1.5697 Melem/s 1.6136 Melem/s 1.6452 Melem/s]
                 change:
                        time:   [−7.3940% −4.3408% −1.3430%] (p = 0.01 < 0.05)
                        thrpt:  [+1.3613% +4.5378% +7.9844%]
                        Performance has improved.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
```

---

## 5. Verification Commands

To reproduce this report from `E:\workspace\egui_xterm` (run sequentially with `RUSTFLAGS` unset):

```powershell
rustc --version
Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
cargo bench -p egui_xterm --bench frame_render
cargo bench -p egui_xterm_core --bench vt_throughput
```
