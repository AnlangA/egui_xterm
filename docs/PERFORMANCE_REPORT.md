# `egui_xterm` End-to-End Performance Engineering Report (Milestones M1–M5)

> **Status**: Ready for Final Criterion Capture (`[PENDING_FINAL_BENCH]` placeholders prepared for `bench-report` after parallel compilation/test workloads finish)  
> **Scope**: Clean-room end-to-end optimization audit (`B1`–`B18`) across `egui_xterm_core`, `egui_xterm_pty`, and `egui_xterm`, plus ready-to-fill before/after Criterion tables covering all **44** workspace benchmarks (`22` `vt_throughput` + `15` `frame_render` + `3` `frame_render_200x60_m5` + `4` `pty_transport`).  
> **Safety & Portability Invariant**: `#![forbid(unsafe_code)]` enforced across all workspace crates; zero unauthorized third-party dependencies; `egui_xterm_core` and default `egui_xterm` compile cleanly for `wasm32-unknown-unknown`.

---

## 1. Executive Summary

`egui_xterm` is an embeddable, clean-room Rust terminal emulator designed to sustain smooth `< 16.0 ms` (`60 fps`) interactive frame budgets inside any `egui` `0.37` container while maintaining high-throughput VT500 / modern terminal stream ingestion and lossless scrollback reflow.

Following Milestone **M1**'s expanded 37-benchmark Criterion baseline capture on `Apple M4 Max` ([`BENCHMARK_BASELINE.md`](./BENCHMARK_BASELINE.md), base commit `387887e`), **18 concrete pipeline bottlenecks (`B1`–`B18`)** were diagnosed and systematically addressed across Milestones **M2** (`d103eb0`), **M3** (`d103eb0`), **M4** (`0be7e62`–`e7d6492`), and **M5** (`a8c1463`–`289266e`):

- **Core VT Ingestion & State (`egui_xterm_core`, M2/M4)**: Split `Parser` and `TerminalState` (removing per-`advance` `mem::take`), added 8-byte SWAR UTF-8 batching (`print_utf8_run`), `memchr` bulk skipping (`OscString`, `DcsPassthrough`, `ApcString`), zero-copy `dispatch_osc_slice`, inline `parse_csi_fast`, dirty-range `ImplicitLinkStore::refresh_dirty` with `"://"` prefiltering, `O(1)` `Row::trimmed_len_cache` (keeping `Row` at 40 bytes on 64-bit), `VecDeque<ShellZone>` with `O(1)` scroll eviction and `live_eol_rows`, `O(Z log Z + L)` reflow with `cell_vec_pool` and `resize_cols_in_place_fast`, `ExtraSlot` capacity retention with bitset hyperlink GC, persistent `LatchedFrame` capacity reuse, zero-alloc `*_into` protocol encoders, and an `O(1)`-amortized row-based `SixelDecoder` canvas.
- **Native PTY Transport (`egui_xterm_pty`, M3)**: Replaced per-chunk `to_vec()` allocations with bounded `RECYCLE_POOL_CAPACITY = 64` / `WRITER_RECYCLE_CAPACITY = 64` buffer channels, decoupled `child.try_wait()` from per-frame `poll_read()`, and moved `write_bytes` onto a dedicated non-blocking background writer thread (`writer_loop`).
- **`egui` Runtime, Layout & Coalesced Mesh Renderer (`egui_xterm`, M5)**: Replaced 60 per-frame `Shape::Text` re-tessellations with a single coalesced `text_mesh: Arc<Mesh>` (`CachedRowSlot` texel-UV storage + 1-row patching), decoupled the block cursor from `BgCacheKey` (`BgLayoutKey` + `GlobalRenderKey` + Stage 4 `Shape::rect_filled`), eliminated `LayoutJob`/`LayoutSection`/`FontId` clones on ASCII rows (`build_ascii_row_mesh` + `AsciiGlyphCache`), replaced `ClusterWidthKey` allocations with `&str`-indexed `cluster_cache`, cached `overlay_mesh: Arc<Mesh>` + `curly_paths`, binary-searched `prompt_gutter` via `ShellZonesView::in_line_range`, switched `attach_context_menu` to `O(1)` `has_non_empty_selection()`, cached `measure_cell_cached` and borrowed `terminal.title()`, replaced per-frame SipHash theme hashing with `theme_epoch` + `palette_epoch`, and coalesced per-frame `poll_read` + `write_bytes` batches in `TerminalRuntime`.

All post-M5 Criterion measurement cells in Section 4 carry **`[PENDING_FINAL_BENCH]`** placeholders so `bench-report` can populate final measurements sequentially once the host CPU is idle.

---

## 2. End-to-End Data Pipeline Architecture

Every frame in `TerminalRuntime::show` / `show_with_config` (`crates/egui_xterm/src/runtime.rs`) executes a bounded, zero-blocking pipeline connecting the user-supplied `TerminalInput` stream to `egui`'s GPU tessellator and back to the input sink:

```text
  [Desktop OS PTY: PtyBackend (egui_xterm_pty)]              [WASM / Custom Stream]
   ├── reader_loop (16 KiB pooled Vec<u8>, RECYCLE_POOL=64)    Rc<RefCell<_>> / WebSocket /
   ├── writer_loop (non-blocking channel, WRITER_RECYCLE=64)   ChannelInput / RingBufferInput
   └── waiter_loop / EOF-only try_wait() (0 idle syscalls)             │
                 │                                                     │
                 └─────────────────────────┬───────────────────────────┘
                                           ▼
  1. TerminalInput::poll_read (&mut poll_scratch / poll_staging, up to read_budget_bytes_per_frame)
        │  Coalesces all ready chunks into one slice per frame
        ▼
  2. Parser::advance (&bytes, &mut TerminalState)  [crates/egui_xterm_core/src/parser/machine.rs]
        ├── Ground ASCII: 8-byte SWAR scan_printable_ascii_prefix ──► print_ascii_run()
        ├── Ground UTF-8: SWAR scan_ground_utf8_bytes + from_utf8  ──► print_utf8_run()
        ├── OSC/DCS/APC:  memchr3 skip + dispatch_osc_slice / put_slice / apc_put_slice
        └── CSI:          inline parse_csi_fast (64-slot stack CsiParams)
        │
        ▼
  3. TerminalState & Grid Mutation  [crates/egui_xterm_core/src/{grid,term,features}/]
        ├── Primary/Alt VecDeque<Row> (40B Row, trimmed_len_cache, ROW_POOL_CAP=64, 20B Copy Cell)
        ├── Sparse CellExtraTable (ExtraSlot reuse, COMBINING_RETAIN_CAP=64, bitset hyperlink GC)
        ├── post_advance(): dirty-range ImplicitLinkStore + O(1) ShellIntegrationState sync
        ├── resize(): O(Z log Z + L) lossless reflow (cell_vec_pool + resize_cols_in_place_fast)
        └── DEC ?2026: persistent LatchedFrame capacity reuse (250 ms watchdog)
        │
        ▼
  4. Terminal::active_snapshot() -> RenderSnapshot<'_>  [Zero-alloc VisibleRowsView<'a> borrow]
        │
        ▼
  5. TerminalRenderer (4-Stage Coalesced Pipeline)  [crates/egui_xterm/src/renderer.rs]
        ├── Stage 1: bg_mesh: Arc<Mesh>      (BgLayoutKey + CachedRowSlot BgRun + 1-row patch)
        ├── Stage 2: text_mesh: Arc<Mesh>    (build_ascii_row_mesh + AsciiGlyphCache + &str cluster_cache)
        ├── Stage 3: overlay_mesh: Arc<Mesh> (decorations, vector-glyphs, O(log Z) prompt_gutter, curly PathShapes)
        ├── Images:  prepare_inline_images   (TextureHandle cache by (id, StoredImage::revision), U+10EEEE coalescing)
        └── Stage 4: Batched Shape::Mesh Arc clones + decoupled block/bar/underline cursor + IME + progress/bell
        │
        ▼
  6. input_map (process_frame_events_into -> outgoing_scratch: Vec<u8>)  [crates/egui_xterm/src/input_map.rs]
        ├── Precedence: Kitty Keyboard (encode_kitty_key_into) > modifyOtherKeys > legacy VT
        ├── Mouse:      ?9/?1000/?1002/?1003 + ?1016 SGR-pixels (encode_mouse_event_pixels_into)
        └── Paste/Focus: format_paste_bytes_into (in-place ESC[201~ strip) + DEC ?1004 transitions
        │
        ▼
  7. TerminalInput::write_bytes(&outgoing_scratch) + debounced TerminalInput::resize(TerminalSize)
        (Coalesced to <= 1 write_bytes call/frame; non-fatal errors surfaced via last_io_error / TerminalEvent::IoError)
```

