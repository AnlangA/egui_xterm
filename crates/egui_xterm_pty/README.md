# `egui_xterm_pty`

`egui_xterm_pty` provides cross-platform PTY backends implementing `TerminalInput`:

- **Desktop (`Linux`, `macOS`, `Windows`)**: `PtyBackend` runs a native shell or command in a pseudoterminal via `portable-pty` with recycled read/write buffer pools and `spawn_with_repaint` UI wakeups.
- **Web (`wasm32-unknown-unknown`)**: `WebPtyBackend` connects over a browser `WebSocket` to a remote PTY gateway (supporting `TaggedBinary`, `Ttyd`, `JsonResize`, and `Raw` wire protocols), with pre-open buffering, coalesced resizes, and `connect_with_repaint` wakeups.
- **All targets**: `protocol` and `WebPtySession` expose the shared framing codecs and transport-agnostic state machine for custom transports or server-side gateways bridging `PtyBackend` to browser clients.

```sh
cargo add egui_xterm_pty
```

```rust
#[cfg(not(target_arch = "wasm32"))]
use egui_xterm_pty::{PtyBackend, PtyConfig, TerminalInput, TerminalIoError};

#[cfg(not(target_arch = "wasm32"))]
fn open_shell() -> Result<impl TerminalInput, TerminalIoError> {
    PtyBackend::spawn(PtyConfig::default())
}
```

On `wasm32-unknown-unknown`, connect to a WebSocket PTY gateway (such as `examples/egui_xterm_web_gateway` or `ttyd`) with `WebPtyBackend`:

```rust
#[cfg(target_arch = "wasm32")]
use egui_xterm_pty::{TerminalInput, TerminalIoError, WebPtyBackend, WebPtyConfig, WebPtyProtocol};

#[cfg(target_arch = "wasm32")]
fn open_web_shell(repaint: egui_xterm_pty::RepaintCallback) -> Result<impl TerminalInput, TerminalIoError> {
    let cfg = WebPtyConfig::new("ws://127.0.0.1:7681/ws")
        .with_protocol(WebPtyProtocol::TaggedBinary);
    WebPtyBackend::connect_with_repaint(cfg, repaint)
}
```

With the `egui_xterm` widget, enable its `pty` feature and import these types from `egui_xterm::pty` on both desktop and `wasm32-unknown-unknown`. See [`examples/egui_xterm_web_gateway`](https://github.com/AnlangA/egui_xterm/tree/main/examples/egui_xterm_web_gateway) for a clean-room native WebSocket PTY bridge pairing `PtyBackend` with browser `WebPtyBackend`.

[API documentation](https://docs.rs/egui_xterm_pty) · [egui widget](https://crates.io/crates/egui_xterm) · [Source and architecture](https://github.com/AnlangA/egui_xterm)

Licensed under either the MIT License or Apache License, Version 2.0, at your option.
