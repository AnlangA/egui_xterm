//! Opaque-box E2E tests for Web PTY sessions (`WebPtySession` + wire protocols) and
//! cross-platform clipboard runtime APIs (`native-clipboard` / fallback).

use egui::{RawInput, Rect, ViewportCommand, ViewportId, pos2, vec2};
#[cfg(feature = "pty")]
use egui_xterm::TerminalEvent;
use egui_xterm::{ClipboardSelection, Osc52ReadPolicy, RingBufferInput, TerminalRuntime};
use egui_xterm_e2e::HeadlessTermHarness;

#[test]
fn clipboard_public_runtime_api_respects_policy_and_feature_gates() {
    let mut h = HeadlessTermHarness::scripted();
    let _ = h.run_frame();

    let platform_avail = TerminalRuntime::<RingBufferInput>::is_platform_clipboard_available();
    #[cfg(all(feature = "native-clipboard", not(target_arch = "wasm32")))]
    assert!(platform_avail);
    assert!(!h.runtime.has_pending_clipboard_read());

    // Deny policy always rejects OSC 52 platform clipboard reads.
    h.config.osc52_read_policy = Osc52ReadPolicy::Deny;
    let _ = h.run_frame();
    assert!(
        !h.runtime
            .request_osc52_read_from_clipboard(&h.ctx, ClipboardSelection::Clipboard)
    );
    assert!(!h.runtime.approve_osc52_read_from_clipboard(&h.ctx));
    assert!(h.runtime.input().written_bytes().is_empty());

    // Under Prompt policy with a pending OSC 52 read, when platform clipboard is unavailable,
    // `approve_osc52_read_from_clipboard` returns false AND leaves `pending_osc52_read` intact.
    h.config.osc52_read_policy = Osc52ReadPolicy::Prompt;
    h.runtime.input_mut().push_str("\x1b]52;c;?\x07");
    let _ = h.run_frame();
    assert_eq!(
        h.runtime.pending_osc52_read(),
        Some(ClipboardSelection::Clipboard)
    );

    if !platform_avail {
        assert!(!h.runtime.approve_osc52_read_from_clipboard(&h.ctx));
        assert_eq!(
            h.runtime.pending_osc52_read(),
            Some(ClipboardSelection::Clipboard)
        );

        // Requesting paste without platform clipboard queues ViewportCommand::RequestPaste.
        h.runtime.request_clipboard_paste(&h.ctx);
        let out = h.ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 480.0))),
                time: Some(1.0),
                ..Default::default()
            },
            |ui| {
                let _ = h.runtime.show(ui);
            },
        );
        let root_cmds = out
            .viewport_output
            .get(&ViewportId::ROOT)
            .map(|v| v.commands.as_slice())
            .unwrap_or(&[]);
        assert!(
            root_cmds
                .iter()
                .any(|c| matches!(c, ViewportCommand::RequestPaste)),
            "expected RequestPaste fallback when platform clipboard is unavailable"
        );
        out.drop_without_applying_deltas();
    } else {
        // Headless CI may or may not have an active OS clipboard; either way it must never panic.
        let _ = h.runtime.approve_osc52_read_from_clipboard(&h.ctx);
        h.runtime.request_clipboard_paste(&h.ctx);
        let _ = h.run_frame();
    }

    // Same-frame Ctrl+Shift+V + Event::Paste must paste exactly once (no double-paste).
    h.focus_terminal();
    let _ = h.run_frame();
    h.runtime.input_mut().clear_written();
    h.inject_key(
        egui::Key::V,
        true,
        egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
    );
    h.inject_paste("single_paste");
    let _ = h.run_frame();
    assert_eq!(h.runtime.input().written_bytes(), b"single_paste");
}