---

## 3. Complete Bottleneck Root-Cause & Optimization Mapping (`B1`–`B18`)

### 3.1 Summary Matrix (`B1`–`B18`)

| ID | Bottleneck Area | Baseline Root Cause (`387887e`) | Implemented Optimization | Primary File(s) & Symbol(s) | Commit(s) | Complexity (Before → After) |
|---|---|---|---|---|---|---|
| **B1** | Implicit URL scanning | `ImplicitLinkStore::refresh` scanned entire 10k scrollback per `advance` and extracted lines on any `:` (`+57.4%` penalty on colon logs) | Dirty-range tracking (`Grid::dirty_abs_start`) + `refresh_dirty` + 3-state `"://"` prefilter (`logical_line_has_scheme_sep`) + `VecDeque<ImplicitLink>` | `crates/egui_xterm_core/src/features/hyperlinks.rs` (`ImplicitLinkStore::refresh_dirty`, `logical_line_has_scheme_sep`, `find_link`); `grid/mod.rs` (`mark_dirty_abs`, `take_dirty_abs_start`) | `d103eb0` | `O(L_total)` full scan → **`O(L_dirty + log U)`**; `0` string extracts on `:` w/o `://` |
| **B2** | Text tessellation | Emitted up to 60 separate `Shape::Text(Arc<Galley>)` shapes re-copied/translated by `epaint::Tessellator` every idle frame (`96.6%` of idle CPU) | Coalesced single `text_mesh: Arc<Mesh>` with per-slot texel-UV vertex cache, atlas re-normalization (`uv_norm`), and in-place 1-row patching | `crates/egui_xterm/src/renderer.rs` (`TerminalRenderer::text_mesh`, `CachedRowSlot`, `TextMeshKey`) | `289266e` | `O(R × C)` tessellator copy/frame → **`O(1)` `Arc::clone` idle**, `O(C_dirty)` 1-row patch |
| **B3** | Background mesh cache | `BgCacheKey` included `damage_epoch` & `block_cursor`, invalidating all 12,000 cells on blink toggle (`+34.9 µs`) or 1 dirty row | Split into `BgLayoutKey` + `GlobalRenderKey` + per-row `CachedRowSlot` (`bg_row_gen`, `bg_sel_range`, `bg_search_sig`); slot rotation on scroll; in-place vertex patch; Stage 4 decoupled cursor | `crates/egui_xterm/src/renderer.rs` (`BgLayoutKey`, `GlobalRenderKey`, `CachedRowSlot`, `write_colored_rect_vertices`) | `289266e` | `O(R × C)` rebuild on blink/1-row → **`O(0)` on blink**, **`O(C)` on 1-row edit** |
| **B4** | Reflow allocs & zones | `O(L × Z)` linear scan over shell zones per logical line (`141.61 ms` = `22.2×` slower) + fresh `Vec<Cell>` per output row | Pre-sorted `TrackedAnchor` index sweep (`anchor_cursor`), `Row::resize_cols_in_place_fast`, `Row::take_cells()` + `cell_vec_pool` recycling via `acquire_cells` | `crates/egui_xterm_core/src/grid/reflow.rs` (`resize_with_reflow`, `acquire_cells`); `grid/row.rs` (`resize_cols_in_place_fast`, `take_cells`) | `d103eb0` | `O(L × Z)` + `O(L_out)` allocs → **`O(Z log Z + L)`** + pooled/in-place reuse |
| **B5** | `OSC 133` sync & eviction | `ShellIntegrationState` rebuilt/shifted `Vec` on eviction (`77.68 MiB/s` saturated 10k `osc133_dense`) | `zones: VecDeque<ShellZone>`, `frozen_prefix_len` skip, `O(evicted)` front pop/clamp, `live_eol_rows: VecDeque<RowEolBucket>`, `advance_live_eol_base` | `crates/egui_xterm_core/src/features/shell_integration.rs` (`ShellIntegrationState`, `RowEolBucket`, `sync_after_eviction`) | `d103eb0` | `O(Z)` shift & scan → **`O(1)` amortized** eviction & `row_has_live_eol` |
| **B6** | Context menu idle copy check | Called `terminal.selection_text()` (`~212 KB` alloc) on every idle frame (`+86.6 µs`) | Added zero-alloc short-circuiting `Terminal::has_non_empty_selection()`; invoked only inside open `response.context_menu` closure | `crates/egui_xterm_core/src/term/selection.rs` (`has_non_empty_text`); `term/mod.rs` (`has_non_empty_selection`); `egui_xterm/src/widgets/context_menu.rs` (`attach_context_menu`) | `d103eb0`, `a8c1463` | `O(sel_cells)` alloc/frame → **`O(1)` closed / `O(1)` zero-alloc short-circuit** |
| **B7** | Parser UTF-8 / string / CSI paths | Byte-by-byte UTF-8 decode, CSI stepping, and `OscString`/`DcsPassthrough`/`ApcString` loops (`310–914 MiB/s` vs `6.76 GiB/s` ASCII) | SWAR `scan_ground_utf8_bytes` + `print_utf8_run`; `memchr3` + `dispatch_osc_slice` / `put_slice` / `apc_put_slice`; inline `parse_csi_fast` | `crates/egui_xterm_core/src/parser/{machine,handler,params}.rs`; `term/mod.rs` (`print_utf8_run`, `put_slice`) | `d103eb0` | `O(N)` per-byte state step → **`O(N/8)` SWAR UTF-8 + `O(N)` SIMD `memchr` + tight CSI loop** |
| **B8** | ASCII `LayoutJob` overhead | `build_ascii_row_galley` built `LayoutJob` + up to 200 `LayoutSection`s with `FontId::clone` per row (`1.99 ms` full redraw) | `build_ascii_row_mesh` emits quads directly from `[Option<AsciiGlyphEntry>; 95]` (`AsciiGlyphCache`) into `slot.text_vertices`/`text_indices` | `crates/egui_xterm/src/renderer.rs` (`AsciiGlyphCache`, `get_or_probe_ascii_entry`, `emit_ascii_entry_quad`, `build_ascii_row_mesh`) | `289266e` | `O(C)` `LayoutSection`/`FontId` allocs → **`O(C)` `O(1)` array lookup, 0 allocs** |
| **B9** | Per-frame idle overhead | Per-frame `hash_theme`, `selection_hash`/`search_hash`, `measure_cell` lock, `title().to_owned()` alloc | `last_theme` equality + `theme_epoch` + `palette_epoch`; `measure_cell_cached` + `AutoScaleCache`; borrowed `terminal.title()` in `WidgetInfo::labeled`; per-row sel/search sigs *(see note on retained sentinel & `events.clone()`)* | `crates/egui_xterm/src/layout.rs` (`measure_cell_cached`, `AutoScaleCache`, `layout_and_resize`); `renderer.rs` (`theme_epoch`, `GlobalRenderKey`) | `b85f6c0`, `289266e` | `O(palette + title)`/frame → **`O(1)` eq + `0` allocs idle** |
| **B10** | Gutter, `overlay_mesh` & vector TUI | Linear `paint_prompt_gutter` (`O(Z)`), unconditional `overlay_mesh` rebuild, vector glyphs forcing slow-path `LayoutJob` (`8.87 ms` dashboard) | `ShellZonesView::in_line_range` `O(log Z + K)` binary search; cached Stage 3 `overlay_mesh: Arc<Mesh>` (`OverlayMeshKey`) + per-slot `curly_paths`; inline `is_vec`/`U+10EEEE` skip keeping ASCII rows on `build_ascii_row_mesh` | `crates/egui_xterm_core/src/features/shell_integration.rs` (`in_line_range`); `egui_xterm/src/widgets/prompt_gutter.rs`; `renderer.rs` (`OverlayMeshKey`, `tessellate_slot_overlay`, `is_vector_glyph_char`) | `d103eb0`, `a8c1463`, `289266e` | `O(Z + R×C)`/idle + slow `LayoutJob` → **`O(log Z + K)` + `O(1)` `Arc::clone` idle + ASCII fast path** |
| **B11** | PTY transport & runtime loops | `PtyBackend`: `to_vec()` per 16 KiB read, `try_wait()` inside `poll_read()`, blocking UI-thread `write_bytes`; `TerminalRuntime`: per-chunk `advance` & uncoalesced `write_bytes` | `RECYCLE_POOL_CAPACITY=64` & `WRITER_RECYCLE_CAPACITY=64` pools, EOF-only `poll_child_exit_on_eof`, async `writer_loop` thread; `TerminalRuntime` coalesces `poll_read` into 1 `advance_with_time` and 1 `write_bytes`/frame | `crates/egui_xterm_pty/src/lib.rs` (`PtyBackend`, `reader_loop`, `writer_loop`, `recycle_buf`); `egui_xterm/src/runtime.rs` (`poll_staging`, `outgoing_scratch`, `show_with_config`) | `d103eb0`, `b85f6c0` | Per-chunk allocs + syscalls → **0 steady-state allocs, 0 idle `try_wait`, 1 parse + 1 write/frame** |
| **B12** | `mem::take`, `row_has_live_eol`, OSC dispatch | `std::mem::take(&mut self.parser)` per `advance` (`+10.1%` 16 KiB chunk overhead), coarse `row_has_live_eol` fallback, buffered OSC copy | Split `Terminal { parser, state }`; fine-grained `row_has_live_eol` fallback (only default-styled spaces or wide overlaps); zero-copy `dispatch_osc_slice` + `OSC 133` fast-path + zero `ColorScheme` clones in `OscContext` | `crates/egui_xterm_core/src/term/mod.rs` (`Terminal`, `TerminalState`, `print_ascii_run`, `osc_dispatch`); `parser/machine.rs` (`dispatch_osc_slice`) | `d103eb0` | Per-`advance` parser swap + OSC copy → **`0` swaps, `0` copies on clean OSC** |
| **B13** | Slow-path `LayoutJob` & `ClusterWidthKey` | Any non-ASCII char triggered row-wide `build_slow_row_galley` + `ClusterWidthKey` `String` alloc on every cache hit (`926.20 µs`) | `cluster_cache: HashMap<String, [Option<CachedCluster>; 4]>` queried via `&str` (`scratch_cluster`) storing extracted `ClusterQuads`; ASCII cells in mixed rows still hit `AsciiGlyphCache`; `LayoutJob` limited to cache misses, `OSC 66`, and contextual scripts | `crates/egui_xterm/src/renderer.rs` (`CachedCluster`, `ClusterQuads`, `build_slow_row_mesh`, `emit_cluster_quads`, `flush_contextual_run`) | `289266e` | Per-row `LayoutJob` + `String` alloc/hit → **`0` alloc cache hit, cluster-level caching** |
| **B14** | `Row::trimmed_len()` scan | Reverse `rposition(|c| !c.is_empty())` across up to `cols` on every call from renderer, hyperlinks, shell integration, and reflow | `Row::trimmed_len_cache: u32` (`UNKNOWN_TRIMMED_LEN` sentinel, `Row` stays 40B on 64-bit) updated incrementally on all cell/row/shift/erase/resize mutations | `crates/egui_xterm_core/src/grid/row.rs` (`Row::trimmed_len`, `trimmed_len_cache`, `write_ascii_run`, `write_single_cell`, `clear_range_with_protection`) | `d103eb0` | `O(cols)` backward scan/call → **`O(1)` cached read** (`O(cols)` only on tail erasure) |
| **B15** | `DEC ?2026` `LatchedFrame` allocs | `Option<LatchedFrame>` + `clone_visible_snapshot()` re-allocated `Vec<Row>` and `Vec<(u32, CellExtra)>` on every `enter_sync_mode` (`3.74 ms`) | Inline persistent `LatchedFrame` (`active: bool`) + `Grid::clone_visible_snapshot_into` reusing `visible_rows` and `extras` `Vec` capacities (`palette.clone_from`) | `crates/egui_xterm_core/src/term/mod.rs` (`LatchedFrame`, `enter_sync_mode`, `exit_sync_mode`); `grid/mod.rs` (`clone_visible_snapshot_into`) | `d103eb0` | Fresh outer vecs per `?2026h` → **Reused `visible_rows`/`extras` buffer capacities** |
| **B16** | Input byte encoding & clipboard | `format!(...).into_bytes()` heap allocation per key/mouse/paste event; per-action `arboard::Clipboard::new()` | Zero-alloc `*_into` encoders (`outgoing_scratch: Vec<u8>`, stack `push_u32_dec`, in-place paste ESC strip); *(note: `arboard::Clipboard::new()` remains on-demand per explicit Copy/Paste user action only)* | `crates/egui_xterm_core/src/term/mouse.rs` (`encode_mouse_event_into`, `encode_mouse_event_pixels_into`, `format_paste_bytes_into`); `features/kitty_keyboard.rs` (`encode_kitty_key_into`, `encode_modify_other_keys_into`); `egui_xterm/src/input_map.rs` | `d103eb0`, `a8c1463`, `b85f6c0` | `O(1)` heap alloc/event → **`0` heap allocs across key/mouse/paste encoding** |
| **B17** | `CellExtra` & hyperlink GC churn | `collect_grid_hyperlink_ids` scanned every grid cell into `HashSet<HyperlinkId>`, while freed `CellExtra`s dropped `combining` `Vec<char>` (`37.92 ms` on 512k links) | `CellExtra::clear` retains capacity (`COMBINING_RETAIN_CAP = 64`), `ExtraSlot` reuse via `insert_with`/`assign_extra`, `push_combining_for_cell`, `hyperlink_extra_count` + `collect_live_hyperlink_ids` bitset scan (`gc_unreferenced_bitset`) | `crates/egui_xterm_core/src/cell.rs` (`CellExtra::clear`, `HyperlinkPool::gc_unreferenced_bitset`); `grid/extras.rs` (`CellExtraTable::insert_with`, `collect_live_hyperlink_ids`) | `d103eb0` | Full `O(R_all×C)` `HashSet` GC → **`O(1)` skip when 0 links / `O(slots)` bitset scan + retained `Vec<char>` cap** |
| **B18** | Per-row `Vec<Cell>` allocs | Each `Row` owns `Vec<Cell>`; reflow allocated `vec![Cell::BLANK; new_cols]` per output row (`6.38 ms` narrow); Sixel grew flat buffer | Saturated `scroll_up_region` rotates `front.reset_for_reuse` (`0` allocs); `resize_with_reflow` uses `resize_cols_in_place_fast` + `cell_vec_pool` (`take_cells`/`acquire_cells`); M4 `SixelDecoder` uses row-wise `rows: Vec<Vec<[u8; 4]>>` | `crates/egui_xterm_core/src/grid/{mod,reflow,row}.rs`; `features/inline_images/sixel.rs` (`SixelDecoder::rows`) | `d103eb0`, `0be7e62` | `O(L_out)` allocs on reflow + `O(N²)` Sixel → **Pooled/in-place `Vec<Cell>` reuse + `O(1)` amort. Sixel rows** |

