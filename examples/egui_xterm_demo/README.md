# `egui_xterm_demo`

Interactive `eframe` `0.37` demo application showcasing [`egui_xterm`](../../crates/egui_xterm/README.md) across both native desktops and browsers (`wasm32-unknown-unknown`).

---

## Running on Desktop

```bash
cargo run -p egui_xterm_demo
```

Enable live widget-tree inspection if needed:

```bash
cargo run -p egui_xterm_demo --features inspection
```

On desktop targets, the app opens with:
- **Local PTY** tab (`PtyBackend::spawn_with_repaint` via `portable-pty`)
- **Trait Demo** tab (`DemoShellInput` implementing `TerminalInput` in pure Rust)

---

## Running in the Browser (`wasm32-unknown-unknown`)

Install the WASM target and [Trunk](https://trunkrs.dev/) once:

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk
```

Serve the demo locally via [Trunk.toml](Trunk.toml) and [index.html](index.html):

```bash
cd examples/egui_xterm_demo
trunk serve
```

Open `http://127.0.0.1:8080/` in your browser. To produce a static release bundle under `examples/egui_xterm_demo/dist/`:

```bash
cd examples/egui_xterm_demo
trunk build --release
```

---

## Connecting the Web PTY Tab

In the browser build, the **Trait Demo** tabs run locally in WASM with zero backend setup, while the toolbar provides live **Web PTY** controls (`URL`, `Proto`, optional `AuthToken`, `Connect Web PTY`, `Reconnect Web PTY`, `Close Web PTY`):

### Option A: Built-in Pure-Rust Gateway (Recommended, Zero External Tools)

Start the workspace's built-in WebSocket PTY gateway (`examples/egui_xterm_web_gateway`, powered by `egui_xterm_pty::PtyBackend` and zero extra third-party server dependencies) in a second terminal:

```bash
cargo run -p egui_xterm_web_gateway
# Optional flags: --bind 127.0.0.1:7681 --proto tagged|ttyd|json|raw --shell pwsh --cwd .
```

Then in the demo toolbar (`http://127.0.0.1:8080/`):
1. Leave the defaults (`ws://127.0.0.1:7681/ws`, `Proto: Tagged`).
2. Click **Connect Web PTY**.

### Option B: Quickstart with `ttyd`

Alternatively, run [`ttyd`](https://github.com/tsl0922/ttyd) locally (with `-W` / `--writable` so terminal input works):

```bash
# Linux / macOS
ttyd -W -p 7681 bash

# Windows
ttyd -W -p 7681 pwsh
```

Then in the demo toolbar:
1. Set URL to `ws://127.0.0.1:7681/ws` (or pass it via query parameters).
2. Click **Proto** until it displays `Proto: ttyd` (and enter `AuthToken` if your `ttyd` instance requires credential auth).
3. Click **Connect Web PTY**.

### Option C: Custom Server Gateway (`TaggedBinary` / `JsonResize` / `Raw`)

Server applications can bridge [`egui_xterm_pty::PtyBackend`](../../crates/egui_xterm_pty/README.md) with any WebSocket server using the cross-platform codecs in `egui_xterm::pty::protocol` (`WebPtyProtocol::TaggedBinary`, `WebPtyProtocol::Ttyd`, `WebPtyProtocol::JsonResize`, or `WebPtyProtocol::Raw`).

### Query Parameters & Mixed Content

You can pre-populate or auto-connect the Web PTY tab from the page URL:

```text
http://127.0.0.1:8080/?ws=ws%3A%2F%2F127.0.0.1%3A7681%2Fws&proto=ttyd&autoconnect=1
```

Supported query parameters:
- `ws` — WebSocket endpoint (`ws://...` or `wss://...`). When omitted, defaults to `wss://<host>/ws` on `https://` pages or `ws://127.0.0.1:7681/ws` on `http://`.
- `proto` — `tagged` (default), `ttyd`, `json`, or `raw`.
- `autoconnect` — `1`, `true`, or `yes` to open the Web PTY tab immediately on page load.

> **Mixed-Content Note**: Browsers block insecure `ws://` connections from `https://` pages; serve remote PTY gateways over `wss://` in HTTPS deployments.

---

## Testing Web Clipboard (`native-clipboard`)

Browser clipboard access (`window.navigator.clipboard`) requires a **secure context** (`https://`, `http://localhost`, or `http://127.0.0.1`):

- **Copy**: Select text (or `Shift+Drag`) and press `Ctrl+Shift+C` / use the context menu (`Copy`).
- **Paste**: Press `Ctrl+Shift+V`, click **Paste** in the top toolbar, or right-click and choose `Paste`. On insecure origins or when browser read permission is denied, `egui_xterm` falls back cleanly to `egui`'s paste event path.
- **`OSC 52`**:
  1. Click `osc52` in the right **Trait Showcase** sidebar.
  2. Switch **OSC 52 Read** to `Prompt` (or `Allow`) in the toolbar.
  3. Trigger `osc52` again and click **Read Clipboard** in the yellow `OSC 52` banner to read via the async browser Web Clipboard API (`Clipboard: Reading…` appears in the status bar while the Promise is in flight).
