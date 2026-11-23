//! Pure-Rust local WebSocket PTY gateway bridging browser `WebPtyBackend` connections to
//! native `egui_xterm_pty::PtyBackend` child shells.
//!
//! Run with `cargo run -p egui_xterm_web_gateway` to serve `ws://127.0.0.1:7681/ws` using
//! [`WebPtyProtocol::TaggedBinary`], matching `egui_xterm_demo`'s browser defaults.

#![forbid(unsafe_code)]

#[cfg(any(not(target_arch = "wasm32"), test))]
use std::path::PathBuf;

#[cfg(any(not(target_arch = "wasm32"), test))]
use egui_xterm_pty::WebPtyProtocol;

#[cfg(any(not(target_arch = "wasm32"), test))]
mod ws;

#[cfg(any(not(target_arch = "wasm32"), test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct GatewayConfig {
    bind: String,
    protocol: WebPtyProtocol,
    shell: Option<String>,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    show_help: bool,
}

#[cfg(any(not(target_arch = "wasm32"), test))]
impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:7681".to_owned(),
            protocol: WebPtyProtocol::TaggedBinary,
            shell: None,
            args: Vec::new(),
            cwd: None,
            show_help: false,
        }
    }
}

#[cfg(any(not(target_arch = "wasm32"), test))]
fn parse_protocol_name(raw: &str) -> Result<WebPtyProtocol, String> {
    if raw.eq_ignore_ascii_case("tagged") || raw.eq_ignore_ascii_case("tagged_binary") {
        Ok(WebPtyProtocol::TaggedBinary)
    } else if raw.eq_ignore_ascii_case("ttyd") {
        Ok(WebPtyProtocol::Ttyd)
    } else if raw.eq_ignore_ascii_case("json") || raw.eq_ignore_ascii_case("json_resize") {
        Ok(WebPtyProtocol::JsonResize)
    } else if raw.eq_ignore_ascii_case("raw") {
        Ok(WebPtyProtocol::Raw)
    } else {
        Err(format!(
            "unsupported protocol '{raw}' (expected tagged, ttyd, json, or raw)"
        ))
    }
}

#[cfg(any(not(target_arch = "wasm32"), test))]
fn parse_args<I>(iter: I) -> Result<GatewayConfig, String>
where
    I: IntoIterator<Item = String>,
{
    let mut cfg = GatewayConfig::default();
    let mut args = iter.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                cfg.show_help = true;
                return Ok(cfg);
            }
            "-b" | "--bind" => {
                let val = args
                    .next()
                    .ok_or_else(|| format!("missing address value after {arg}"))?;
                cfg.bind = val;
            }
            "-p" | "--proto" => {
                let val = args
                    .next()
                    .ok_or_else(|| format!("missing protocol value after {arg}"))?;
                cfg.protocol = parse_protocol_name(&val)?;
            }
            "-s" | "--shell" => {
                let val = args
                    .next()
                    .ok_or_else(|| format!("missing shell command after {arg}"))?;
                cfg.shell = Some(val);
            }
            "--cwd" => {
                let val = args
                    .next()
                    .ok_or_else(|| "missing directory path after --cwd".to_owned())?;
                cfg.cwd = Some(PathBuf::from(val));
            }
            "--" => {
                cfg.args.extend(args);
                break;
            }
            other => {
                return Err(format!("unknown argument '{other}' (use --help for usage)"));
            }
        }
    }
    Ok(cfg)
}

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
mod native_gateway {
    use std::io::Write as _;
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex, MutexGuard};
    use std::thread;
    use std::time::Duration;

    use egui_xterm_pty::{
        OwnedClientFrame, PtyBackend, PtyConfig, ServerFrame, TerminalInput, WebPtyProtocol,
    };

    use crate::ws::{self, WsFrameReader, WsIncoming};
    use crate::{GatewayConfig, parse_args};

    const USAGE: &str = "\
egui_xterm_web_gateway - Local WebSocket PTY bridge for egui_xterm

USAGE:
    cargo run -p egui_xterm_web_gateway -- [OPTIONS] [-- SHELL_ARGS...]