---

### 3.2 Core VT, Grid, Reflow & Memory Optimizations (`B1`, `B4`, `B5`, `B7`, `B12`, `B14`–`B18`)

1. **`B1` — Incremental `ImplicitLinkStore::refresh_dirty` & `"://"` Prefilter (`d103eb0`)**:
   - *Root Cause*: `ImplicitLinkStore::refresh` scanned the entire `0..=end_buf_idx` scrollback buffer (`10,000+` rows) after every `advance()`, and extracted full row strings whenever `:` appeared (making timestamped logs like `[2026-11-20T12:00:00Z ...]` `57.4%` slower than colon-free logs).
   - *Fix*: `Grid` records `dirty_abs_start: Option<AbsLine>` via `mark_dirty_abs()` on every mutation (`write_ascii_slice`, `write_cell`, `scroll_up_region`, `erase_*`, etc.). `TerminalState::post_advance` drains `take_dirty_abs_start()` and invokes `ImplicitLinkStore::refresh_dirty` (`crates/egui_xterm_core/src/features/hyperlinks.rs`), which stores links in `links: VecDeque<ImplicitLink>`, trims evicted front entries (`end_line < oldest`) and overwritten tail entries (`end_line >= abs_start`) in `O(evicted + dirty)`, scans only `[scan_start_buf..=end_buf_idx]`, and runs a zero-alloc 3-state `logical_line_has_scheme_sep` (`':'` → `'/'` → `'/'`) prefilter directly over `Row::cells()[..trimmed_len]` before touching `extract_logical_line_with_spans` or `memchr::memmem`. `find_link` binary-searches the two deque slices via `partition_point`.

