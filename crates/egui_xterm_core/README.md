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

Default features include scrollback search, OSC 8 hyperlinks, OSC 133 shell integration, and the Kitty keyboard protocol. Optional features enable Serde support and metrics.

[API documentation](https://docs.rs/egui_xterm_core) · [egui widget](https://crates.io/crates/egui_xterm) · [Source and architecture](https://github.com/AnlangA/egui_xterm)

Licensed under either the MIT License or Apache License, Version 2.0, at your option.
