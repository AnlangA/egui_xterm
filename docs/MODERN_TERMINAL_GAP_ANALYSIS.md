# Modern Terminal Survey & Gap Analysis (`egui_xterm`)

> **Scope**: Authoritative comparison of `egui_xterm` against modern reference terminals (**Ghostty**, **Kitty**, **WezTerm**, **Alacritty**, **Foot**), tracking the pre-M2 baseline through M1–M5 feature and performance closure under `#![forbid(unsafe_code)]` and `wasm32-unknown-unknown` portability.

---

## 1. Executive Summary & Evaluation Criteria

`egui_xterm` is an **embeddable, clean-room Rust terminal emulator workspace** designed for [`egui`](https://github.com/emilk/egui) `0.37` / `eframe` `0.37` applications across native desktops (`Windows`, `macOS`, `Linux`) and WebAssembly (`wasm32-unknown-unknown`). Unlike standalone desktop terminal applications, `egui_xterm` optimizes for:

1. **Minimal Host Integration Surface**: Consumers implement a single non-blocking [`TerminalInput::poll_read`](../crates/egui_xterm_core/src/input.rs) method (with no `Send + Sync` bound) and invoke `TerminalRuntime::show(ui)` inside any `egui::Frame` or panel. Container sizing, DPI pixel snapping, `ScaleMode` resize/reflow or font scaling, VT500/modern protocol state, selection, IME, clipboard, and cached `<16ms` rendering happen automatically.
2. **Modular Cargo Feature Gating**: Core VT500, TrueColor, grid/scrollback, reflow, and input traits are always enabled; high-level capabilities (`search`, `hyperlinks`, `shell-integration`, `kitty-keyboard`, `kitty-graphics`, `sixel`, `vector-glyphs`, `serde`, `metrics`, `pty`, `native-clipboard`) live behind explicit Cargo features.
3. **Pure-Safe Rust & WASM32 Parity**: All workspace crates enforce `#![forbid(unsafe_code)]`, use zero external terminal emulation libraries, and compile `egui_xterm_core`, default `egui_xterm`, and `egui_xterm_demo` cleanly for `wasm32-unknown-unknown`.
4. **End-to-End 60 fps Pipeline Performance**: Ingestion, reflow, PTY transport, and `egui` mesh generation avoid per-frame allocations and re-tessellation churn (tracked via `B1`–`B18` in [`docs/BENCHMARK_BASELINE.md`](./BENCHMARK_BASELINE.md)).

---

## 2. Authoritative Industry Survey Matrix

Capabilities below were audited against official documentation and behavior of **Ghostty** (`libghostty-vt` / `ghostty.org`), **Kitty** (`sw.kovidgoyal.net/kitty`), **WezTerm** (`wezterm.org`), **Alacritty** (`alacritty.org`), and **Foot** (`codeberg.org/dnkl/foot`), contrasted against `egui_xterm` before M2 and after M1–M5:

| Dimension / Feature | Ghostty (`libghostty-vt`) | Kitty | WezTerm | Alacritty | Foot | `egui_xterm` (Pre-M2 Baseline) | `egui_xterm` (Post M1–M5) |
|---|---|---|---|---|---|---|---|
| **Primary Role & Embeddability** | Native app + C ABI Zig VT lib (`-Dsimd=false` WASM) | Standalone C/Python GPU app | Standalone Rust GPU app + mux (`wezterm-term` heavy deps) | Standalone Rust GPU app (`alacritty_terminal` core, no UI) | Wayland-only C daemon/client | Embeddable `egui` crate workspace (`wasm32` + native) | Embeddable `egui` crate workspace (`wasm32` + native, `#![forbid(unsafe_code)]`) |
| **VT500 / DEC Margins (`DECLRMM` / `DECSLRM`)** | Full (`?69`) | Partial / legacy subset | Full | Partial | Full | Partial (`?69` incomplete; `BS`/`HT`/`NEL` edge cases) | **Full** (`?69` + `CSI Pl ; Pr s` parity with `SCOSC`) |
| **Rectangular Ops (`DECCRA`/`ERA`/`FRA`/`SERA`) & `DECSCA`** | Unimplemented / partial | No / partial | Full | No | Partial | Missing | **Full** (`DECSCA` + `DECRQSS "q`, `DECSED`/`DECSEL`, all 4 rect ops) |
| **ISO 2022 `G0..G3`, `LS*/SS*`, `DECALN`, `DECBI/FI`** | Full | Partial (`G0/G1`) | Full | `G0/G1` | Full | `G0/G1` only | **Full** (`G0..G3`, `LS2/3/1R/2R/3R`, `SS2/3`, `ESC # 8`, `DECBI/FI`) |
| **`XTPUSHSGR`/`XTPOPSGR` & `XTWINOPS` / `XTGETTCAP` / `DECRQSS` / `XTVERSION`** | Full | Partial | Full | Minimal | Partial | `DECSCUSR`/`DA`/`DSR`/`DECRQM` only | **Full** (`SgrStack` depth 10, `XTWINOPS 11/13/14/16/18/19/20/21/22/23`, `XTGETTCAP`, `DECRQSS`, `XTVERSION`) |
| **DEC 2026 Synchronized Output** | Yes | Yes | Yes | Yes | Yes | Yes (with 250 ms watchdog) | **Yes** (persistent `LatchedFrame` buffer reuse) |
| **Kitty Keyboard Protocol (`CSI > / < / = / ? u`)** | Yes | Yes (Originator) | Yes (opt-in config) | Yes | Yes | Yes (`kitty-keyboard` feature) | **Yes** (+ `KpBegin`, `CapsLock`/`NumLock` tracking, `*_into` zero-alloc encoders) |
| **`modifyOtherKeys` (`CSI > 4 ; Pv m`)** | Partial / Kitty preferred | Discouraged (warns) | Yes | Partial | No | Missing | **Yes** (modes 1 & 2, Kitty precedence, zero-alloc) |
| **SGR-Pixels Mouse (`DEC ?1016`)** | Yes | Yes | Yes | No | Yes | Missing (`1006` cell SGR only) | **Full** (`MouseEncoding::SgrPixels`, pixel coord tracking + cell fallback) |
| **`OSC 8` Hyperlinks + Implicit URLs** | Yes | Yes | Yes | Yes (hints) | Yes | Yes (`hyperlinks` feature) | **Yes** (+ incremental dirty-range `ImplicitLinkStore`, hover URI tooltip, optional Ctrl-click) |
| **`OSC 9;4` Progress Indicator** | Yes | Yes | Yes | No | **No** (`OSC 9` notify only) | Missing | **Full** (`TerminalEvent::Progress` + `widgets/progress_bar.rs` states `0..=4`) |
| **`OSC 22` Pointer Cursor Shapes** | Yes | Yes (Originator) | No / limited | No | Yes | Missing | **Full** (stack `>`/`<`, `=`, `?` queries, zero-alloc `CursorIcon` mapping) |
| **`OSC 52` Clipboard Policy** | Configurable | Configurable | Configurable | Config opt-in | Config opt-in | `bool` `allow_osc52_read` | **Full** (`Osc52ReadPolicy::{Deny, Allow, Prompt}` + `osc52_write_enabled` + UI approval hooks) |
| **`OSC 66` Text Sizing Protocol** | Parsed in `libghostty-vt` (GUI unimplemented) | Yes (Originator) | No | No | Explicit width/sizing | Missing | **Parsed & Scaled** (`TextSizing` + `SIZED_SPACER` + slow-path scaled layout) |
| **`OSC 99` Desktop Notifications** | Parsed in `libghostty-vt` (`OSC 9` GUI) | Yes (Originator) | No (`OSC 9`/`777`) | No | No (`OSC 777`) | Missing | **Full** (`OSC 9`/`99`/`777`; `OSC 99` `i=` chunking, `p=title/body/close/?`, `e=1`, `u=`, `o=`) |
| **`OSC 133` Shell Integration** | Yes | Yes | Yes | In progress / hints | Yes | Yes (`shell-integration` feature) | **Yes** (+ `O(1)` eviction, `O(Z log Z + L)` reflow, `O(log Z)` binary-searched gutter) |
| **Kitty Graphics Protocol** | Yes (Full) | Yes (Originator) | Yes (opt-in) | **No** (by design) | **No** | Missing | **Full** (`t=d/f/t/s`, concurrent chunking, `a=f/a/c` animations, `U=1`/`U+10EEEE`, `P/Q/H/V`, CRC32/Adam7 PNG, `revision`) |
| **Sixel Graphics** | **No** (explicitly omitted) | **No** (explicitly omitted) | Yes (preliminary) | **No** (by design) | Yes | Missing | **Full** (`P1`/`P2`/`Pan;Pad;Ph;Pv`, `DECSDM` `?80`, `?1070`, `?8452`, `O(1)`-amortized row canvas, dynamic `cell_px`) |
| **Text Shaping & Ligatures** | HarfBuzz/CoreText (ligatures) | HarfBuzz (ligatures) | HarfBuzz (ligatures) | Crossfont (no ligatures) | `fcft` (no ligatures) | `egui` `LayoutJob` + `AsciiGlyphCache` | Coalesced `text_mesh` + `AsciiGlyphCache` + zero-alloc `cluster_cache` + bold/faux-bold weights; ligatures deferred |
| **Cell Memory & Scrollback** | Page-arena + style table | Ring + extra attrs | Line/Cell vecs | Ring grid | Line pool | 20B `Copy` `Cell` + `CellExtraTable` + row pool | 20B `Copy` `Cell` + `ExtraSlot` reuse + `Row::trimmed_len_cache` + `O(Z log Z + L)` reflow |
| **Parser & Render Pipeline** | SIMD VT + Metal/OpenGL | SIMD + OpenGL | Rust state + WGPU/GL | Rust `vte` + OpenGL/GLES | SIMD/C + Wayland SHM | Safe SWAR ASCII + `memchr`; 60 `Shape::Text` galleys | Safe SWAR + batch UTF-8 + `memchr` + split `Parser`/`TerminalState` + coalesced `bg_mesh`/`text_mesh`/`overlay_mesh` |

### Key Ecosystem Trade-Offs Observed
- **Kitty & Ghostty explicitly omit Sixel**: Both reject Sixel in favor of the Kitty Graphics Protocol, which means applications emitting Sixel (`gnuplot`, `lsix`, `img2sixel`, legacy DEC tools) cannot display inline graphics there. `egui_xterm` implements **both** `kitty-graphics` and `sixel` behind independent default-on Cargo features.
- **Alacritty omits both Sixel & Kitty Graphics** (as well as `OSC 9;4`, `OSC 22`, `OSC 66`, `OSC 99`, and `DEC ?1016`), prioritizing minimal desktop emulator scope; `alacritty_terminal` also provides no GUI widget layer.
- **Foot is Wayland-only and omits Kitty Graphics & `OSC 9;4`**: Excellent Wayland throughput and Sixel/`OSC 133`/`OSC 66` support, but unavailable on macOS, Windows, `wasm32`, or embedded Rust UIs.
- **WezTerm is a full standalone mux/terminal**: Supports Kitty Graphics, experimental Sixel, and iTerm2 images, but `wezterm-term` drags in large shaping/multiplexing ecosystems rather than an `egui`-native `TerminalInput` widget.

---

## 3. Capability Inventory: Pre-M2 Baseline vs. M1–M5 Additions

### A. VT / CSI / DEC / OSC / DCS Protocols
- **Pre-M2 Baseline**:
  - 14-state clean-room DEC VT500 `Parser` (`C1Policy::Utf8Only` default, `C1Policy::Vt500EightBit` opt-in, 64-slot `CsiParams` supporting `;` and `:`).
  - `G0`/`G1` character sets (`Ascii`, `DecSpecialGraphics`), `DECSC`/`DECRC`, `DECSTBM`, `DECSCUSR`, `DA1`/`DA2`/`DSR`/`DECRQM`/`DECSTR`, SGR 16/256/24-bit TrueColor (`;` and `:`), 5 underline styles (`4:1..=5`), overline (`SGR 53`), `DEC ?2026` synchronized output with 250 ms watchdog, alternate screen (`?47`/`?1047`/`?1049`), mouse protocols (`?9`/`?1000`/`?1002`/`?1003`/`?1005`/`?1006`/`?1007`), bracketed paste (`?2004`), `OSC 0/1/2/4/7/8/10/11/12/52/104/110/111/112/133`, and `kitty-keyboard` (`CSI >/</=/ ? u`).
- **M2 Additions**:
  - **VT/CSI/ESC/DCS**: `SL`/`SR` (`CSI Ps SP @`/`A`); `DECSCA` (`CSI Ps " q` + live `DECRQSS "q`); selective erase `DECSED`/`DECSEL` (`CSI ? Ps J/K`); rectangular area ops (`DECCRA`, `DECERA`, `DECFRA`, `DECSERA`); `XTPUSHSGR`/`XTPOPSGR` (`CSI [Pm] # {` / `CSI # }` / `# p` bounded 10-deep `SgrStack`); `XTWINOPS` (`11/13/14/16/18/19/20/21/22/23 t`); `XTGETTCAP` (`DCS + q`); `DECRQSS` (`DCS $ q` for `m`, `r`, `q`, `"q`); `XTVERSION` (`CSI > q`); `G2`/`G3` charsets with `LS2`/`LS3`/`LS1R`/`LS2R`/`LS3R`/`SS2`/`SS3`; `DECALN` (`ESC # 8`); `DECBI`/`DECFI`; `DECLRMM` (`CSI ? 69 h/l`) + `DECSLRM` (`CSI Pl ; Pr s` parity with `SCOSC`); `DEC ?2027` (`DECRQM` grapheme clustering); `DEC ?2031` + `DSR ? 996 n` color-scheme reporting.
  - **Modern OSC & Input**: `OSC 5`/`105` dynamic special SGR colors (`SpecialColors`); `OSC 6` document URI (`Terminal::document_uri()`, `TerminalEvent::DocumentChanged`); `OSC 9;4` progress reporting (`TerminalEvent::Progress`); `OSC 9`/`99`/`777` notifications (keyed chunked `OSC 99` supporting `i=`, `p=title/body/close/?`, `e=0/1`, `d=0/1`, `u=0/1/2`, `o=always/unfocused/invisible`); `OSC 22` pointer shape stack (`>`/`<`), set (`=`), and `?` queries (`TerminalEvent::MouseShape`); `OSC 66` text sizing metadata (`TextSizing` + `CellFlags::SIZED_SPACER`); `Osc52ReadPolicy::{Deny, Allow, Prompt}` (`TerminalEvent::ClipboardReadPrompt`); `DEC ?1016` SGR-pixels mouse encoding (`MouseEncoding::SgrPixels`, `encode_mouse_event_pixels_into`); `modifyOtherKeys` (`CSI > 4 ; Pv m` / `CSI ? 4 m`, `ModifyOtherKeys`); and `?2004` `ESC[201~` paste sanitization.
  - **Core Search, Selection & Snapshot APIs**: `SearchQuery::whole_word`; `O(log M)` row-indexed match lookup (`search_matches_on_line`, `search_matches_on_abs_line`, `search_matches_in_range`); incremental search (`begin_incremental_search`, `step_incremental_search`, `refresh_search`); `Terminal::has_non_empty_selection()`; `palette_epoch` + `special_colors` tracking; zero-alloc `RenderSnapshot<'_>` via `VisibleRowsView<'a>`; and split `Parser` + `TerminalState`.

### B. Inline Graphics & Media (`inline_images`)
- **Pre-M2 Baseline**: Absent (explicitly deferred as future work in the initial `ARCHITECTURE.md`).
- **M4 Additions** (zero new external dependencies, `#![forbid(unsafe_code)]`, pure-Rust decoders):
  - **Pure-Rust Decoders**: Base64 (`base64.rs`), RFC 1950/1951 zlib/DEFLATE (`inflate.rs`), full PNG (`png.rs`: IHDR/PLTE/tRNS/IDAT/IEND, filters `0..=4`, bit depths `1/2/4/8/16`, IEEE 802.3 CRC-32 validation, Adam7 7-pass interlacing, strict chunk ordering & pre-inflate `IDAT` cap), and Sixel (`sixel.rs`).
  - **Kitty Graphics Protocol (`kitty.rs`, `store.rs`)**:
    - Actions `a=t/T/p/d/f/a/c/q`, pixel formats `f=24/32/100`, zlib `o=z`, quiet levels `q=0/1/2`.
    - Mediums `t=d/f/t/s` gated by `KittyTransportPolicy` / `KittyFileMediumPolicy` / `KittyShmReader` (`O=`/`S=` slicing, `/proc|/sys|/dev` & non-regular file guards, temp-dir `"tty-graphics-protocol"` unlink guard on `t=t`, safe Linux/FreeBSD `/dev/shm` + host callback on `t=s`, uniform `EBADF` on error or `wasm32`).
    - Concurrent multi-ID `pending_kitty` chunking (`m=1`/`m=0`, keyed by `ImageId(i)` / `ImageNumber(I)` / `Anonymous`, capped at 16 transfers / 32 MiB; immediate `a=q/d/p/a/c` never swallowed).
    - Animations (`a=f` frame upload/edit, `a=a` `s=1/2/3, v, c, r, z`, `a=c` frame composition, `tick_animations`, `next_animation_deadline`).
    - Unicode placeholders (`U=1` virtual placements, `KITTY_PLACEHOLDER_CHAR` `U+10EEEE`, 297-entry `KITTY_DIACRITICS`, `decode_placeholder_cell`), relative placements (`P/Q/H/V`, `ENOPARENT`/`ECYCLE`/`ETOODEEP`, cascade delete & movement), and full `a=d, d=...` delete specifiers (`a/A`, `c/C`, `f/F`, `i/I`, `n/N`, `p/P`, `q/Q`, `r/R`, `x/X`, `y/Y`, `z/Z`).
    - `ImageStore` 64 MiB LRU budget, `StoredImage::revision: u64` + `active_rgba()`, `abs_line` reflow remapping via `ReflowAnchors::extra_points`, scroll-region shifting (`shift_placements_in_region`), viewport-scoped erase (`clear_viewport_placements`), and dynamic `Terminal::set_cell_pixel_size(w, h)`.
  - **Sixel Graphics (`sixel.rs`, `modes.rs`, `term/mod.rs`)**:
    - `DCS P1 ; P2 ; P3 q` + raster `"Pan;Pad;Ph;Pv` (`SixelImage`), color registers `#` (HLS `Pu=1` & RGB `Pu=2`), repeat `!`, `$`, `-`.
    - Fixed `P2` transparency (`P2=1` transparent `[0,0,0,0]`, `P2=0/2` fill unset pixels with register `0`), `DECSDM` (`DEC ?80`), private vs shared color registers (`DEC ?1070`), cursor placement after Sixel (`DEC ?8452`), `O(1)`-amortized row-based growing canvas (`rows: Vec<Vec<[u8; 4]>>`), and dynamic cell pixel sizing.

### C. Text, Fonts, Color & `egui` Widget UX
- **Pre-M2 Baseline**: 20B `Copy` `Cell` + sparse `CellExtra`, `unicode-segmentation` + `unicode-width`, 5 underline styles, `vector-glyphs` (Box Drawing, Block Elements, Braille, Powerline), `ScaleMode::ReflowGrid` / `AutoScaleToFit`, IME, clipboard, focus lock, overlays (`search_bar`, `scrollbar`, `prompt_gutter`, `hyperlink`, `context_menu`).
- **M5 Additions**:
  - **Rendering & Presentation (`renderer.rs`, `config.rs`)**: Bold/faux-bold font weight rendering (`monospace_bold` / variable `wght` / faux-bold offset), `OSC 5` `special_colors`, `OSC 66` scaled text layout + `SIZED_SPACER` skip, SGR `BLINK_SLOW`/`BLINK_RAPID` 500 ms cycle, configurable visual bell (`visual_bell_color`, `visual_bell_opacity`, `visual_bell_duration_ms`), cached image texture `revision: u64` invalidation + `sub_x`/`sub_y` + `U+10EEEE` run-coalesced placeholder quads, and IME `active_range_chars` visual highlight.
  - **Widgets & Input (`runtime.rs`, `input_map.rs`, `widgets/*`)**: `OSC 9;4` progress bar (`ProgressIndicator`, `show_progress_bar`, `widgets/progress_bar.rs`), expanded zero-alloc `OSC 22` CSS/X11 pointer cursor mapping + hyperlink hover precedence + `hyperlink_require_ctrl` + hover URI tooltip, `OSC 52` write/read policy configuration (`osc52_write_enabled`, `osc52_read_policy`, `TerminalEvent::ClipboardReadRequested` / `ClipboardReadPrompt`), `SearchQuery::whole_word` + inline regex error display in `search_bar`, `modifyOtherKeys` + `Key::Clear` -> `KpBegin` + `CapsLock`/`NumLock` tracking (`set_lock_modifiers`), and `last_io_error()` / `take_io_error()` surfacing across I/O operations.

### D. End-to-End Performance Architecture (`B1`–`B18`)
Using the canonical bottleneck IDs from [`docs/BENCHMARK_BASELINE.md`](./BENCHMARK_BASELINE.md) §4:
- **M2 (`egui_xterm_core`)**: Lazy dirty-range `ImplicitLinkStore` + `"://"` prefilter (`B1`), `O(Z log Z + L)` reflow with buffer pool & `ReflowAnchors` (`B4`), `VecDeque<ShellZone>` + `O(1)` eviction (`B5`), `print_utf8_run` batch UTF-8 + `memchr` `OscString`/DCS/APC + CSI digit fast-path (`B7`), split `Terminal { parser, state }` eliminating `mem::take(&mut self.parser)` + zero-copy `dispatch_osc_slice` (`B12`), `Row::trimmed_len_cache` (`B14`), persistent `LatchedFrame` reuse (`B15`), zero-alloc `*_into` key/mouse/paste encoders (`B16`), `ExtraSlot` capacity retention + bitset GC (`B17`), and row capacity pooling (`B18`).
- **M3 (`egui_xterm_pty`)**: `PtyBackend` recycled read/write buffer pools (`RECYCLE_POOL_CAPACITY`, `WRITER_RECYCLE_CAPACITY`), dedicated non-blocking background writer thread, and decoupled `try_wait()` (`B11`).
- **M5 (`egui_xterm`)**: Coalesced `text_mesh: Arc<Mesh>` (`B2`), cursor separated from `GlobalRenderKey`/`BgLayoutKey` + `CachedRowSlot` incremental row `bg_mesh` updates (`B3`), `O(1)` zero-alloc `has_non_empty_selection()` in `context_menu` (`B6`), direct `build_ascii_row_mesh` bypassing `LayoutJob`/`LayoutSection`/`FontId::clone` (`B8`), `palette_epoch` + `last_theme` equality + cached `measure_cell` + borrowed `terminal.title()` (`B9`), cached `overlay_mesh` + `is_vec`/`U+10EEEE` ASCII-row preservation + binary-searched `prompt_gutter` (`B10`), coalesced `poll_read` + single per-frame `write_bytes` flush (`B11`), zero-alloc `cluster_cache` + standalone cluster quads (`B13`), and `O(log M)` `partition_point` search highlights.

---

## 4. Crate, Module & Cargo Feature Mapping

| Capability Area | Crate | Module Path | Cargo Feature | Default | WASM32 Safe |
|---|---|---|---|---|---|
| `TerminalInput`, `TerminalSize`, `TerminalIoError`, `FnInput`, `ChannelInput`, `RingBufferInput` | `egui_xterm_core` | `src/input.rs` | Always on | Yes | Yes |
| VT500 `Parser` (SWAR ASCII, batch UTF-8, `memchr`, 64-slot `CsiParams`) | `egui_xterm_core` | `src/parser/{mod,state,params,utf8,handler,machine}.rs` | Always on | Yes | Yes |
| 20B `Copy` `Cell`, `CellExtra`, `TextSizing`, `UnderlineStyle`, `HyperlinkPool`, `ColorScheme`, `SpecialColors` | `egui_xterm_core` | `src/cell.rs`, `src/color.rs` | Always on | Yes | Yes |
| `Grid` ring buffer, `Row` pool + `trimmed_len_cache`, `CellExtraTable`, `DirtyTracker`, `O(Z log Z + L)` reflow | `egui_xterm_core` | `src/grid/{mod,row,extras,reflow}.rs` | Always on | Yes | Yes |
| `Terminal` / `TerminalState`, `RenderSnapshot`, `VisibleRowsView`, VT/CSI/ESC/DCS/OSC/Modes/Selection/Mouse | `egui_xterm_core` | `src/term/{mod,cursor,dcs,modes,mouse,osc,selection,sgr,tabs}.rs` | Always on | Yes | Yes |
| Literal/regex/whole-word scrollback & incremental search + floating `search_bar` | `egui_xterm_core`, `egui_xterm` | `core/src/features/search.rs`, `egui_xterm/src/widgets/search_bar.rs` | `search` | **Yes** | Yes |
| `OSC 8` + lazy `ImplicitLinkStore` + hover tooltip & click activation | `egui_xterm_core`, `egui_xterm` | `core/src/features/hyperlinks.rs`, `egui_xterm/src/widgets/hyperlink.rs` | `hyperlinks` | **Yes** | Yes |
| `OSC 133` `ShellZone`s, `ShellZonesView` + binary-searched `prompt_gutter` | `egui_xterm_core`, `egui_xterm` | `core/src/features/shell_integration.rs`, `egui_xterm/src/widgets/prompt_gutter.rs` | `shell-integration` | **Yes** | Yes |
| Kitty Keyboard (`CSI >/</=/ ? u`), `modifyOtherKeys`, lock-key tracking, zero-alloc encoders | `egui_xterm_core`, `egui_xterm` | `core/src/features/kitty_keyboard.rs`, `egui_xterm/src/input_map.rs` | `kitty-keyboard` | **Yes** | Yes |
| Kitty Graphics (`t=d/f/t/s`, animations, `U+10EEEE`, `P/Q/H/V`, CRC32/Adam7 PNG, inflate, base64) & texture cache | `egui_xterm_core`, `egui_xterm` | `core/src/features/inline_images/{mod,store,kitty,png,inflate,base64}.rs`, `egui_xterm/src/renderer.rs` | `kitty-graphics` | **Yes** | Yes (`t=f/t/s` disabled with `EBADF`) |
| Sixel Graphics (`P1/P2/Pan;Pad;Ph;Pv`, `DECSDM` `?80`, `?1070`, `?8452`, row canvas) & texture cache | `egui_xterm_core`, `egui_xterm` | `core/src/features/inline_images/{mod,store,sixel}.rs`, `egui_xterm/src/renderer.rs` | `sixel` | **Yes** | Yes |
| Pixel-snapped vector Box Drawing, Block Elements, Braille & Powerline glyphs | `egui_xterm` | `src/renderer.rs` | `vector-glyphs` | **Yes** | Yes |
| `TerminalRuntime`, `TerminalWidget`, `TerminalConfig`, `ScaleMode`, auto-layout, `bg_mesh`/`text_mesh`/`overlay_mesh`, `progress_bar`, `scrollbar`, `context_menu` | `egui_xterm` | `src/{lib,config,layout,input_map,renderer,runtime}.rs`, `src/widgets/*` | Always on | Yes | Yes |
| Serializable `TerminalSnapshot`, `TerminalConfig`, `Theme`, `ColorScheme`, `SearchQuery` | `egui_xterm_core`, `egui_xterm` | `serde` derives across core & widget types | `serde` | No | Yes |
| `TerminalMetrics` (`bytes_processed`, `frames_rendered`, `reflow_count`, etc.) | `egui_xterm_core`, `egui_xterm` | `core/src/features/metrics.rs` | `metrics` | No | Yes |
| Desktop `PtyBackend` (`portable-pty 0.9`, recycled reader/writer pools, ConPTY handshake) | `egui_xterm_pty`, `egui_xterm` | `crates/egui_xterm_pty/src/{lib,shell}.rs` | `pty` | No | Stubs only on `wasm32` |
| OS clipboard fallback (`arboard`) | `egui_xterm` | `src/input_map.rs` | `native-clipboard` | No | **No** (Desktop) |

---

## 5. Prioritized Remaining Gaps & Intentional Non-Goals

### Prioritized Remaining Gaps
1. **P1 — Programming Ligatures & Multi-Cell Contextual Shaping**: `egui 0.37` relies on `epaint::Fonts` and `egui_xterm`'s cell-snapped `AsciiGlyphCache` / single-cluster `cluster_cache` to keep `200x60` frame times sub-millisecond. Cross-cell contextual ligatures (e.g., `->`, `!==`) across heterogeneous SGR attribute runs require custom HarfBuzz span shaping and cell-boundary rules; deferred post-v0.1.
2. **P1 — Bidirectional Text (Unicode UAX #9 BiDi / `ECMA-48` `SPD`/`SDS`)**: Full implicit terminal BiDi reordering (also omitted by Alacritty and Foot) interacts deeply with cursor addressing, selection, and soft-wrapping; left for a dedicated milestone.
3. **P2 — Native macOS / Windows `t=s` POSIX `shm_open` Without `unsafe`**: Linux/FreeBSD `/dev/shm` works in pure-safe `std::fs` under `#![forbid(unsafe_code)]`. On macOS and Windows, kernel `shm_open` / `OpenFileMapping` requires FFI (`unsafe`). Core delegates this safely via the optional host [`KittyShmReader`](../crates/egui_xterm_core/src/features/inline_images/kitty.rs) trait without introducing `unsafe` into default builds.
4. **P2 — Numpad Key Differentiation in `egui` (`KP_0..9`, `KP_Enter`)**: `egui 0.37`'s `egui::Key` enum collapses main-row and numpad digits/operators (`Key::Clear` -> `KittyKey::KpBegin` is distinct). Full `KP_0..9` / `KP_Enter` distinction awaits upstream `egui` physical scancode / numpad key variants.
5. **P2 — Sub-Row Smooth Pixel Scrolling & `OSC 1337;File=` Inline Images**: Viewport scrolling is row-granular to preserve `CachedRowSlot` rotation efficiency; iTerm2 `OSC 1337;File=` is lower priority given full Kitty Graphics and Sixel coverage.

### Intentional Non-Goals (Architectural Boundaries)
- **Built-In Tab/Split Multiplexer, Daemon, or Config File Parser**: `egui_xterm` is a composable widget/engine library, not an application shell. Host `egui`/`eframe` applications own tabs, splits, window chrome, keybindings, and config files (demonstrated in [`examples/egui_xterm_demo`](../examples/egui_xterm_demo/src/main.rs)).
- **Audio Playback / System Bell Waveforms**: Core emits `TerminalEvent::Bell` and `egui_xterm` provides a configurable visual bell flash; host apps play audio if desired, keeping OS audio dependencies out of core and `wasm32`.
- **Mandatory `unsafe` SIMD Intrinsics**: Safe 8-byte SWAR + `memchr` achieves `> 6.7 GiB/s` raw ASCII parser throughput while keeping `#![forbid(unsafe_code)]` and `wasm32` parity across every crate.

---

## 6. Verification Summary

Every capability documented above is backed by multi-layered verification across the workspace:

- **Per-Crate Unit & Headless UI Suites**: `egui_xterm_core` (VT500/modern protocols, `Grid`/reflow, `search`, `hyperlinks`, `shell-integration`, `kitty-keyboard`, `inline_images`), `egui_xterm` (layout snapping, `renderer` mesh/cache/weight/image invariants, headless `egui::Context::run_ui` + `AccessKit` tests), and `egui_xterm_pty` (live PTY lifecycle, buffer recycling, non-blocking writer, ConPTY sizing).
- **Runnable Doc-Tests**: Verified across `cargo test --doc --workspace`, `--all-features`, and `--no-default-features`.
- **Opaque-Box E2E Suite (`crates/egui_xterm_e2e`)**: Tiers 1–5 (`wave1_protocol_and_ux`, `wave2_vt_and_osc`, `wave2_search_and_snapshot`, `wave3_inline_graphics`, `wave3_ux_and_input`, `modern_features`, `tier1_features` through `tier5_adversarial`) exercising public APIs under `#![forbid(unsafe_code)]`.
- **WASM32 & Lint Gates**: `cargo +1.95.0 check --target wasm32-unknown-unknown` across `egui_xterm_core`, `egui_xterm`, `egui_xterm_pty`, and `egui_xterm_demo`, alongside `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- **Criterion `--release` Benchmarks**: `vt_throughput`, `frame_render`, and `pty_transport` tracked in [`docs/BENCHMARK_BASELINE.md`](./BENCHMARK_BASELINE.md), [`docs/BENCHMARKS.md`](./BENCHMARKS.md), and [`docs/PERFORMANCE_REPORT.md`](./PERFORMANCE_REPORT.md).