2. **`B4` — `O(Z log Z + L)` Reflow & Buffer Pooling (`d103eb0`)**:
   - *Root Cause*: `resize_with_reflow` scanned `anchors.extra_points` (carrying `OSC 133` shell zone boundaries) linearly on every logical line (`O(L × Z)` → `141.61 ms` on 10k lines with 2,500 zones, `22.2×` slower than plain reflow) and freshly allocated `Vec<Cell>` per output row (`6.38 ms`).
   - *Fix*: `Grid::resize_with_reflow` (`crates/egui_xterm_core/src/grid/reflow.rs`) pre-sorts active `TrackedAnchor` indices by `(buf_row, col)` once (`sorted_anchors.sort_unstable_by_key`), advancing a single monotonic `anchor_cursor` so each logical line visits only its `O(k)` active slice. Non-wrapped single rows whose `trimmed_len <= new_cols` mutate in place via `Row::resize_cols_in_place_fast` (`grid/row.rs:796`) without touching `cell_vec_pool`; wrapped rows drain `row_pool` and consumed old rows (`Row::take_cells`) into `cell_vec_pool`, reusing buffers through `Grid::acquire_cells` and returning extras to `self.row_pool`.

3. **`B5` — `ShellIntegrationState` `VecDeque` & Viewport `live_eol_rows` (`d103eb0`)**:
   - *Root Cause*: Saturated `10k` scrollback eviction caused `O(Z)` `Vec` shifts and full-history `end_is_eol` scans (`3.52 ms` / `77.68 MiB/s`).
   - *Fix*: `ShellIntegrationState` (`crates/egui_xterm_core/src/features/shell_integration.rs`) stores `zones: VecDeque<ShellZone>` (`O(1)` `pop_front`), tracks `frozen_prefix_len` so `sync_with_grid` / `sync_after_eviction` skip frozen scrollback zones and inspect only modified suffix rows (`row.generation() > prev_damage_epoch`), stops front-clamping on `z.start_abs.line >= oldest`, and indexes active viewport `end_is_eol` rows in `live_eol_rows: VecDeque<RowEolBucket>` (`BinaryHeap` + `multi_line`) so `row_has_live_eol` is an `O(1)` deque offset check.

4. **`B7` — Batch UTF-8, `memchr` Bulk Skipping & Inline CSI Fast-Path (`d103eb0`)**:
   - *Root Cause*: Non-ASCII UTF-8, CSI parameters, and OSC/DCS/APC string states stepped byte-by-byte through `Parser::step`, lagging ASCII SWAR (`6.76 GiB/s`) by `7×`–`22×`.
   - *Fix* (`crates/egui_xterm_core/src/parser/{machine,handler,params}.rs`):
     - `Ground` uses 8-byte SWAR `word_has_c0_or_del` / `scan_ground_utf8_bytes` to isolate C0/DEL-free UTF-8 spans, validates once with `std::str::from_utf8`, and dispatches `VtHandler::print_utf8_run(&str)`.
     - Under `C1Policy::Utf8Only`, `OscString`, `DcsPassthrough`, `ApcString`, `DcsIgnore`, and `SosPmApcString` skip bulk payloads using `memchr::memchr3` / `memchr::memchr`: single-chunk clean `OscString` dispatches directly via zero-copy `dispatch_osc_slice` (16-slot stack `&[&[u8]; 16]` + `memchr_iter(b';', payload)`), `DcsPassthrough` batches via `VtHandler::put_slice`, and `ApcString` appends via `apc_put_slice`.
     - `Parser::parse_csi_fast` parses optional private markers (`0x3C..=0x3F`), contiguous ASCII digit runs directly into `CsiParams::start_or_resume_param` / `set_active_param`, `;`/`:` delimiters, and final `0x40..=0x7E` dispatch in one loop.

5. **`B12` — Split `Parser`/`TerminalState` & Selective `print_ascii_run` Guard (`d103eb0`)**:
   - *Root Cause*: `Terminal::advance` swapped `std::mem::take(&mut self.parser)` on every call (`+10.1%` overhead on 16 KiB chunks), and `print_ascii_run` dropped to per-char `print_scalar` whenever `row_has_live_eol` was true.
   - *Fix*: `Terminal` holds disjoint `{ parser: Parser, state: TerminalState }` (`term/mod.rs:763-806`), calling `self.parser.advance(bytes, &mut self.state)` with zero swaps. `TerminalState::print_ascii_run` (`term/mod.rs:4335-4361`) only drops to `print_scalar` when `row_has_live_eol` is true *and* `(default_blank_space || wide_overlap)`; all other runs stay on `write_ascii_slice` with one `on_primary_row_mutated` update per row slice. `osc_dispatch` fast-paths `OSC 133` before constructing `OscContext` and borrows `&ColorScheme` / `&mut ColorScheme` with zero scheme clones.

6. **`B14` — 40-Byte `Row` with `trimmed_len_cache` (`d103eb0`)**:
   - *Root Cause*: Every call to `Row::trimmed_len()` scanned backward across `cells`.
   - *Fix*: Added `trimmed_len_cache: u32` (`UNKNOWN_TRIMMED_LEN = u32::MAX`) into existing 64-bit struct padding of `Row` (`size_of::<Row>() == 40`, `grid/row.rs:28-40`), updated in `O(1)` across writes, erases, shifts, fills, reuse, and in-place resizes (rescanning only when erasing right at the previous trimmed tail).

7. **`B15` — Persistent `LatchedFrame` Buffer Reuse (`d103eb0`)**:
   - *Root Cause*: Entering `DEC ?2026` (`enter_sync_mode`) allocated fresh `Vec<Row>` and `Vec<(u32, CellExtra)>` via `clone_visible_snapshot()`.
   - *Fix*: `TerminalState::latched_frame: LatchedFrame` (`term/mod.rs:554-611,2892-2956`) is retained inline across `?2026h/l` cycles; `Grid::clone_visible_snapshot_into` (`grid/mod.rs:1690`) clears and reuses existing `visible_rows` and `extras` capacities (cloning row contents only as required for frozen snapshot isolation).

8. **`B17` — `CellExtra` Capacity Retention & Bitset Hyperlink GC (`d103eb0`)**:
   - *Root Cause*: `HyperlinkPool::gc_unreferenced` scanned every cell of every row into a `HashSet<HyperlinkId>`, while freed `CellExtra` slots discarded `combining: Vec<char>` buffers (`37.92 ms` on `osc8_and_graphemes/512k_distinct_links`).
   - *Fix*: `CellExtra::clear` (`cell.rs:349`) retains `combining` capacity up to `COMBINING_RETAIN_CAP = 64` across `ExtraSlot` reuse (`CellExtraTable::insert_with` / `assign_extra`); `push_combining_for_cell` avoids dirtying hyperlink tracking; `CellExtraTable::collect_live_hyperlink_ids` (`grid/extras.rs:248`) skips in `O(1)` when `hyperlink_extra_count == 0 && !hyperlink_count_dirty`, or scans only occupied `slots` into `TerminalState::hyperlink_gc_bits: Vec<u64>` for `HyperlinkPool::gc_unreferenced_bitset` (`cell.rs:539`).

