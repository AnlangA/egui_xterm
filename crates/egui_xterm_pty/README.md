# `egui_xterm_pty`

`egui_xterm_pty` runs a native shell or command in a pseudoterminal on Linux, macOS, and Windows. `PtyBackend` implements `TerminalInput` and reads child output on a background thread. `spawn_with_repaint` can notify a UI when output arrives.

```sh
cargo add egui_xterm_pty
```

```rust
use egui_xterm_pty::{PtyBackend, PtyConfig, TerminalInput, TerminalIoError};

fn open_shell() -> Result<impl TerminalInput, TerminalIoError> {
    PtyBackend::spawn(PtyConfig::default())
}
```

With the `egui_xterm` widget, enable its `pty` feature and import these same types from `egui_xterm::pty`. On `wasm32` this crate exposes the shared input types.

[API documentation](https://docs.rs/egui_xterm_pty) · [egui widget](https://crates.io/crates/egui_xterm) · [Source and architecture](https://github.com/AnlangA/egui_xterm)

Licensed under either the MIT License or Apache License, Version 2.0, at your option.