OPTIONS:
    -b, --bind <ADDR>     TCP bind address (default: 127.0.0.1:7681)
    -p, --proto <PROTO>   Wire protocol: tagged | ttyd | json | raw (default: tagged)
    -s, --shell <CMD>     Shell executable override (default: platform shell)
        --cwd <DIR>       Initial child process working directory
    -h, --help            Print this help message
";

    fn lock_unpoison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        match mutex.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    const fn protocol_cli_name(proto: WebPtyProtocol) -> &'static str {
        match proto {
            WebPtyProtocol::TaggedBinary => "tagged",
            WebPtyProtocol::Ttyd => "ttyd",
            WebPtyProtocol::JsonResize => "json",
            WebPtyProtocol::Raw => "raw",
            _ => "custom",
        }
    }

    fn is_loopback_bind(addr: std::net::SocketAddr) -> bool {
        addr.ip().is_loopback()
    }

    pub(crate) fn run() -> ExitCode {
        let config = match parse_args(std::env::args().skip(1)) {
            Ok(cfg) => cfg,
            Err(err) => {
                eprintln!("Error: {err}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };

        if config.show_help {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }

        let listener = match TcpListener::bind(&config.bind) {
            Ok(l) => l,
            Err(err) => {
                eprintln!(
                    "Failed to bind WebSocket PTY gateway on {}: {err}",
                    config.bind
                );
                return ExitCode::FAILURE;
            }
        };

        let local_addr = match listener.local_addr() {
            Ok(addr) => addr,
            Err(err) => {
                eprintln!("Failed to inspect bound address: {err}");
                return ExitCode::FAILURE;
            }
        };

        println!(
            "egui_xterm Web PTY gateway listening on ws://{local_addr}/ws (protocol={})",
            protocol_cli_name(config.protocol)
        );
        if !is_loopback_bind(local_addr) {
            eprintln!(
                "Warning: bound to non-loopback address {local_addr}; remote hosts can spawn shells without TLS/authentication."
            );
        }
        println!(
            "In egui_xterm_demo (browser), set URL to ws://{local_addr}/ws, Proto: {}, and click Connect Web PTY.",
            protocol_cli_name(config.protocol)
        );
        drop(std::io::stdout().flush());

        for incoming in listener.incoming() {
            match incoming {
                Ok(stream) => {
                    let conn_cfg = config.clone();
                    thread::spawn(move || {
                        if let Err(err) = handle_connection(stream, &conn_cfg) {
                            eprintln!("[gateway] connection ended: {err}");
                        }
                    });
                }
                Err(err) => {
                    eprintln!("[gateway] accept error: {err}");
                }
            }
        }

        ExitCode::SUCCESS
    }

    pub(crate) fn handle_connection(
        mut stream: TcpStream,
        config: &GatewayConfig,
    ) -> std::io::Result<()> {
        drop(stream.set_nodelay(true));
        let info = ws::accept_handshake(&mut stream, config.protocol.default_subprotocol())?;
        eprintln!(
            "[gateway] WebSocket client connected path={} subprotocol={:?}",
            info.path, info.negotiated_subprotocol
        );

        let writer_stream = stream.try_clone()?;
        let ws_writer = Arc::new(Mutex::new(writer_stream));

        let wake_pair = Arc::new((Mutex::new(false), Condvar::new()));
        let wake_pair_cb = Arc::clone(&wake_pair);
        let pty_cfg = PtyConfig {
            shell: config.shell.clone(),
            args: config.args.clone(),
            cwd: config.cwd.clone(),
            ..PtyConfig::default()
        };

        let pty = match PtyBackend::spawn_with_repaint(
            pty_cfg,
            Box::new(move || {
                let (lock, cvar) = &*wake_pair_cb;
                *lock_unpoison(lock) = true;
                cvar.notify_all();
            }),
        ) {
            Ok(pty) => Arc::new(Mutex::new(pty)),
            Err(err) => {
                if let Some(frame) = config
                    .protocol
                    .encode_server_frame(&ServerFrame::Error(err.to_string()))
                {
                    let mut writer = lock_unpoison(&ws_writer);
                    drop(ws::write_ws_message(&mut *writer, &frame));
                }
                let mut writer = lock_unpoison(&ws_writer);
                drop(ws::write_ws_close(&mut *writer, 1011, "PTY spawn failed"));
                return Err(std::io::Error::other(err.to_string()));
            }
        };

        let stop = Arc::new(AtomicBool::new(false));
        let out_stop = Arc::clone(&stop);
        let out_pty = Arc::clone(&pty);
        let out_writer = Arc::clone(&ws_writer);
        let out_wake = Arc::clone(&wake_pair);
        let protocol = config.protocol;

        let out_thread = thread::spawn(move || {
            pump_pty_output(out_pty, out_writer, out_wake, out_stop, protocol);
        });

        let mut ws_reader = WsFrameReader::new();
        loop {
            match ws_reader.read_next(&mut stream) {
                Ok(WsIncoming::Message(msg)) => match protocol.decode_client_message(&msg) {
                    Ok(Some(OwnedClientFrame::Input(bytes))) => {
                        if let Err(err) = lock_unpoison(&pty).write_bytes(&bytes)
                            && (err.is_closed()
                                || !send_server_error(protocol, &ws_writer, err.to_string()))
                        {
                            break;
                        }
                    }
                    Ok(Some(OwnedClientFrame::Resize(size))) => {
                        if let Err(err) = lock_unpoison(&pty).resize(size)
                            && (err.is_closed()
                                || !send_server_error(protocol, &ws_writer, err.to_string()))
                        {
                            break;
                        }
                    }
                    Ok(Some(_) | None) => {}
                    Err(err) => {
                        if !send_server_error(protocol, &ws_writer, err.to_string()) {
                            break;
                        }
                    }
                },
                Ok(WsIncoming::Ping(payload)) => {
                    let mut writer = lock_unpoison(&ws_writer);
                    if ws::write_ws_pong(&mut *writer, &payload).is_err() {
                        break;
                    }
                }
                Ok(WsIncoming::Pong(_)) => {}
                Ok(WsIncoming::Close { code, reason }) => {
                    eprintln!("[gateway] client requested close code={code} reason={reason:?}");
                    let mut writer = lock_unpoison(&ws_writer);
                    drop(ws::write_ws_close(&mut *writer, 1000, ""));
                    break;
                }
                Err(_) => break,
            }
        }

        stop.store(true, Ordering::Release);
        {
            let (lock, cvar) = &*wake_pair;
            *lock_unpoison(lock) = true;
            cvar.notify_all();
        }
        drop(stream.shutdown(Shutdown::Both));
        drop(out_thread.join());
        Ok(())
    }

    fn send_server_error(
        protocol: WebPtyProtocol,
        ws_writer: &Mutex<TcpStream>,
        message: String,
    ) -> bool {
        if let Some(frame) = protocol.encode_server_frame(&ServerFrame::Error(message)) {
            let mut writer = lock_unpoison(ws_writer);
            return ws::write_ws_message(&mut *writer, &frame).is_ok();
        }
        true
    }

    fn pump_pty_output(
        pty: Arc<Mutex<PtyBackend>>,
        ws_writer: Arc<Mutex<TcpStream>>,
        wake_pair: Arc<(Mutex<bool>, Condvar)>,
        stop: Arc<AtomicBool>,
        protocol: WebPtyProtocol,
    ) {
        let (wake_lock, wake_cvar) = &*wake_pair;
        let mut buf = vec![0u8; 16 * 1024];

        while !stop.load(Ordering::Acquire) {
            let mut read_any = false;
            loop {
                let res = lock_unpoison(&pty).poll_read(&mut buf);
                match res {
                    Ok(0) => break,
                    Ok(n) => {
                        read_any = true;
                        if let Some(frame) =
                            protocol.encode_server_frame(&ServerFrame::Output(buf[..n].to_vec()))
                        {
                            let mut writer = lock_unpoison(&ws_writer);
                            if ws::write_ws_message(&mut *writer, &frame).is_err() {
                                stop.store(true, Ordering::Release);
                                break;
                            }
                        }
                    }
                    Err(err) if err.is_closed() => {
                        let exit_code = lock_unpoison(&pty).try_wait().ok().flatten();
                        if let Some(code) = exit_code
                            && let Some(frame) =
                                protocol.encode_server_frame(&ServerFrame::Exit(code))
                        {
                            let mut writer = lock_unpoison(&ws_writer);
                            drop(ws::write_ws_message(&mut *writer, &frame));
                        }
                        let mut writer = lock_unpoison(&ws_writer);
                        drop(ws::write_ws_close(&mut *writer, 1000, "PTY exited"));
                        drop(writer.shutdown(Shutdown::Both));
                        stop.store(true, Ordering::Release);
                        break;
                    }
                    Err(err) => {
                        if !send_server_error(protocol, &ws_writer, err.to_string()) {
                            stop.store(true, Ordering::Release);
                        }
                        break;
                    }
                }
            }

            if stop.load(Ordering::Acquire) {
                break;
            }

            if !read_any {
                let mut notified = lock_unpoison(wake_lock);
                while !*notified && !stop.load(Ordering::Acquire) {
                    let (next, timeout_res) =
                        match wake_cvar.wait_timeout(notified, Duration::from_millis(50)) {
                            Ok(pair) => pair,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                    notified = next;
                    if timeout_res.timed_out() {
                        break;
                    }
                }
                *notified = false;
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    native_gateway::run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parser_defaults_and_overrides() {
        let def = parse_args(Vec::<String>::new()).expect("empty args");
        assert_eq!(def, GatewayConfig::default());

        let parsed = parse_args([
            "--bind".to_owned(),
            "127.0.0.1:9001".to_owned(),
            "--proto".to_owned(),
            "ttyd".to_owned(),
            "--shell".to_owned(),
            "pwsh".to_owned(),
            "--cwd".to_owned(),
            ".".to_owned(),
            "--".to_owned(),
            "-NoLogo".to_owned(),
        ])
        .expect("full args");
        assert_eq!(parsed.bind, "127.0.0.1:9001");
        assert_eq!(parsed.protocol, WebPtyProtocol::Ttyd);
        assert_eq!(parsed.shell.as_deref(), Some("pwsh"));
        assert_eq!(parsed.cwd, Some(PathBuf::from(".")));
        assert_eq!(parsed.args, vec!["-NoLogo".to_owned()]);
        assert!(!parsed.show_help);

        assert!(parse_args(["--proto".to_owned(), "nope".to_owned()]).is_err());
        assert!(parse_args(["--unknown".to_owned()]).is_err());
        assert!(parse_args(["--bind".to_owned()]).is_err());
        assert!(parse_args(["--help".to_owned()]).expect("help").show_help);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn loopback_gateway_bridges_real_pty_over_tagged_binary_websocket() {
        use std::io::{Read as _, Write as _};
        use std::net::{TcpListener, TcpStream};
        use std::time::{Duration, Instant};

        use egui_xterm_pty::{TerminalInput, TerminalSize, WebPtyConfig, WebPtySession};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");

        #[cfg(windows)]
        let (shell, shell_args) = (
            "cmd.exe".to_owned(),
            vec!["/C".to_owned(), "echo GATEWAY_LOOPBACK_OK".to_owned()],
        );
        #[cfg(not(windows))]
        let (shell, shell_args) = (
            "sh".to_owned(),
            vec![
                "-c".to_owned(),
                "printf 'GATEWAY_LOOPBACK_OK\\n'".to_owned(),
            ],
        );

        let conn_cfg = GatewayConfig {
            bind: addr.to_string(),
            protocol: WebPtyProtocol::TaggedBinary,
            shell: Some(shell),
            args: shell_args,
            cwd: None,
            show_help: false,
        };

        let server_handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept client");
            native_gateway::handle_connection(stream, &conn_cfg).expect("gateway connection");
        });

        let mut client = TcpStream::connect(addr).expect("connect loopback");
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("set read timeout");

        let client_key = "dGhlIHNhbXBsZSBub25jZQ==";
        let req = format!(
            "GET /ws HTTP/1.1\r\n\
             Host: {addr}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {client_key}\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        client.write_all(req.as_bytes()).expect("send upgrade");
        client.flush().expect("flush upgrade");

        let mut resp_buf = Vec::new();
        let mut b = [0u8; 1];
        loop {
            client.read_exact(&mut b).expect("read handshake byte");
            resp_buf.push(b[0]);
            if resp_buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let resp_str = std::str::from_utf8(&resp_buf).expect("utf8 handshake");
        assert!(resp_str.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
        assert!(resp_str.contains(&format!(
            "Sec-WebSocket-Accept: {}\r\n",
            ws::compute_accept_key(client_key)
        )));

        let mut session = WebPtySession::new(
            WebPtyConfig::new(format!("ws://{addr}/ws"))
                .with_protocol(WebPtyProtocol::TaggedBinary)
                .with_initial_size(TerminalSize::new(100, 30)),
        )
        .expect("session init");
        for msg in session.mark_open() {
            let mask = [0x21, 0x43, 0x65, 0x87];
            ws::write_masked_client_frame(&mut client, true, ws::OP_BINARY, mask, msg.as_bytes())
                .expect("send initial frame");
        }

        let resize_msg = WebPtyProtocol::TaggedBinary
            .encode_client_resize(TerminalSize::new(90, 28))
            .expect("resize frame");
        ws::write_masked_client_frame(
            &mut client,
            true,
            ws::OP_BINARY,
            [0x11, 0x22, 0x33, 0x44],
            resize_msg.as_bytes(),
        )
        .expect("send resize");

        let mut captured = Vec::new();
        let mut poll_buf = [0u8; 4096];
        let deadline = Instant::now() + Duration::from_secs(10);

        while Instant::now() < deadline {
            match read_server_ws_frame(&mut client) {
                Ok((opcode, payload)) => match opcode {
                    ws::OP_BINARY => {
                        session.push_incoming(egui_xterm_pty::WireMessage::Binary(payload));
                    }
                    ws::OP_TEXT => {
                        let text = String::from_utf8(payload).expect("utf8 text");
                        session.push_incoming(egui_xterm_pty::WireMessage::Text(text));
                    }
                    ws::OP_CLOSE => {
                        session.mark_closed(1000, "EOF", true);
                        break;
                    }
                    _ => {}
                },
                Err(_) => break,
            }

            while let Ok(n) = session.poll_read(&mut poll_buf) {
                if n == 0 {
                    break;
                }
                captured.extend_from_slice(&poll_buf[..n]);
            }
            if String::from_utf8_lossy(&captured).contains("GATEWAY_LOOPBACK_OK")
                && session.try_wait().ok().flatten().is_some()
            {
                break;
            }
        }

        loop {
            match session.poll_read(&mut poll_buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => captured.extend_from_slice(&poll_buf[..n]),
            }
        }

        drop(client);
        server_handle.join().expect("server thread finishes");

        let text = String::from_utf8_lossy(&captured);
        assert!(
            text.contains("GATEWAY_LOOPBACK_OK"),
            "expected echoed gateway output, got: {text:?}"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn read_server_ws_frame(reader: &mut std::net::TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
        use std::io::Read as _;

        let mut hdr = [0u8; 2];
        reader.read_exact(&mut hdr)?;
        let opcode = hdr[0] & 0x0F;
        let len7 = hdr[1] & 0x7F;
        let len = match len7 {
            0..=125 => usize::from(len7),
            126 => {
                let mut ext = [0u8; 2];
                reader.read_exact(&mut ext)?;
                usize::from(u16::from_be_bytes(ext))
            }
            _ => {
                let mut ext = [0u8; 8];
                reader.read_exact(&mut ext)?;
                usize::try_from(u64::from_be_bytes(ext)).unwrap_or(0)
            }
        };
        let mut payload = vec![0u8; len];
        reader.read_exact(&mut payload)?;
        Ok((opcode, payload))
    }
}
