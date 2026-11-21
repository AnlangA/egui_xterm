# `egui_xterm_core`

`egui_xterm_core` is a pure-Rust VT500 terminal emulator for native and WebAssembly applications. It manages the terminal parser, grid, scrollback, resize reflow, and input encoding. The `TerminalInput` trait connects a terminal frontend to a non-blocking byte source.

```sh
cargo add egui_xterm_core
```

```rust
use egui_xterm_core::{CoreConfig, Terminal};

let mut terminal = Terminal::new(80, 24, CoreConfig::default());
terminal.advance(b"\x1b[32mHello, terminal!\x1b[0m\r\n");
let first_line = terminal.render_snapshot_line_text(0);
```

Default features include `search` (scrollback search), `hyperlinks` (OSC 8 and implicit URLs), `shell-integration` (OSC 133 prompt zones), `kitty-keyboard` (Kitty keyboard protocol), `kitty-graphics` (Kitty Graphics Protocol), and `sixel` (Sixel inline graphics). Optional features enable `serde` support and `metrics`.

## Feature Flags

| Feature | Default | WASM32 Safe | Description |
|---|---|---|---|
| `search` | **Yes** | Yes | Literal, `regex`, and `whole_word` scrollback search |
| `hyperlinks` | **Yes** | Yes | Deduplicated `OSC 8` `HyperlinkPool` and lazy `ImplicitLinkStore` |
| `shell-integration` | **Yes** | Yes | `OSC 133` semantic prompt, input, and output zone tracking |
| `kitty-keyboard` | **Yes** | Yes | Kitty progressive keyboard protocol (`CSI > / < / = / ? u`) and `modifyOtherKeys` |
| `kitty-graphics` | **Yes** | Yes | Kitty Graphics Protocol (`t=d/f/t/s`, animations, `U+10EEEE`, pure-Rust PNG/zlib; `t=f/t/s` disabled with `EBADF` on `wasm32`) |
| `sixel` | **Yes** | Yes | Sixel inline graphics (`DCS ... q`, `DECSDM` `?80`, `?1070`, `?8452`, `O(1)`-amortized row canvas) |
| `serde` | No | Yes | `serde::Serialize` / `Deserialize` on core terminal types and snapshots |
| `metrics` | No | Yes | Lightweight `TerminalMetrics` throughput and frame counters |

[API documentation](https://docs.rs/egui_xterm_core) · [egui widget](https://crates.io/crates/egui_xterm) · [Source and architecture](https://github.com/AnlangA/egui_xterm)

Licensed under either the MIT License or Apache License, Version 2.0, at your option.