9. **`B18` — Zero-Alloc Ring Scrolling, Reflow `Vec<Cell>` Pooling & Sixel Row Canvas (`d103eb0`, `0be7e62`)**:
   - *Root Cause*: Each `Row` owns an independent `Vec<Cell>`, causing allocation churn during reflow (`resize_with_reflow`) and dynamic Sixel canvas growth.
   - *Fix*: Steady-state saturated `scroll_up_region` (`grid/mod.rs:851-883`) recycles evicted front rows in place via `pop_front` + `reset_for_reuse` + `push_back` (zero allocations) while preserving `VecDeque` `O(1)` rotation and 40B `Row`. Reflow (`grid/reflow.rs`) reuses `Vec<Cell>` via `resize_cols_in_place_fast` and `cell_vec_pool`. In M4 (`0be7e62`), `SixelDecoder` (`sixel.rs:56,430-491`) replaced flat `O(N²)` canvas reallocs with row-wise `rows: Vec<Vec<[u8; 4]>>` + `hint_width` capacity pre-reservation.

---

### 3.3 Evaluation: Safe SWAR + `memchr` + Zero-Alloc Paths vs. Opt-In `unsafe` SIMD (`B7`)

Per `PROJECT.md` item #6, we evaluated whether introducing an opt-in `unsafe` Cargo feature (e.g., `simd` using `std::arch::{aarch64,x86_64}` NEON/AVX2 intrinsics) was justified over pure-safe SWAR + `memchr` + zero-alloc paths:

1. **Amdahl's Law on `Apple M4 Max` Baseline**:
   - Pure-safe 8-byte SWAR `scan_printable_ascii_prefix` (`bytes.as_chunks::<8>()`) already sustains **`6.7598 GiB/s`** (`72.237 µs` for `512 KiB`) in `vt_stream_throughput/parser_only/ascii_plain`.
   - Full `Terminal::advance` on the exact same payload (`vt_stream_throughput/terminal/ascii_plain`) takes **`2.2128 ms`** (`225.97 MiB/s`).
   - Thus raw ASCII scanning represents **`3.26%`** of end-to-end terminal ingestion time (`72.2 µs / 2,212.8 µs`). Even an infinite-speed SIMD scanner could yield at most `~1.03×` total speedup on ASCII streams.
2. **SIMD Delegation Already Exists Safely via `memchr`**:
   - Bulk delimiter scanning in `OscString`, `DcsPassthrough`, `ApcString`, `DcsIgnore`, `SosPmApcString`, `dispatch_osc_slice`, `ImplicitLinkStore`, and `search` delegates to `memchr 2.8` (`memchr`, `memchr3`, `memmem`), which internally selects NEON/SSE2/AVX2 paths behind a 100% safe Rust API.
3. **Real Bottlenecks Were Algorithmic & Per-Byte Dispatch**:
   - Fixing `B1` (`ImplicitLinkStore` full 10k scan), `B7` (`scan_ground_utf8_bytes` + `parse_csi_fast` + `memchr3`), `B12` (`mem::take` + `row_has_live_eol`), `B14` (`trimmed_len_cache`), and `B17` (`ExtraSlot` + bitset GC) delivers order-of-magnitude gains in safe Rust without weakening `[workspace.lints.rust] unsafe_code = "forbid"` or complicating `wasm32-unknown-unknown` verification.
4. **Conclusion**: **Opt-in `unsafe` SIMD is unnecessary** and remains omitted; all crates retain `#![forbid(unsafe_code)]`.

---

### 3.4 Native PTY Transport & Runtime Coalescing (`B11`)

- **`egui_xterm_pty` (`crates/egui_xterm_pty/src/lib.rs`, `d103eb0`)**:
  - **Reader buffer pool**: `reader_loop` obtains 16 KiB buffers from `acquire_read_buf(&recycle_rx)` (`RECYCLE_POOL_CAPACITY = 64`); `PtyBackend::poll_read` returns drained buffers via `recycle_buf` / `recycle_pending` (eliminating `to_vec()` allocations per 16 KiB read).
  - **Decoupled process wait**: `poll_read()` never calls `child.try_wait()` while the stream is open; `poll_child_exit_on_eof()` runs only on `ReaderEvent::Eof`, `Error`, or channel disconnect (`0` OS process wait syscalls on idle frames).
  - **Non-blocking writer thread**: `PtyBackend::write_bytes` populates a recycled buffer from `writer_recycle_rx` (`WRITER_RECYCLE_CAPACITY = 64`) and calls non-blocking `tx.try_send(WriterCommand::Data(buf))` to `"egui_xterm_pty_writer"` (`writer_loop`), which batches up to `WRITER_CHANNEL_CAPACITY` pending writes before `writer.flush()`, surfacing broken-pipe/I/O errors asynchronously via `WriterSharedState`.
- **`egui_xterm` (`crates/egui_xterm/src/runtime.rs`, `b85f6c0`)**:
  - `RuntimeInner::show_with_config` drains `input.poll_read` into `poll_scratch` / `poll_staging` up to `config.read_budget_bytes_per_frame` and invokes `self.terminal.advance_with_time(chunk, safe_now)` **once per frame**, avoiding redundant `post_advance()` passes.
  - All outgoing VT replies, keyboard/mouse/paste sequences, and `DEC ?1004` focus transitions accumulate into `outgoing_scratch` and flush in **one `input.write_bytes(&self.outgoing_scratch)` call per frame**, surfacing errors via `last_io_error()` / `take_io_error()` and `TerminalEvent::IoError`.

---

### 3.5 `egui` Widget, Layout, Input Encoding & Coalesced Mesh Renderer (`B2`, `B3`, `B6`, `B8`–`B10`, `B13`, `B16`)

1. **`B2` & `B8` — Coalesced `text_mesh: Arc<Mesh>` & Direct `build_ascii_row_mesh` (`289266e`)**:
   - Replaced up to 60 `Shape::Text` galleys (which cost `~140 µs` of `epaint::Tessellator` vertex copy/translation per idle frame) and `build_ascii_row_galley` (`LayoutJob` + `200` `LayoutSection`s + `FontId::clone` per row) with a single `text_mesh: Arc<Mesh>` (`TextureId::default()`) assembled from `CachedRowSlot` row-local texel-UV caches.
   - `build_ascii_row_mesh` probes `AsciiGlyphCache` (`entries`/`bold_entries: [Option<AsciiGlyphEntry>; 95]`) once per glyph and stamps quads directly into `slot.text_vertices`/`text_indices` (handling italic shear and `BoldRenderMode::{FontFamily, VariableWght, FauxBold}` directly). Idle frames submit `Arc::clone(&self.text_mesh)`; 1-row edits with constant vertex/index lengths patch `text_mesh.vertices[slot.text_mesh_vert_range]` in place; font atlas growth (`post_tex_size`) re-normalizes cached texel UVs without re-shaping.

2. **`B3` — Incremental `bg_mesh: Arc<Mesh>` & Decoupled Cursor (`289266e`)**:
   - Removed `damage_epoch` and `block_cursor` from global background invalidation (`BgLayoutKey` + `GlobalRenderKey`). Each `CachedRowSlot` caches `bg_runs: Vec<BgRun>` keyed by `(row.generation(), bg_sel, row_search_sig)`; scroll shifts rotate `self.slots` (`rotate_left`/`rotate_right`); 1-row background changes patch vertices in place via `write_colored_rect_vertices`. Stage 4 draws the block/underline/bar/hollow cursor independently (`Shape::rect_filled`), so cursor blink toggles cause zero `bg_mesh` or `overlay_mesh` rebuilds.

