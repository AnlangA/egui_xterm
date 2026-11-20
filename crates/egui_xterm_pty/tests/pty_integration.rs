//! Integration tests for [`egui_xterm_pty::PtyBackend`] against live OS PTYs.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use egui_xterm_pty::{PtyBackend, PtyConfig, TerminalInput, TerminalIoError, TerminalSize};

fn poll_until(
    backend: &mut PtyBackend,
    timeout: Duration,
    mut pred: impl FnMut(&str, &PtyBackend) -> bool,
) -> (String, bool) {
    let deadline = Instant::now() + timeout;
    let mut captured = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match backend.poll_read(&mut buf) {
            Ok(0) | Err(TerminalIoError::Closed) => {}
            Ok(n) => captured.extend_from_slice(&buf[..n]),
            Err(err) => panic!("unexpected PTY poll_read error: {err}"),
        }
        let text = String::from_utf8_lossy(&captured).into_owned();
        if pred(&text, backend) {
            return (text, true);
        }
        if Instant::now() >= deadline {
            return (text, false);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn os_command_roundtrip_and_drain_before_closed() {
    #[cfg(windows)]
    let (shell, args) = (
        "cmd".to_owned(),
        vec!["/C".to_owned(), "echo PTY_INT_OK && exit 0".to_owned()],
    );
    #[cfg(not(windows))]
    let (shell, args) = (
        "sh".to_owned(),
        vec!["-c".to_owned(), "echo PTY_INT_OK; exit 0".to_owned()],
    );

    let mut backend = PtyBackend::spawn(PtyConfig {
        shell: Some(shell),
        args,
        cwd: None,
        env: Vec::new(),
        initial_size: TerminalSize::new(80, 24),
    })
    .expect("spawn OS command");

    let (text, found) = poll_until(&mut backend, Duration::from_secs(6), |s, b| {
        s.contains("PTY_INT_OK") && b.is_closed()
    });
    assert!(found, "expected marker + close, got: {text:?}");

    let mut tail = [0u8; 64];
    assert_eq!(backend.poll_read(&mut tail), Err(TerminalIoError::Closed));
    assert_eq!(backend.poll_read(&mut []), Err(TerminalIoError::Closed));
    assert_eq!(
        backend.write_bytes(b"echo ignored\r"),
        Err(TerminalIoError::Closed)
    );
    assert_eq!(backend.try_wait().expect("try_wait after exit"), Some(0));
}

#[test]
fn interactive_shell_roundtrip_and_exit() {
    let mut backend = PtyBackend::spawn(PtyConfig::default()).expect("spawn default shell");
    backend
        .write_bytes(b"echo INTERACTIVE_XYZ\r")
        .expect("write echo");
    let (text, found) = poll_until(&mut backend, Duration::from_secs(6), |s, _| {
        s.contains("INTERACTIVE_XYZ")
    });
    assert!(found, "interactive echo missing: {text:?}");

    backend.write_bytes(b"exit\r").expect("write exit");
    let (_, closed) = poll_until(&mut backend, Duration::from_secs(6), |_, b| b.is_closed());
    assert!(closed, "interactive shell did not close after exit");
}

#[test]
fn env_defaults_and_user_overrides() {
    #[cfg(windows)]
    let (shell, args) = (
        "cmd".to_owned(),
        vec![
            "/C".to_owned(),
            "echo %TERM% %COLORTERM% %EGUI_XTERM_OVERRIDE%".to_owned(),
        ],
    );
    #[cfg(not(windows))]
    let (shell, args) = (
        "sh".to_owned(),
        vec![
            "-c".to_owned(),
            "echo $TERM $COLORTERM $EGUI_XTERM_OVERRIDE".to_owned(),
        ],
    );

    let mut backend = PtyBackend::spawn(PtyConfig {
        shell: Some(shell),
        args,
        cwd: None,
        env: vec![
            ("COLORTERM".to_owned(), "custom-color".to_owned()),
            ("EGUI_XTERM_OVERRIDE".to_owned(), "OVERRIDE_99".to_owned()),
        ],
        initial_size: TerminalSize::new(120, 24),
    })
    .expect("spawn env probe");

    let (text, found) = poll_until(&mut backend, Duration::from_secs(6), |s, _| {
        s.contains("xterm-256color") && s.contains("custom-color") && s.contains("OVERRIDE_99")
    });
    assert!(found, "missing expected env markers in {text:?}");
}

#[test]
fn cwd_valid_and_missing_behavior() {
    let tmp_dir = std::env::temp_dir().canonicalize().expect("canonical temp");
    let folder = tmp_dir
        .file_name()
        .and_then(|n| n.to_str())
        .expect("temp folder name");

    #[cfg(windows)]
    let (shell, args) = ("cmd".to_owned(), vec!["/C".to_owned(), "cd".to_owned()]);
    #[cfg(not(windows))]
    let (shell, args) = ("sh".to_owned(), vec!["-c".to_owned(), "pwd".to_owned()]);

    let mut backend = PtyBackend::spawn(PtyConfig {
        shell: Some(shell),
        args,
        cwd: Some(tmp_dir.clone()),
        env: Vec::new(),
        initial_size: TerminalSize::new(120, 24),
    })
    .expect("spawn with valid cwd");

    let (text, found) = poll_until(&mut backend, Duration::from_secs(6), |s, _| {
        s.to_ascii_lowercase()
            .contains(&folder.to_ascii_lowercase())
    });
    assert!(found, "expected cwd folder {folder:?} in {text:?}");

    let missing = std::env::temp_dir().join("egui_xterm_missing_dir_integration_98765");
    let err = PtyBackend::spawn(PtyConfig {
        cwd: Some(missing),
        ..PtyConfig::default()
    })
    .expect_err("missing cwd must return Err");
    assert!(!err.to_string().is_empty());
}

#[test]
fn invalid_shell_returns_error() {
    let err = PtyBackend::spawn(PtyConfig {
        shell: Some("egui_xterm_definitely_missing_binary_000".to_owned()),
        ..PtyConfig::default()
    })
    .expect_err("invalid binary must fail");
    assert!(!err.to_string().is_empty());
}

#[test]
fn resize_clamping_dedup_and_live_propagation() {
    #[cfg(windows)]
    let (shell, args) = (
        "powershell.exe".to_owned(),
        vec!["-NoLogo".to_owned(), "-NoProfile".to_owned()],
    );
    #[cfg(not(windows))]
    let (shell, args) = ("sh".to_owned(), Vec::new());

    let mut backend = PtyBackend::spawn(PtyConfig {
        shell: Some(shell),
        args,
        cwd: None,
        env: Vec::new(),
        initial_size: TerminalSize::new(0, 0),
    })
    .expect("spawn shell with clamped 0x0 initial size");

    assert_eq!(backend.current_size(), TerminalSize::new(1, 1));

    // Resize to 80x24, then repeat identical call (deduplicated).
    backend
        .resize(TerminalSize::new(80, 24))
        .expect("resize 80x24");
    backend
        .resize(TerminalSize::new(80, 24))
        .expect("deduplicated resize");
    assert_eq!(backend.current_size(), TerminalSize::new(80, 24));

    #[cfg(windows)]
    {
        let (boot, ready) = poll_until(&mut backend, Duration::from_secs(6), |s, _| {
            s.contains("PS ") || s.contains('>')
        });
        assert!(ready, "powershell prompt not ready: {boot:?}");
    }

    backend
        .resize(TerminalSize::with_pixels(102, 34, 816, 680))
        .expect("resize 102x34");
    assert_eq!(
        backend.current_size(),
        TerminalSize::with_pixels(102, 34, 816, 680)
    );

    #[cfg(windows)]
    let (probe, expected) = (
        b"([string][Console]::WindowWidth + 'x' + [Console]::WindowHeight)\r".as_slice(),
        "102x34",
    );
    #[cfg(not(windows))]
    let (probe, expected) = (b"stty size\r".as_slice(), "34 102");

    backend.write_bytes(probe).expect("write size probe");
    let (text, found) = poll_until(&mut backend, Duration::from_secs(8), |s, _| {
        s.contains(expected)
    });
    let _ = backend.write_bytes(b"exit\r");
    assert!(found, "expected {expected:?} in {text:?}");
}

#[test]
fn repaint_callback_and_empty_buf_poll_read() {
    let repaints = Arc::new(AtomicUsize::new(0));
    let cb_count = Arc::clone(&repaints);

    #[cfg(windows)]
    let (shell, args) = (
        "cmd".to_owned(),
        vec!["/C".to_owned(), "echo REPAINT_PROBE && exit 0".to_owned()],
    );
    #[cfg(not(windows))]
    let (shell, args) = (
        "sh".to_owned(),
        vec!["-c".to_owned(), "echo REPAINT_PROBE; exit 0".to_owned()],
    );

    let mut backend = PtyBackend::spawn_with_repaint(
        PtyConfig {
            shell: Some(shell),
            args,
            ..PtyConfig::default()
        },
        Box::new(move || {
            cb_count.fetch_add(1, Ordering::Relaxed);
        }),
    )
    .expect("spawn with repaint");

    let start = Instant::now();
    let empty_res = backend.poll_read(&mut []);
    assert!(start.elapsed() < Duration::from_millis(100));
    assert!(matches!(empty_res, Ok(0) | Err(TerminalIoError::Closed)));

    let (_, closed) = poll_until(&mut backend, Duration::from_secs(6), |_, b| b.is_closed());
    assert!(closed, "expected close");
    assert!(repaints.load(Ordering::Relaxed) >= 1);

    backend.set_repaint_callback(None);
}

#[test]
fn drop_with_grandchild_completes_quickly() {
    #[cfg(windows)]
    let (shell, args) = (
        "cmd".to_owned(),
        vec!["/C".to_owned(), "ping -n 60 127.0.0.1 > nul".to_owned()],
    );
    #[cfg(not(windows))]
    let (shell, args) = (
        "sh".to_owned(),
        vec!["-c".to_owned(), "sleep 60".to_owned()],
    );

    let backend = PtyBackend::spawn(PtyConfig {
        shell: Some(shell),
        args,
        ..PtyConfig::default()
    })
    .expect("spawn sleeping grandchild");

    let start = Instant::now();
    drop(backend);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "PtyBackend::drop took too long: {:?}",
        start.elapsed()
    );
}

#[test]
fn extreme_resize_does_not_kill_live_child() {
    #[cfg(windows)]
    let (shell, args) = (
        "cmd".to_owned(),
        vec![
            "/C".to_owned(),
            "echo RESIZE_START & ping -n 2 127.0.0.1 > nul & echo RESIZE_EXTREME_DONE & exit 0"
                .to_owned(),
        ],
    );
    #[cfg(not(windows))]
    let (shell, args) = (
        "sh".to_owned(),
        vec![
            "-c".to_owned(),
            "echo RESIZE_START; sleep 0.3; echo RESIZE_EXTREME_DONE; exit 0".to_owned(),
        ],
    );

    let mut backend = PtyBackend::spawn(PtyConfig {
        shell: Some(shell),
        args,
        cwd: None,
        env: Vec::new(),
        initial_size: TerminalSize::new(80, 24),
    })
    .expect("spawn live child for extreme resize regression");

    let (boot, started) = poll_until(&mut backend, Duration::from_secs(6), |s, _| {
        s.contains("RESIZE_START")
    });
    assert!(started, "child did not emit RESIZE_START: {boot:?}");

    for extreme in [
        TerminalSize::new(u16::MAX, u16::MAX),
        TerminalSize::new(32767, 24),
        TerminalSize::new(80, 32767),
    ] {
        backend
            .resize(extreme)
            .unwrap_or_else(|err| panic!("resize({extreme:?}) failed: {err}"));
        let clamped = backend.current_size();
        assert!(
            (1..32767).contains(&clamped.cols) && (1..32767).contains(&clamped.rows),
            "clamped size {clamped:?} must stay below ConPTY crash threshold"
        );
    }

    let (tail, done) = poll_until(&mut backend, Duration::from_secs(6), |s, b| {
        s.contains("RESIZE_EXTREME_DONE") && b.is_closed()
    });
    assert!(
        done,
        "child did not complete cleanly after extreme resize: {tail:?}"
    );
    assert_eq!(
        backend.try_wait().expect("try_wait after extreme resize"),
        Some(0),
        "live child must exit with status 0 after extreme resize"
    );
}