#[cfg(feature = "pty")]
#[test]
fn web_pty_session_end_to_end_across_protocols_in_runtime() {
    use egui_xterm::pty::{
        OwnedClientFrame, ServerFrame, WebPtyConfig, WebPtyProtocol, WebPtySession, WebPtyState,
        WireMessage,
    };
    use egui_xterm::{TerminalInput, TerminalSize};

    // 1. TaggedBinary: pre-open resize + keystrokes -> open handshake -> output/exit/error/EOF.
    let cfg = WebPtyConfig::new("wss://pty.example.com/ws")
        .with_protocol(WebPtyProtocol::TaggedBinary)
        .with_initial_size(TerminalSize::new(80, 24));
    let session = WebPtySession::new(cfg).expect("valid wss URL");
    let mut h = HeadlessTermHarness::new(session);
    h.set_viewport_size(960.0, 540.0);
    h.focus_terminal();
    h.inject_text("whoami");
    let f1 = h.run_frame();
    let expected_dims = (f1.terminal_response.cols, f1.terminal_response.rows);
    assert_eq!(h.runtime.input().state(), WebPtyState::Connecting);

    // Transition to Open -> emits coalesced initial resize matching viewport + queued input.
    let open_frames = h.runtime.input_mut().mark_open();
    assert_eq!(h.runtime.input().state(), WebPtyState::Open);
    assert!(
        open_frames.len() >= 2,
        "expected handshake resize + queued input, got {open_frames:?}"
    );
    let first_decoded = WebPtyProtocol::TaggedBinary
        .decode_client_message(&open_frames[0])
        .expect("decode handshake");
    assert!(matches!(
        first_decoded,
        Some(OwnedClientFrame::Resize(sz)) if (sz.cols, sz.rows) == expected_dims
    ));
    let second_decoded = WebPtyProtocol::TaggedBinary
        .decode_client_message(&open_frames[1])
        .expect("decode queued input");
    assert_eq!(
        second_decoded,
        Some(OwnedClientFrame::Input(b"whoami".to_vec()))
    );

    // Server streams output, exit code, and an abnormal close.
    let out_frame = WebPtyProtocol::TaggedBinary
        .encode_server_frame(&ServerFrame::Output(b"alice\r\n".to_vec()))
        .expect("encode output");
    let exit_frame = WebPtyProtocol::TaggedBinary
        .encode_server_frame(&ServerFrame::Exit(13))
        .expect("encode exit");
    h.runtime.input_mut().push_incoming(out_frame);
    h.runtime.input_mut().push_incoming(exit_frame);
    h.runtime.input_mut().mark_closed(1006, "", false);

    let f2 = h.run_frame();
    assert!(h.runtime.terminal().full_text().contains("alice"));
    assert_eq!(
        h.runtime.input_mut().try_wait().expect("try_wait"),
        Some(13)
    );
    assert!(
        f2.terminal_response
            .events
            .iter()
            .any(|e| matches!(e, TerminalEvent::IoError(msg) if msg.contains("1006"))),
        "expected abnormal close error event, got {:?}",
        f2.terminal_response.events
    );
    assert!(
        h.runtime
            .last_io_error()
            .is_some_and(|e| e.to_string().contains("1006"))
    );
    assert!(h.runtime.input().is_closed());

    // 2. Ttyd: auth token escaping, sanitized OSC 0 title injection defense, ignored preferences.
    let ttyd_cfg = WebPtyConfig::new("ws://127.0.0.1:7681/ws")
        .with_protocol(WebPtyProtocol::Ttyd)
        .with_auth_token("tok\"1\\2");
    let mut h_ttyd = HeadlessTermHarness::new(WebPtySession::new(ttyd_cfg).expect("ttyd session"));
    let _ = h_ttyd.run_frame();
    let ttyd_open = h_ttyd.runtime.input_mut().mark_open();
    assert_eq!(ttyd_open.len(), 1);
    let WireMessage::Text(ref handshake_json) = ttyd_open[0] else {
        panic!("expected text ttyd handshake");
    };
    assert!(handshake_json.contains(r#"\"1\\2"#));

    h_ttyd
        .runtime
        .input_mut()
        .push_incoming(WireMessage::Binary(
            b"1web-shell\x1b]52;c;evil\x07".to_vec(),
        ));
    h_ttyd
        .runtime
        .input_mut()
        .push_incoming(WireMessage::Binary(b"2{\"theme\":\"dark\"}".to_vec()));
    h_ttyd
        .runtime
        .input_mut()
        .push_incoming(WireMessage::Binary(b"0ready$ ".to_vec()));
    let f_ttyd = h_ttyd.run_frame();
    assert!(h_ttyd.runtime.terminal().full_text().contains("ready$"));
    assert!(!h_ttyd.runtime.terminal().full_text().contains("theme"));
    assert_eq!(h_ttyd.runtime.terminal().title(), "web-shell]52;c;evil");
    assert!(
        !f_ttyd
            .terminal_response
            .events
            .iter()
            .any(|e| matches!(e, TerminalEvent::ClipboardWrite(_))),
        "sanitized ttyd title must never inject OSC 52"
    );

    // 3. JsonResize: viewport resize emits JSON resize frame with pixel dimensions.
    let json_cfg =
        WebPtyConfig::new("wss://pty.example.com/json").with_protocol(WebPtyProtocol::JsonResize);
    let mut h_json = HeadlessTermHarness::new(WebPtySession::new(json_cfg).expect("json session"));
    h_json.set_viewport_size(640.0, 400.0);
    let _ = h_json.run_frame();
    let _ = h_json.runtime.input_mut().mark_open();

    h_json.set_viewport_size(1024.0, 600.0);
    let _ = h_json.run_frame();
    h_json.advance_time(0.25);
    let _ = h_json.run_frame();
    let json_out = h_json.runtime.input_mut().drain_outgoing();
    assert!(
        json_out.iter().any(|msg| matches!(
            WebPtyProtocol::JsonResize.decode_client_message(msg),
            Ok(Some(OwnedClientFrame::Resize(sz))) if sz.pixel_width > 0 && sz.pixel_height > 0
        )),
        "expected pixel-aware JsonResize frame, got {json_out:?}"
    );

    // 4. URL fragment rejection (RFC 6455) and ttyd title overflow must not wedge VT parser.
    assert!(egui_xterm::pty::validate_ws_url("wss://pty.example.com/ws#frag").is_err());
    let overflow_cfg = WebPtyConfig::new("ws://127.0.0.1:7681/ws")
        .with_protocol(WebPtyProtocol::Ttyd)
        .with_max_rx_buffer_bytes(4096);
    let mut h_ov =
        HeadlessTermHarness::new(WebPtySession::new(overflow_cfg).expect("valid overflow session"));
    let _ = h_ov.runtime.input_mut().mark_open();
    let mut filler = vec![b'0'];
    filler.extend(std::iter::repeat_n(b'a', 4093));
    h_ov.runtime
        .input_mut()
        .push_incoming(WireMessage::Binary(filler));
    h_ov.runtime
        .input_mut()
        .push_incoming(WireMessage::Binary(b"1dropped-title".to_vec()));
    let _ = h_ov.run_frame();
    h_ov.runtime.input_mut().push_incoming(WireMessage::Binary(
        b"0\r\nVISIBLE_AFTER_OVERFLOW\r\n".to_vec(),
    ));
    let _ = h_ov.run_frame();
    assert!(
        h_ov.runtime
            .terminal()
            .full_text()
            .contains("VISIBLE_AFTER_OVERFLOW"),
        "dropped title overflow must never leave an unterminated OSC swallowing output"
    );
}

#[cfg(all(feature = "pty", not(target_arch = "wasm32")))]
#[test]
fn desktop_pty_bridged_over_tagged_binary_web_pty_protocol_roundtrip() {
    use std::time::{Duration, Instant};

    use egui::{Key, Modifiers};
    use egui_xterm::pty::{
        OwnedClientFrame, PtyBackend, PtyConfig, ServerFrame, WebPtyConfig, WebPtyProtocol,
        WebPtySession,
    };
    use egui_xterm::{TerminalInput, TerminalSize};

    let proto = WebPtyProtocol::TaggedBinary;
    let mut server_pty = PtyBackend::spawn(PtyConfig {
        initial_size: TerminalSize::new(80, 24),
        ..PtyConfig::default()
    })
    .expect("spawn server-side desktop PTY");

    let client_session = WebPtySession::new(
        WebPtyConfig::new("wss://localhost:8443/pty")
            .with_protocol(proto)
            .with_initial_size(TerminalSize::new(80, 24)),
    )
    .expect("create WebPtySession");
    let mut h = HeadlessTermHarness::new(client_session);
    h.set_viewport_size(800.0, 480.0);
    h.focus_terminal();

    let pump_bridge = |harness: &mut HeadlessTermHarness<WebPtySession>,
                       server: &mut PtyBackend| {
        // Client -> Server
        for wire in harness.runtime.input_mut().drain_outgoing() {
            match proto
                .decode_client_message(&wire)
                .expect("valid client wire frame")
            {
                Some(OwnedClientFrame::Input(bytes)) => {
                    if let Err(err) = server.write_bytes(&bytes) {
                        assert!(err.is_closed(), "unexpected server write error: {err}");
                    }
                }
                Some(OwnedClientFrame::Resize(size)) => {
                    if let Err(err) = server.resize(size) {
                        assert!(err.is_closed(), "unexpected server resize error: {err}");
                    }
                }
                _ => {}
            }
        }
        // Server -> Client
        let mut buf = [0u8; 4096];
        loop {
            match server.poll_read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Some(frame) =
                        proto.encode_server_frame(&ServerFrame::Output(buf[..n].to_vec()))
                    {
                        harness.runtime.input_mut().push_incoming(frame);
                    }
                }
                Err(err) if err.is_closed() => {
                    if let Ok(Some(code)) = server.try_wait()
                        && let Some(exit_frame) =
                            proto.encode_server_frame(&ServerFrame::Exit(code))
                    {
                        harness.runtime.input_mut().push_incoming(exit_frame);
                    }
                    harness.runtime.input_mut().mark_closed(1000, "eof", true);
                    break;
                }
                Err(err) => {
                    if let Some(err_frame) =
                        proto.encode_server_frame(&ServerFrame::Error(err.to_string()))
                    {
                        harness.runtime.input_mut().push_incoming(err_frame);
                    }
                    break;
                }
            }
        }
    };

    // Perform handshake open and forward initial resize to desktop PtyBackend.
    let open_frames = h.runtime.input_mut().mark_open();
    for wire in open_frames {
        if let Some(OwnedClientFrame::Resize(size)) = proto
            .decode_client_message(&wire)
            .expect("decode open handshake")
        {
            server_pty.resize(size).expect("apply initial resize");
        }
    }

    // Type command into TerminalRuntime -> WebPtySession -> TaggedBinary -> real OS PTY.
    h.inject_text("echo WEB_PTY_BRIDGE_OK");
    h.inject_key(Key::Enter, true, Modifiers::NONE);
    let _ = h.run_frame();
    pump_bridge(&mut h, &mut server_pty);

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut saw_marker = false;
    while Instant::now() < deadline {
        pump_bridge(&mut h, &mut server_pty);
        let _ = h.run_frame();
        if h.runtime
            .terminal()
            .full_text()
            .contains("WEB_PTY_BRIDGE_OK")
        {
            saw_marker = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(
        saw_marker,
        "expected WEB_PTY_BRIDGE_OK through WebPty protocol bridge; screen:\n{}",
        h.runtime.terminal().full_text()
    );

    // Exit the shell cleanly and verify Exit(0) + clean EOF propagation.
    h.inject_text("exit");
    h.inject_key(Key::Enter, true, Modifiers::NONE);
    let _ = h.run_frame();

    let exit_deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < exit_deadline && !h.runtime.input().is_closed() {
        pump_bridge(&mut h, &mut server_pty);
        let _ = h.run_frame();
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(
        h.runtime.input().is_closed(),
        "WebPtySession should close cleanly after remote shell exits"
    );
    assert_eq!(h.runtime.input_mut().try_wait().expect("try_wait"), Some(0));
    assert!(h.runtime.last_io_error().is_none());
}