3. **`B6` — Zero-Alloc `context_menu` Selection Check (`d103eb0`, `a8c1463`)**:
   - `attach_context_menu` (`crates/egui_xterm/src/widgets/context_menu.rs:22`) calls `terminal.has_non_empty_selection()` (`term/selection.rs:417`) inside the open `response.context_menu` closure, eliminating per-frame `selection_text()` (`~212 KB`) string allocations.

4. **`B9` — Idle Per-Frame Hashing, Layout & Title Caching (`b85f6c0`, `289266e`)**:
   - Eliminated per-frame `hash_theme` (`SipHash`), `selection_hash`, and `search_hash` in favor of `self.last_theme.as_ref() != Some(&config.theme)` + `theme_epoch: u64` + `snapshot.palette_epoch`.
   - `TerminalLayoutState::measure_cell_cached` (`layout.rs:159`) caches `(raw_w, cell_size)` by `(font_size_bits, ppp_bits)`, and `AutoScaleCache` caches auto-scale binary search results.
   - `layout_and_resize` (`layout.rs:431-434`) borrows `terminal.title()` directly into `WidgetInfo::labeled(Role::Terminal, true, title)` without `to_owned()`.
   - `TerminalRenderer::render_with_bell_style` borrows `terminal.active_snapshot()` once across Stages 1–3. *(Retained guards verified in `HEAD`: `atlas_sentinel_galley` via `fonts.layout_job(LayoutJob::default())` at `renderer.rs:956` to reliably catch `fonts.set_fonts` atlas resets, and `ui.input(|i| i.events.clone())` at `input_map.rs:200` which is empty/`0`-alloc on idle frames.)*

5. **`B10` — `O(log Z + K)` `prompt_gutter`, Cached `overlay_mesh` & ASCII-Eligible Vector Glyphs (`d103eb0`, `a8c1463`, `289266e`)**:
   - `paint_prompt_gutter` (`widgets/prompt_gutter.rs:32`) and `compute_shell_zones_sig` (`renderer.rs:2517`) query `ShellZonesView::in_line_range` (`partition_point` binary search, `O(log Z + K)` instead of `O(Z)`).
   - Stage 3 caches `overlay_mesh: Arc<Mesh>` (`OverlayMeshKey`) + `cached_curly_paths: Vec<PathShape>`, tessellating only dirty rows via `tessellate_slot_overlay`.
   - `is_vector_glyph_char` (`Box Drawing`, `Block Elements`, `Braille`, `Powerline`) and `U+10EEEE` cells are tessellated in Stage 3 / image pass and skipped inline during Stage 2 text scanning (`renderer.rs:1074,1131,1225,3004-3005`), keeping TUI dashboards like `vector_glyphs_dashboard` on `build_ascii_row_mesh` rather than falling back to `LayoutJob`.

6. **`B13` — `&str`-Keyed `cluster_cache` & Extracted Cluster Quads (`289266e`)**:
   - Replaced `HashMap<ClusterWidthKey, ClusterMetrics>` (which allocated a `String` on every lookup) with `cluster_cache: HashMap<String, [Option<CachedCluster>; 4]>` queried via `&str` (`self.scratch_cluster.as_str()`).
   - Non-contextual clusters store `ClusterQuads` (`Empty`/`Single`/`Multi`), emitting quads directly via `emit_cluster_quads` with zero `LayoutJob` construction on cache hits; plain ASCII cells inside mixed rows still hit `AsciiGlyphCache` directly (Case B), leaving `fonts.layout_job` only for cache misses, `OSC 66` scaled text (Case A), and contextual scripts (`ContextualRunScratch`).

7. **`B16` — Zero-Alloc Input Encoders (`d103eb0`, `a8c1463`, `b85f6c0`)**:
   - `encode_mouse_event_into`, `encode_mouse_event_pixels_into`, `format_paste_bytes_into` (`term/mouse.rs`), `encode_kitty_key_into`, `encode_modify_other_keys_into` (`features/kitty_keyboard.rs`), and `encode_egui_key_*_into` (`input_map.rs`) write directly into `InputMapperState::outgoing_scratch: Vec<u8>` using stack integer formatting (`push_u32_dec`) and in-place paste sanitization.
   - *Clipboard boundary*: Under optional `native-clipboard`, `arboard::Clipboard::new()` in `copy_text_everywhere` / `try_read_native_clipboard` (`input_map.rs:1065,1077`) remains intentionally on-demand per user-initiated Copy/Paste action (never called on idle/render frames), avoiding cross-frame OS pasteboard handles and preserving `TerminalRuntime` thread/`wasm32` safety.

8. **Additional M4/M5 Algorithmic Improvements (`41f43a5`, `289266e`)**:
   - **Search Highlight Binary Search + Cursor (`renderer.rs:700-704,779-791,3774-3831`)**: Slices `search_matches` to the visible viewport via `partition_point` (`O(log M)`) and advances a monotonic `row_m_idx` cursor (`search_highlight_row_cursor`, `O(1)` amortized per cell) instead of `O(cells × M)`.
   - **Inline Image `revision` Caching & `U+10EEEE` Coalescing (`renderer.rs:1962-2418`, `store.rs:42`)**: `emit_image_quad` caches `egui::TextureHandle`s by `(image_id, StoredImage::revision)` via `TextureHandle::set` for animations/replacements, and `prepare_kitty_placeholders` merges horizontal `U+10EEEE` tile runs into single clipped `PlaceholderRun` quads.

---

## 4. Before / After Criterion Comparison Tables (`44` Benchmarks)

> **Baseline Source**: [`docs/BENCHMARK_BASELINE.md`](./BENCHMARK_BASELINE.md) (Milestone M1, captured prior to M2–M5 algorithmic & transport optimizations). *(Note: [`docs/BENCHMARKS.md`](./BENCHMARKS.md) holds an earlier Windows 11 285K 14-benchmark run; all 44 comparisons below anchor against the canonical `Apple M4 Max` M1 baseline plus M3/M5 additions.)*

### 4.1 Environment & Run Metadata

| Field | Pre-Optimization Baseline (M1) | Post-M5 Evaluation (`bench-report`) |
|---|---|---|
| **Timestamp (UTC)** | `2026-11-21T01:42:23Z` | `[PENDING_FINAL_BENCH]` |
| **Git Commit** | `387887e4846dbf9c516881f52d690fb407a10693` | `[PENDING_FINAL_BENCH]` |
| **OS** | `macOS 27.2.1` (`BuildVersion: 26B131`, `arm64`) | `[PENDING_FINAL_BENCH]` |
| **CPU** | `Apple M4 Max` (`16` logical CPUs) | `[PENDING_FINAL_BENCH]` |
| **Memory** | `48 GiB` (`51,539,607,552` bytes) | `[PENDING_FINAL_BENCH]` |
| **Compiler (`rustc`)** | `rustc 1.99.0-nightly (375b1431b 2026-07-10)` | `[PENDING_FINAL_BENCH]` |
| **Cargo** | `cargo 1.99.0-nightly (59800466c 2026-07-07)` | `[PENDING_FINAL_BENCH]` |
| **Harness** | `criterion 0.8` (`--bench` / `--release`, `RUSTFLAGS` unset) | `criterion 0.8` (`--bench` / `--release`, `RUSTFLAGS` unset) |

---

### 4.2 Core Pipeline (`vt_throughput`) — `22` Benchmarks

#### 4.2.1 `vt_stream_throughput` (`512 KiB` single-shot streams, `120x40` terminal) — `8` Benchmarks

| Benchmark | Baseline Time `[Low, Est, High]` | Baseline Throughput | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Speedup / Delta | Bottleneck(s) |
|---|---|---|---|---|---|---|
| `vt_stream_throughput/parser_only/ascii_plain` | `[71.769 µs, 72.237 µs, 72.781 µs]` | `[6.7092, 6.7598, 6.8039] GiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | SWAR `print_ascii_run` ceiling |
| `vt_stream_throughput/parser_only/utf8_mixed` | `[1.5145 ms, 1.5585 ms, 1.5999 ms]` | `[312.53, 320.83, 330.15] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B7** |
| `vt_stream_throughput/parser_only/truecolor_sgr` | `[1.5980 ms, 1.6088 ms, 1.6216 ms]` | `[308.39, 310.84, 312.93] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B7** |
| `vt_stream_throughput/parser_only/osc133_dense` | `[354.63 µs, 365.16 µs, 378.39 µs]` | `[882.46, 914.45, 941.61] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B7** |
| `vt_stream_throughput/terminal/ascii_plain` | `[2.2026 ms, 2.2128 ms, 2.2239 ms]` | `[224.85, 225.97, 227.02] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B1**, **B12** |
| `vt_stream_throughput/terminal/utf8_mixed` | `[4.9204 ms, 4.9337 ms, 4.9472 ms]` | `[101.07, 101.35, 101.62] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B7**, **B17** |
| `vt_stream_throughput/terminal/truecolor_sgr` | `[2.5811 ms, 2.5869 ms, 2.5926 ms]` | `[192.88, 193.31, 193.74] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B7**, **B17** |
| `vt_stream_throughput/terminal/osc133_dense` | `[2.4134 ms, 2.4175 ms, 2.4218 ms]` | `[137.88, 138.12, 138.36] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B5**, **B7**, **B12** |

#### 4.2.2 `terminal_saturated_10k` (`10,000` pre-filled scrollback lines + `4,096` lines in `16 KiB` chunks) — `4` Benchmarks

| Benchmark | Baseline Time `[Low, Est, High]` | Baseline Throughput | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Speedup / Delta | Bottleneck(s) |
|---|---|---|---|---|---|---|
| `terminal_saturated_10k/ascii_with_colons` | `[1.7483 ms, 1.7519 ms, 1.7552 ms]` | `[173.59, 173.92, 174.27] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B1**, **B14**, **B18** |
| `terminal_saturated_10k/ascii_no_colons` | `[1.1115 ms, 1.1129 ms, 1.1144 ms]` | `[273.42, 273.79, 274.11] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B1**, **B18** |
| `terminal_saturated_10k/osc133_dense` | `[3.5116 ms, 3.5189 ms, 3.5266 ms]` | `[77.513, 77.682, 77.844] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B5**, **B7** |
| `terminal_saturated_10k/utf8_mixed` | `[3.1244 ms, 3.1418 ms, 3.1597 ms]` | `[86.230, 86.721, 87.204] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B1**, **B7**, **B17** |

#### 4.2.3 `chunked_16k_advance` (Fresh `120x40` terminal, `16 KiB` read-chunk simulation) — `3` Benchmarks

| Benchmark | Baseline Time `[Low, Est, High]` | Baseline Throughput | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Speedup / Delta | Bottleneck(s) |
|---|---|---|---|---|---|---|
| `chunked_16k_advance/ascii_plain` | `[2.4313 ms, 2.4361 ms, 2.4421 ms]` | `[204.75, 205.26, 205.66] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B1**, **B12** |
| `chunked_16k_advance/truecolor_sgr` | `[2.7455 ms, 2.7506 ms, 2.7567 ms]` | `[181.40, 181.81, 182.14] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B7**, **B12** |
| `chunked_16k_advance/osc133_dense` | `[2.4540 ms, 2.4590 ms, 2.4643 ms]` | `[135.50, 135.80, 136.07] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B5**, **B7**, **B12** |

#### 4.2.4 `sync_output_dec2026_200x60` & `osc8_and_graphemes` — `2` Benchmarks

| Benchmark | Baseline Time `[Low, Est, High]` | Baseline Throughput | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Speedup / Delta | Bottleneck(s) |
|---|---|---|---|---|---|---|
| `sync_output_dec2026_200x60/truecolor_combining_frames` | `[3.7272 ms, 3.7410 ms, 3.7551 ms]` | `[226.20, 227.06, 227.90] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B15**, **B17** |
| `osc8_and_graphemes/512k_distinct_links` | `[37.839 ms, 37.915 ms, 38.002 ms]` | `[13.157, 13.187, 13.214] MiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B12**, **B17** |

#### 4.2.5 `reflow_10k_scrollback` & `reflow_10k_with_2500_osc133_zones` — `3` Benchmarks

| Benchmark | Baseline Time `[Low, Est, High]` | Baseline Throughput | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Speedup / Delta | Bottleneck(s) |
|---|---|---|---|---|---|---|
| `reflow_10k_scrollback/narrow_120x40_to_80x40` | `[6.3153 ms, 6.3791 ms, 6.4754 ms]` | `[1.5443, 1.5676, 1.5835] Melem/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B4**, **B14**, **B18** |
| `reflow_10k_scrollback/widen_80x40_to_160x40` | `[3.3846 ms, 3.4123 ms, 3.4418 ms]` | `[2.9055, 2.9306, 2.9546] Melem/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B4**, **B14**, **B18** |
| `reflow_10k_with_2500_osc133_zones/narrow_120x40_to_80x40` | `[141.49 ms, 141.61 ms, 141.72 ms]` | `[70.562, 70.616, 70.679] Kelem/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B4** |

#### 4.2.6 `input_adapters` (`512 KiB` in `16 KiB` chunks, `64 KiB` scratch read) — `2` Benchmarks

| Benchmark | Baseline Time `[Low, Est, High]` | Baseline Throughput | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Speedup / Delta | Bottleneck(s) |
|---|---|---|---|---|---|---|
| `input_adapters/ring_buffer` | `[11.018 µs, 11.034 µs, 11.050 µs]` | `[44.187, 44.251, 44.316] GiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B11** baseline |
| `input_adapters/channel` | `[10.663 µs, 10.690 µs, 10.715 µs]` | `[45.570, 45.678, 45.790] GiB/s` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B11** baseline |

---

### 4.3 Frame Rendering Baseline (`frame_render`, `200x60` = `12,000` cells) — `15` Benchmarks

#### 4.3.1 `frame_render_stages` (Show vs. Show+Tessellate isolation) — `4` Benchmarks

| Benchmark | Baseline Time `[Low, Est, High]` | Baseline Throughput (60fps Share) | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput (60fps Share) | Speedup / Delta | Bottleneck(s) |
|---|---|---|---|---|---|---|
| `frame_render_stages/show_only_idle` | `[4.9637 µs, 4.9716 µs, 4.9797 µs]` | `2.4137 Gelem/s` (`0.03%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B9**, **B10** |
| `frame_render_stages/show_and_tessellate_idle` | `[144.68 µs, 145.12 µs, 145.70 µs]` | `82.692 Melem/s` (`0.91%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B2** |
| `frame_render_stages/show_only_full_redraw` | `[1.8716 ms, 1.8777 ms, 1.8840 ms]` | `6.3910 Melem/s` (`11.74%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B1**, **B3**, **B8**, **B10** |
| `frame_render_stages/show_and_tessellate_full_redraw` | `[2.0022 ms, 2.0072 ms, 2.0121 ms]` | `5.9786 Melem/s` (`12.55%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B2**, **B3**, **B8** |

#### 4.3.2 `frame_render_200x60_truecolor` — `8` Benchmarks

| Benchmark | Baseline Time `[Low, Est, High]` | Baseline Throughput (60fps Share) | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput (60fps Share) | Speedup / Delta | Bottleneck(s) |
|---|---|---|---|---|---|---|
| `frame_render_200x60_truecolor/idle` | `[143.78 µs, 144.26 µs, 144.83 µs]` | `83.185 Melem/s` (`0.90%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B2** |
| `frame_render_200x60_truecolor/1_dirty_row` | `[232.50 µs, 232.70 µs, 232.94 µs]` | `51.568 Melem/s` (`1.45%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B3**, **B8** |
| `frame_render_200x60_truecolor/scroll_1_line` | `[256.22 µs, 256.91 µs, 257.50 µs]` | `46.709 Melem/s` (`1.61%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B3**, **B8** |
| `frame_render_200x60_truecolor/full_redraw` | `[1.9874 ms, 1.9922 ms, 1.9974 ms]` | `6.0236 Melem/s` (`12.45%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B2**, **B3**, **B8** |
| `frame_render_200x60_truecolor/cursor_blink_toggle` | `[178.97 µs, 179.13 µs, 179.29 µs]` | `66.992 Melem/s` (`1.12%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B3** |
| `frame_render_200x60_truecolor/active_selection_1k_lines` | `[229.84 µs, 230.84 µs, 232.05 µs]` | `51.984 Melem/s` (`1.44%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B6** |
| `frame_render_200x60_truecolor/osc133_2500_zones_idle` | `[149.26 µs, 149.86 µs, 150.70 µs]` | `80.074 Melem/s` (`0.94%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B10** |
| `frame_render_200x60_truecolor/scrollback_scrub_10k` | `[115.84 µs, 116.00 µs, 116.19 µs]` | `103.45 Melem/s` (`0.73%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B3**, **B10** |

#### 4.3.3 `frame_render_200x60_utf8` & `frame_render_200x60_tui` — `3` Benchmarks

| Benchmark | Baseline Time `[Low, Est, High]` | Baseline Throughput (60fps Share) | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput (60fps Share) | Speedup / Delta | Bottleneck(s) |
|---|---|---|---|---|---|---|
| `frame_render_200x60_utf8/utf8_wide_full_redraw` | `[921.18 µs, 926.20 µs, 934.17 µs]` | `12.956 Melem/s` (`5.79%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B2**, **B3**, **B13** |
| `frame_render_200x60_utf8/utf8_wide_1_dirty_row` | `[175.04 µs, 176.10 µs, 177.71 µs]` | `68.144 Melem/s` (`1.10%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B3**, **B13** |
| `frame_render_200x60_tui/vector_glyphs_dashboard` | `[8.4188 ms, 8.8714 ms, 9.3805 ms]` | `1.3527 Melem/s` (`55.45%`) | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | **B3**, **B10**, **B13** |

---

### 4.4 M5 Modern Rendering Extensions (`frame_render_200x60_m5`) — `3` Benchmarks

| Benchmark | Baseline Time & Throughput | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput (60fps Share) | Notes / Feature Exercised |
|---|---|---|---|---|
| `frame_render_200x60_m5/bold_and_italic_full_redraw` | `N/A (new in M5)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | SGR 1 (bold) + SGR 3 (italic) full `200x60` redraw via `build_ascii_row_mesh` + `BoldRenderMode` |
| `frame_render_200x60_m5/search_2500_matches_redraw` | `N/A (new in M5)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `>= 2,500` active search matches across `200x60` redraw (`partition_point` + `search_highlight_row_cursor`) |
| `frame_render_200x60_m5/kitty_placeholder_grid` | `N/A (new in M5)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | Full `200x60` Kitty `U+10EEEE` virtual placeholder grid coalesced into row `PlaceholderRun` quads |

---

### 4.5 Native PTY Transport (`pty_transport`) — `4` Benchmarks

| Benchmark | Baseline Time & Throughput | Post-M5 Time `[Low, Est, High]` | Post-M5 Throughput | Notes / Workload Scope |
|---|---|---|---|---|
| `pty_transport_read/512k_16k_chunks` | `N/A (new in M3)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `32 × 16 KiB` (`512 KiB`) through `PtyBackend::poll_read` with `RECYCLE_POOL_CAPACITY = 64` |
| `pty_transport_read/4m_16k_chunks` | `N/A (new in M3)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `256 × 16 KiB` (`4 MiB`) steady-state buffer recycling across `reader_loop` ↔ `poll_read` |
| `pty_transport_idle/10k_poll_read_calls` | `N/A (new in M3)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `10,000` idle `poll_read` calls (`0` `try_wait()` syscalls; pure `try_recv()` empty check) |
| `pty_transport_write/256x64b_burst` | `N/A (new in M3)` | `[PENDING_FINAL_BENCH]` | `[PENDING_FINAL_BENCH]` | `256 × 64 B` (`16 KiB`) UI-thread `write_bytes` burst queued to background `writer_loop` (`WRITER_RECYCLE_CAPACITY = 64`) |

---

## 5. Reproduction Commands & Methodology

### 5.1 Sequential Criterion Execution Protocol

Run all three Criterion `--release` harnesses **sequentially on an idle host** (zero concurrent `cargo build`/`test` jobs) with `RUSTFLAGS` unset so compiler optimization flags match the baseline capture:

```bash
# 1. Record environment & toolchain metadata
date -u +"%Y-%m-%dT%H:%M:%SZ"
git rev-parse HEAD
rustc --version
cargo --version
unset RUSTFLAGS

# 2. Core VT parser, 10k saturated terminal, DEC 2026, OSC 8, reflow & input adapters (22 benches)
cargo bench -p egui_xterm_core --all-features --bench vt_throughput

# 3. Headless 200x60 egui frame update, auto-layout, coalesced mesh render & tessellation (18 benches)
cargo bench -p egui_xterm --all-features --bench frame_render

# 4. Native PTY background reader/writer buffer-pool throughput & idle poll latency (4 benches)
cargo bench -p egui_xterm_pty --bench pty_transport
```

### 5.2 Harness & Workload Methodology

- **`vt_throughput` (`crates/egui_xterm_core/benches/vt_throughput.rs`)**:
  - Uses `criterion 0.8` (`WallTime`) with `BatchSize::LargeInput` / `BatchSize::PerIteration`.
  - Measures `Parser::advance` in isolation (`CountingHandler` overriding `print_ascii_run` & `print_utf8_run`) vs. full `120x40` `Terminal::advance` (`10,000`-line scrollback), saturated `10,000`-line `16 KiB` chunked ingestion (`terminal_saturated_10k`), `16 KiB` chunked stream processing (`chunked_16k_advance`), `200x60` `DEC ?2026` synchronized frames (`sync_output_dec2026_200x60`), `512 KiB` distinct `OSC 8` + grapheme GC churn (`osc8_and_graphemes`), `10,000`-line resize reflow (`sample_size = 20`; `sample_size = 10` for `reflow_10k_with_2500_osc133_zones`), and `RingBufferInput` / `ChannelInput` baselines (`sample_size = 100`).
- **`frame_render` (`crates/egui_xterm/benches/frame_render.rs`)**:
  - Drives a headless `egui::Context::run_ui` (`200x60` = `12,000` cells, `native_pixels_per_point = Some(1.0)`, `ScaleMode::ReflowGrid`, `resize_debounce_secs = 0.0`, `read_budget_bytes_per_frame = usize::MAX`) followed by `ctx.tessellate(shapes, ppp)` (`sample_size = 50`, `30` for `utf8_wide_full_redraw`, `warm_up_time = 2s`, `measurement_time = 4s`).
  - Isolates `show_only` vs. `show_and_tessellate`, truecolor idle/1-row/scroll/full-redraw/blink-toggle/1k-selection/2,500-zone/10k-scrub workloads, CJK/ZWJ/combining rows, `vector-glyphs` + `SGR 4:3` TUI dashboards, and M5 bold/italic, 2,500-match search, and `U+10EEEE` placeholder grids.
- **`pty_transport` (`crates/egui_xterm_pty/benches/pty_transport.rs`)**:
  - Constructs an in-process `PtyBackend::from_parts` with deterministic `BenchReader`, `BenchWriter`, `BenchChild`, `BenchMaster`, and `RepaintCallback` (`Condvar`-synchronized), measuring `512 KiB` and `4 MiB` `16 KiB`-chunked read throughput, `10,000` idle `poll_read` calls, and `256 × 64 B` non-blocking `write_bytes` bursts.

### 5.3 Accompanying Safety, Feature-Matrix & WASM32 Gates

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo +1.95.0 check -p egui_xterm_core --no-default-features
cargo +1.95.0 check -p egui_xterm --no-default-features
cargo +1.95.0 check --target wasm32-unknown-unknown -p egui_xterm_core -p egui_xterm -p egui_xterm_pty -p egui_xterm_demo
```
