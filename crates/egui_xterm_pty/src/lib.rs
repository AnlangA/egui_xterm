//! Cross-platform PTY backends for `egui_xterm` implementing [`TerminalInput`].
//!
//! - On native desktop targets (`Windows`, `macOS`, `Linux`), [`PtyBackend`] spawns local OS
//!   shells via `portable-pty` with recycled read/write buffer pools and a non-blocking
//!   background writer thread.
//! - On `wasm32-unknown-unknown`, `WebPtyBackend` connects over browser `WebSocket`s to a
//!   remote PTY gateway (supporting [`WebPtyProtocol::TaggedBinary`], [`WebPtyProtocol::Ttyd`],
//!   [`WebPtyProtocol::JsonResize`], and [`WebPtyProtocol::Raw`]), while compile-safe
//!   [`PtyBackend`] / [`PtyConfig`] stubs remain available for cross-platform callers.
//! - On **all targets**, [`protocol`] and [`WebPtySession`] provide clean-room framing codecs
//!   and a transport-agnostic session state machine for building custom Web PTY bridges or
//!   server gateways.
//!
//! # Examples
//!
//! Spawning a shell inside a native PTY (`no_run` because CI may lack a TTY):
//!
//! ```no_run
//! use egui_xterm_pty::{PtyBackend, PtyConfig, TerminalInput, TerminalIoError, TerminalSize};
//!
//! let config = PtyConfig {
//!     initial_size: TerminalSize::new(100, 30),
//!     ..PtyConfig::default()
//! };
//! let mut pty = PtyBackend::spawn(config)?;
//! pty.write_bytes(b"echo hello\r")?;
//!
//! let mut buf = [0u8; 1024];
//! let _read = pty.poll_read(&mut buf)?;
//! # Ok::<(), TerminalIoError>(())
//! ```

#![forbid(unsafe_code)]

pub use egui_xterm_core::{TerminalInput, TerminalIoError, TerminalSize};

/// Callback invoked when PTY output arrives, connection state changes, or EOF occurs.
pub type RepaintCallback = Box<dyn Fn() + Send + Sync>;

pub mod protocol;
mod web;

pub use protocol::{
    ClientFrame, OwnedClientFrame, ServerFrame, WebPtyProtocol, WireMessage, validate_ws_url,
};
#[cfg(target_arch = "wasm32")]
pub use web::WebPtyBackend;
pub use web::{
    DEFAULT_MAX_PENDING_TX_BYTES, DEFAULT_MAX_RX_BUFFER_BYTES, WebPtyConfig, WebPtySession,
    WebPtyState,
};

#[cfg(not(target_arch = "wasm32"))]
pub mod shell;

#[cfg(any(target_arch = "wasm32", test))]
mod wasm_stub {
    use std::path::PathBuf;

    use egui_xterm_core::{TerminalInput, TerminalIoError, TerminalSize};

    pub use crate::RepaintCallback;

    /// Configuration for spawning a child process attached to a native PTY via [`PtyBackend`].
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct PtyConfig {
        /// Executable or shell command to launch; `None` uses the platform default shell.
        pub shell: Option<String>,
        /// Arguments passed to the launched executable.
        pub args: Vec<String>,
        /// Optional working directory for the child process (validated to exist as a directory).
        pub cwd: Option<PathBuf>,
        /// Environment variable overrides `(key, value)` applied after `TERM`/`COLORTERM` defaults.
        pub env: Vec<(String, String)>,
        /// Initial character-grid and pixel dimensions for the PTY.
        pub initial_size: TerminalSize,
    }

    /// Compile-safe `wasm32` stub for [`PtyBackend`] whose operations return transport errors.
    #[derive(Debug)]
    pub struct PtyBackend {
        last_size: TerminalSize,
    }

    const WASM_UNSUPPORTED_MSG: &str = "local OS PTY is unavailable on wasm32 targets; use WebPtyBackend::connect with a WebSocket PTY endpoint";

    fn wasm_unsupported_error() -> TerminalIoError {
        TerminalIoError::message(WASM_UNSUPPORTED_MSG)
    }

    impl PtyBackend {
        /// Attempts to spawn a native PTY process, always returning an error on `wasm32`.
        ///
        /// # Errors
        ///
        /// Always returns [`TerminalIoError::Message`] on `wasm32` targets.
        pub fn spawn(_config: PtyConfig) -> Result<Self, TerminalIoError> {
            Err(wasm_unsupported_error())
        }

        /// Attempts to spawn a native PTY process with a repaint callback, always returning an
        /// error on `wasm32`.
        ///
        /// # Errors
        ///
        /// Always returns [`TerminalIoError::Message`] on `wasm32` targets.
        pub fn spawn_with_repaint(
            _config: PtyConfig,
            _repaint: RepaintCallback,
        ) -> Result<Self, TerminalIoError> {
            Err(wasm_unsupported_error())
        }

        /// Replaces or clears the repaint callback (no-op on `wasm32`).
        pub fn set_repaint_callback(&self, _repaint: Option<RepaintCallback>) {}

        /// Returns the stored PTY size (`1x1` minimum when constructed for tests).
        #[must_use]
        pub const fn current_size(&self) -> TerminalSize {
            self.last_size
        }

        /// Checks child exit status, always returning an error on `wasm32`.
        ///
        /// # Errors
        ///
        /// Always returns [`TerminalIoError::Message`] on `wasm32` targets.
        pub fn try_wait(&mut self) -> Result<Option<u32>, TerminalIoError> {
            Err(wasm_unsupported_error())
        }

        #[cfg(test)]
        pub(crate) const fn new_for_test(initial_size: TerminalSize) -> Self {
            Self {
                last_size: initial_size,
            }
        }
    }

    impl TerminalInput for PtyBackend {
        fn poll_read(&mut self, _buf: &mut [u8]) -> Result<usize, TerminalIoError> {
            Err(wasm_unsupported_error())
        }

        fn write_bytes(&mut self, _bytes: &[u8]) -> Result<(), TerminalIoError> {
            Err(wasm_unsupported_error())
        }

        fn resize(&mut self, _size: TerminalSize) -> Result<(), TerminalIoError> {
            Err(wasm_unsupported_error())
        }

        fn is_closed(&self) -> bool {
            true
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn wasm_stub_returns_errors_and_reports_closed() {
            let cfg = PtyConfig {
                shell: Some("sh".to_owned()),
                args: vec!["-c".to_owned(), "echo hi".to_owned()],
                cwd: None,
                env: vec![("TERM".to_owned(), "xterm-256color".to_owned())],
                initial_size: TerminalSize::new(100, 40),
            };
            assert!(matches!(
                PtyBackend::spawn(cfg.clone()),
                Err(TerminalIoError::Message(_))
            ));
            assert!(matches!(
                PtyBackend::spawn_with_repaint(cfg, Box::new(|| {})),
                Err(TerminalIoError::Message(_))
            ));

            let mut stub = PtyBackend::new_for_test(TerminalSize::new(80, 24));
            assert_eq!(stub.current_size(), TerminalSize::new(80, 24));
            stub.set_repaint_callback(Some(Box::new(|| {})));
            stub.set_repaint_callback(None);
            assert!(stub.is_closed());
            assert!(matches!(stub.try_wait(), Err(TerminalIoError::Message(_))));
            let mut buf = [0u8; 16];
            assert!(matches!(
                stub.poll_read(&mut buf),
                Err(TerminalIoError::Message(_))
            ));
            assert!(matches!(
                stub.write_bytes(b"hi"),
                Err(TerminalIoError::Message(_))
            ));
            assert!(matches!(
                stub.resize(TerminalSize::new(90, 30)),
                Err(TerminalIoError::Message(_))
            ));
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm_stub::{PtyBackend, PtyConfig};

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::fmt;
    use std::io::{ErrorKind, Read, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use egui_xterm_core::{TerminalInput, TerminalIoError, TerminalSize};
    #[cfg(any(windows, test))]
    use portable_pty::ChildKiller;
    use portable_pty::{Child, MasterPty, PtySize, native_pty_system};

    use crate::shell;

    /// Bounded number of chunk messages buffered between the PTY reader thread and UI thread.
    const READER_CHANNEL_CAPACITY: usize = 256;
    /// Read buffer size per OS pipe read on the background PTY reader thread.
    const READER_CHUNK_BYTES: usize = 16 * 1024;
    /// Bounded number of recycled read buffers kept ready for reuse by the reader thread.
    const RECYCLE_POOL_CAPACITY: usize = 64;
    /// Maximum buffer capacity eligible to re-enter the read buffer pool.
    const MAX_RECYCLED_CHUNK_BYTES: usize = READER_CHUNK_BYTES * 2;
    /// Bounded number of outgoing write chunks buffered between the UI thread and writer thread.
    const WRITER_CHANNEL_CAPACITY: usize = 256;
    /// Bounded number of recycled outgoing write buffers kept ready for reuse by `write_bytes`.
    const WRITER_RECYCLE_CAPACITY: usize = 64;
    /// Maximum time [`PtyBackend::drop`] waits for background threads to finish before detaching.
    const DROP_JOIN_WAIT: Duration = Duration::from_millis(50);
    /// Safe upper bound for PTY columns and rows.
    ///
    /// Windows `ConPTY` crashes the pseudoconsole (`0xC000013A`) on `ResizePseudoConsole`
    /// when either dimension reaches `i16::MAX` (`32767`); `4096` stays well below that
    /// threshold while accommodating any practical terminal viewport.
    const MAX_PTY_DIM: u16 = 4096;

    pub use crate::RepaintCallback;

    /// Configuration for spawning a child process attached to a native PTY via [`PtyBackend`].
    ///
    /// # Examples
    ///
    /// ```
    /// use egui_xterm_pty::{PtyConfig, TerminalSize, shell};
    ///
    /// let cfg = PtyConfig {
    ///     shell: Some(shell::default_shell()),
    ///     args: vec!["-l".to_owned()],
    ///     env: vec![("MY_VAR".to_owned(), "1".to_owned())],
    ///     initial_size: TerminalSize::with_pixels(120, 40, 960, 640),
    ///     ..PtyConfig::default()
    /// };
    /// assert_eq!(cfg.initial_size.cols, 120);
    /// assert!(cfg.shell.as_deref().is_some_and(|s| !s.is_empty()));
    /// ```
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct PtyConfig {
        /// Executable or shell command to launch; `None` uses the platform default shell.
        pub shell: Option<String>,
        /// Arguments passed to the launched executable.
        pub args: Vec<String>,
        /// Optional working directory for the child process (validated to exist as a directory).
        pub cwd: Option<PathBuf>,
        /// Environment variable overrides `(key, value)` applied after `TERM`/`COLORTERM` defaults.
        pub env: Vec<(String, String)>,
        /// Initial character-grid and pixel dimensions for the PTY.
        pub initial_size: TerminalSize,
    }

    #[derive(Debug)]
    enum ReaderEvent {
        Data { buf: Vec<u8>, len: usize },
        Error(String),
        Eof,
    }

    enum WriterCommand {
        Data(Vec<u8>),
        Close,
    }

    #[derive(Debug, Default)]
    struct WriterSharedState {
        closed: AtomicBool,
        error: Mutex<Option<TerminalIoError>>,
    }

    struct DoneSignal {
        state: Arc<(Mutex<bool>, Condvar)>,
    }

    impl Drop for DoneSignal {
        fn drop(&mut self) {
            let (lock, cvar) = &*self.state;
            let mut done = match lock.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            *done = true;
            cvar.notify_all();
        }
    }

    #[derive(Clone, Copy)]
    enum ExitMonitorMode {
        #[cfg(any(not(windows), test))]
        DirectOnEof,
        #[cfg(any(windows, test))]
        WaiterThread,
    }

    impl ExitMonitorMode {
        const fn platform_default() -> Self {
            #[cfg(windows)]
            {
                Self::WaiterThread
            }
            #[cfg(not(windows))]
            {
                Self::DirectOnEof
            }
        }
    }

    enum ChildControl {
        #[cfg(any(not(windows), test))]
        Direct(Option<Box<dyn Child + Send + Sync>>),
        #[cfg(any(windows, test))]
        Waiter {
            killer: Option<Box<dyn ChildKiller + Send + Sync>>,
            shared_exit: Arc<Mutex<Option<Result<u32, String>>>>,
            waiter_done: Arc<(Mutex<bool>, Condvar)>,
        },
    }

    impl ChildControl {
        fn kill_best_effort(&mut self) {
            match self {
                #[cfg(any(not(windows), test))]
                Self::Direct(child_opt) => {
                    if let Some(mut child) = child_opt.take()
                        && matches!(child.try_wait(), Ok(None))
                    {
                        if child.kill().is_err() {
                            // Best-effort termination during teardown; child may have just exited.
                        }
                        if child.try_wait().is_err() {
                            // Best-effort reap during teardown.
                        }
                    }
                }
                #[cfg(any(windows, test))]
                Self::Waiter { killer, .. } => {
                    if let Some(mut k) = killer.take()
                        && k.kill().is_err()
                    {
                        // Best-effort termination during teardown; child may have just exited.
                    }
                }
            }
        }

        fn wait_done(&self, _deadline: Instant) {
            match self {
                #[cfg(any(not(windows), test))]
                Self::Direct(_) => {}
                #[cfg(any(windows, test))]
                Self::Waiter { waiter_done, .. } => {
                    wait_done(waiter_done, _deadline);
                }
            }
        }
    }

    #[cfg(test)]
    #[derive(Debug, Default)]
    struct TestPoolStats {
        read_allocs: std::sync::atomic::AtomicUsize,
        read_reuses: std::sync::atomic::AtomicUsize,
        recycled_returns: std::sync::atomic::AtomicUsize,
        recycled_drops: std::sync::atomic::AtomicUsize,
        seen_ptrs: Mutex<std::collections::HashSet<usize>>,
    }

    type SharedRepaint = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;
    type SharedMaster = Arc<Mutex<Option<Box<dyn PtyMaster>>>>;
    type SharedWriterTx = Arc<Mutex<Option<SyncSender<WriterCommand>>>>;

    struct TransportHandles {
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        child: Box<dyn Child + Send + Sync>,
        master: Box<dyn PtyMaster>,
    }

    /// Internal abstraction over [`portable_pty::MasterPty`] for benchmarks and deterministic tests.
    #[doc(hidden)]
    pub trait PtyMaster: Send {
        /// Resizes the PTY to `size`.
        ///
        /// # Errors
        ///
        /// Returns [`TerminalIoError`] if the underlying resize operation fails.
        fn resize_pty(&self, size: TerminalSize) -> Result<(), TerminalIoError>;
    }

    impl PtyMaster for Box<dyn MasterPty + Send> {
        fn resize_pty(&self, size: TerminalSize) -> Result<(), TerminalIoError> {
            self.resize(to_pty_size(size))
                .map_err(|err| TerminalIoError::message(format!("failed to resize PTY: {err}")))
        }
    }

    /// Native desktop PTY session implementing non-blocking [`TerminalInput`].
    ///
    /// A background reader thread forwards child output through a bounded recycled-buffer
    /// channel, and a dedicated background writer thread drains outgoing bytes without blocking
    /// the UI thread. [`TerminalInput::poll_read`] triggers an optional [`RepaintCallback`]
    /// whenever new bytes arrive or the stream reaches EOF.
    pub struct PtyBackend {
        master: SharedMaster,
        writer_tx: SharedWriterTx,
        writer_recycle_rx: Receiver<Vec<u8>>,
        writer_state: Arc<WriterSharedState>,
        child: ChildControl,
        rx: Option<Receiver<ReaderEvent>>,
        recycle_tx: SyncSender<Vec<u8>>,
        pending: Vec<u8>,
        pending_offset: usize,
        pending_len: usize,
        pending_error: Option<String>,
        closed: bool,
        exit_code: Option<u32>,
        child_wait_error: Option<TerminalIoError>,
        last_size: TerminalSize,
        repaint: SharedRepaint,
        shutdown: Arc<AtomicBool>,
        reader_done: Arc<(Mutex<bool>, Condvar)>,
        writer_done: Arc<(Mutex<bool>, Condvar)>,
        #[cfg(test)]
        test_stats: Arc<TestPoolStats>,
    }

    const _: fn() = || {
        const fn assert_send<T: Send>() {}
        assert_send::<PtyBackend>();
    };

    impl fmt::Debug for PtyBackend {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("PtyBackend")
                .field("last_size", &self.last_size)
                .field("closed", &self.closed)
                .field("exit_code", &self.exit_code)
                .field("pending_len", &self.pending_len())
                .finish_non_exhaustive()
        }
    }

    impl PtyBackend {
        /// Spawns a child process inside a newly allocated PTY without a repaint callback.
        ///
        /// # Errors
        ///
        /// Returns [`TerminalIoError::Message`] if `config.cwd` is not a directory, the PTY
        /// fails to open, or the child process cannot be spawned.
        ///
        /// # Examples
        ///
        /// Missing working directories are rejected before allocating an OS PTY:
        ///
        /// ```
        /// use egui_xterm_pty::{PtyBackend, PtyConfig, TerminalIoError};
        ///
        /// let missing = std::env::temp_dir().join("egui_xterm_pty_missing_doctest_dir");
        /// let cfg = PtyConfig {
        ///     cwd: Some(missing),
        ///     ..PtyConfig::default()
        /// };
        /// assert!(matches!(
        ///     PtyBackend::spawn(cfg),
        ///     Err(TerminalIoError::Message(_))
        /// ));
        /// ```
        pub fn spawn(config: PtyConfig) -> Result<Self, TerminalIoError> {
            Self::spawn_inner(config, None)
        }

        /// Spawns a child process inside a PTY and registers a [`RepaintCallback`] invoked
        /// from the background reader thread on output or EOF.
        ///
        /// # Errors
        ///
        /// Returns [`TerminalIoError::Message`] if `config.cwd` is not a directory, the PTY
        /// fails to open, or the child process cannot be spawned.
        pub fn spawn_with_repaint(
            config: PtyConfig,
            repaint: RepaintCallback,
        ) -> Result<Self, TerminalIoError> {
            Self::spawn_inner(config, Some(repaint))
        }

        /// Constructs a [`PtyBackend`] from pre-built transport trait objects.
        ///
        /// Intended for internal benchmarking and deterministic transport tests.
        ///
        /// # Errors
        ///
        /// Returns [`TerminalIoError::Message`] if spawning a background worker thread fails.
        #[doc(hidden)]
        pub fn from_parts(
            reader: Box<dyn Read + Send>,
            writer: Box<dyn Write + Send>,
            child: Box<dyn Child + Send + Sync>,
            master: Box<dyn PtyMaster>,
            initial_size: TerminalSize,
            repaint: Option<RepaintCallback>,
        ) -> Result<Self, TerminalIoError> {
            Self::from_parts_inner(
                TransportHandles {
                    reader,
                    writer,
                    child,
                    master,
                },
                clamp_size(initial_size),
                repaint,
                ExitMonitorMode::platform_default(),
                #[cfg(test)]
                Arc::new(TestPoolStats::default()),
            )
        }

        fn spawn_inner(
            config: PtyConfig,
            repaint: Option<RepaintCallback>,
        ) -> Result<Self, TerminalIoError> {
            let clamped = clamp_size(config.initial_size);
            let cmd = shell::build_command(config)?;
            let pty_system = native_pty_system();
            let pair = pty_system
                .openpty(to_pty_size(clamped))
                .map_err(|err| TerminalIoError::message(format!("failed to open PTY: {err}")))?;

            let reader = pair.master.try_clone_reader().map_err(|err| {
                TerminalIoError::message(format!("failed to clone PTY reader: {err}"))
            })?;
            let writer = pair.master.take_writer().map_err(|err| {
                TerminalIoError::message(format!("failed to take PTY writer: {err}"))
            })?;

            let child = pair.slave.spawn_command(cmd).map_err(|err| {
                TerminalIoError::message(format!("failed to spawn PTY command: {err}"))
            })?;
            // Immediately release the slave handle so parent EOF/ConPTY teardown works reliably.
            drop(pair.slave);

            Self::from_parts_inner(
                TransportHandles {
                    reader,
                    writer,
                    child,
                    master: Box::new(pair.master),
                },
                clamped,
                repaint,
                ExitMonitorMode::platform_default(),
                #[cfg(test)]
                Arc::new(TestPoolStats::default()),
            )
        }

        fn from_parts_inner(
            handles: TransportHandles,
            clamped: TerminalSize,
            repaint: Option<RepaintCallback>,
            exit_mode: ExitMonitorMode,
            #[cfg(test)] test_stats: Arc<TestPoolStats>,
        ) -> Result<Self, TerminalIoError> {
            let TransportHandles {
                reader,
                writer,
                mut child,
                master,
            } = handles;
            let (tx, rx) = mpsc::sync_channel::<ReaderEvent>(READER_CHANNEL_CAPACITY);
            let (recycle_tx, recycle_rx) = mpsc::sync_channel::<Vec<u8>>(RECYCLE_POOL_CAPACITY);
            let (writer_tx_raw, writer_rx) =
                mpsc::sync_channel::<WriterCommand>(WRITER_CHANNEL_CAPACITY);
            let (writer_recycle_tx, writer_recycle_rx) =
                mpsc::sync_channel::<Vec<u8>>(WRITER_RECYCLE_CAPACITY);

            let repaint_slot: SharedRepaint = Arc::new(Mutex::new(repaint.map(Arc::from)));
            let shared_master: SharedMaster = Arc::new(Mutex::new(Some(master)));
            let shared_writer_tx: SharedWriterTx =
                Arc::new(Mutex::new(Some(writer_tx_raw.clone())));
            let writer_state = Arc::new(WriterSharedState::default());
            let shutdown = Arc::new(AtomicBool::new(false));
            let reader_done = Arc::new((Mutex::new(false), Condvar::new()));
            let writer_done = Arc::new((Mutex::new(false), Condvar::new()));

            // Spawn background writer thread first.
            let w_state = Arc::clone(&writer_state);
            let w_done = Arc::clone(&writer_done);
            let w_shutdown = Arc::clone(&shutdown);
            if let Err(err) = thread::Builder::new()
                .name("egui_xterm_pty_writer".to_owned())
                .spawn(move || {
                    let _done_guard = DoneSignal { state: w_done };
                    writer_loop(writer, writer_rx, writer_recycle_tx, &w_state, &w_shutdown);
                })
            {
                cleanup_child_and_master(&mut *child, &shared_master);
                return Err(TerminalIoError::message(format!(
                    "failed to spawn PTY writer thread: {err}"
                )));
            }

            // Spawn background reader thread.
            let r_repaint = Arc::clone(&repaint_slot);
            let r_done = Arc::clone(&reader_done);
            let r_shutdown = Arc::clone(&shutdown);
            #[cfg(windows)]
            let r_cpr_tx = Some(writer_tx_raw);
            #[cfg(not(windows))]
            drop(writer_tx_raw);
            #[cfg(windows)]
            let r_writer_state = Arc::clone(&writer_state);
            #[cfg(test)]
            let r_stats = Arc::clone(&test_stats);

            if let Err(err) = thread::Builder::new()
                .name("egui_xterm_pty_reader".to_owned())
                .spawn(move || {
                    let _done_guard = DoneSignal { state: r_done };
                    reader_loop(
                        ReaderChannelContext {
                            reader,
                            tx,
                            recycle_rx,
                            repaint: r_repaint,
                            shutdown: r_shutdown,
                        },
                        #[cfg(windows)]
                        r_cpr_tx,
                        #[cfg(windows)]
                        &r_writer_state,
                        #[cfg(test)]
                        &r_stats,
                    );
                })
            {
                shutdown.store(true, Ordering::Release);
                close_writer_tx(&shared_writer_tx);
                cleanup_child_and_master(&mut *child, &shared_master);
                wait_done(&writer_done, Instant::now() + DROP_JOIN_WAIT);
                return Err(TerminalIoError::message(format!(
                    "failed to spawn PTY reader thread: {err}"
                )));
            }

            let child_control = match exit_mode {
                #[cfg(any(not(windows), test))]
                ExitMonitorMode::DirectOnEof => ChildControl::Direct(Some(child)),
                #[cfg(any(windows, test))]
                ExitMonitorMode::WaiterThread => {
                    let killer = child.clone_killer();
                    let shared_exit = Arc::new(Mutex::new(None));
                    let waiter_done = Arc::new((Mutex::new(false), Condvar::new()));

                    let w_exit = Arc::clone(&shared_exit);
                    let w_tx = Arc::clone(&shared_writer_tx);
                    let w_master = Arc::clone(&shared_master);
                    let w_repaint = Arc::clone(&repaint_slot);
                    let w_waiter_done = Arc::clone(&waiter_done);

                    // Keep a backup killer in case thread::Builder::spawn fails and consumes `child`.
                    let mut fallback_killer = killer.clone_killer();
                    if let Err(err) = thread::Builder::new()
                        .name("egui_xterm_pty_waiter".to_owned())
                        .spawn(move || {
                            let _done_guard = DoneSignal {
                                state: w_waiter_done,
                            };
                            waiter_loop(child, &w_exit, &w_tx, &w_master, &w_repaint);
                        })
                    {
                        shutdown.store(true, Ordering::Release);
                        drop(rx);
                        close_writer_tx(&shared_writer_tx);
                        if fallback_killer.kill().is_err() {
                            // Best-effort cleanup on waiter thread spawn failure.
                        }
                        take_and_drop_master(&shared_master);
                        let deadline = Instant::now() + DROP_JOIN_WAIT;
                        wait_done(&reader_done, deadline);
                        wait_done(&writer_done, deadline);
                        return Err(TerminalIoError::message(format!(
                            "failed to spawn PTY child waiter thread: {err}"
                        )));
                    }

                    ChildControl::Waiter {
                        killer: Some(killer),
                        shared_exit,
                        waiter_done,
                    }
                }
            };

            Ok(Self {
                master: shared_master,
                writer_tx: shared_writer_tx,
                writer_recycle_rx,
                writer_state,
                child: child_control,
                rx: Some(rx),
                recycle_tx,
                pending: Vec::new(),
                pending_offset: 0,
                pending_len: 0,
                pending_error: None,
                closed: false,
                exit_code: None,
                child_wait_error: None,
                last_size: clamped,
                repaint: repaint_slot,
                shutdown,
                reader_done,
                writer_done,
                #[cfg(test)]
                test_stats,
            })
        }

        /// Replaces or clears the repaint callback invoked when PTY output or EOF arrives.
        pub fn set_repaint_callback(&self, repaint: Option<RepaintCallback>) {
            let mut guard = match self.repaint.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            *guard = repaint.map(Arc::from);
        }

        /// Returns the last PTY grid and pixel dimensions applied during spawn or resize.
        #[must_use]
        pub const fn current_size(&self) -> TerminalSize {
            self.last_size
        }

        /// Non-blocking check for whether the child process has exited, returning its exit code.
        ///
        /// # Errors
        ///
        /// Returns [`TerminalIoError::Message`] if querying the child process status fails.
        pub fn try_wait(&mut self) -> Result<Option<u32>, TerminalIoError> {
            if let Some(code) = self.exit_code {
                return Ok(Some(code));
            }
            if let Some(err) = self.child_wait_error.take() {
                return Err(err);
            }
            match &mut self.child {
                #[cfg(any(not(windows), test))]
                ChildControl::Direct(child_opt) => {
                    let Some(child) = child_opt.as_mut() else {
                        return Ok(self.exit_code);
                    };
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            let code = status.exit_code();
                            self.exit_code = Some(code);
                            Ok(Some(code))
                        }
                        Ok(None) => Ok(None),
                        Err(err) => Err(TerminalIoError::message(format!(
                            "failed to query child process status: {err}"
                        ))),
                    }
                }
                #[cfg(any(windows, test))]
                ChildControl::Waiter { shared_exit, .. } => {
                    let guard = shared_exit.lock().map_err(|_| {
                        TerminalIoError::message("PTY child exit state lock poisoned")
                    })?;
                    match guard.as_ref() {
                        Some(Ok(code)) => {
                            let code = *code;
                            self.exit_code = Some(code);
                            Ok(Some(code))
                        }
                        Some(Err(msg)) => Err(TerminalIoError::message(msg.clone())),
                        None => Ok(None),
                    }
                }
            }
        }

        fn poll_child_exit_on_eof(&mut self) {
            if self.exit_code.is_some() || self.child_wait_error.is_some() {
                return;
            }
            if let Err(err) = self.try_wait() {
                self.child_wait_error = Some(err);
            }
        }

        const fn pending_len(&self) -> usize {
            self.pending_len.saturating_sub(self.pending_offset)
        }

        fn recycle_buf(&self, buf: Vec<u8>) {
            if buf.capacity() >= READER_CHUNK_BYTES && buf.capacity() <= MAX_RECYCLED_CHUNK_BYTES {
                let sent = self.recycle_tx.try_send(buf).is_ok();
                #[cfg(test)]
                if sent {
                    self.test_stats
                        .recycled_returns
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    self.test_stats
                        .recycled_drops
                        .fetch_add(1, Ordering::Relaxed);
                }
                #[cfg(not(test))]
                let _sent = sent;
            } else {
                #[cfg(test)]
                self.test_stats
                    .recycled_drops
                    .fetch_add(1, Ordering::Relaxed);
            }
        }

        fn recycle_pending(&mut self) {
            let buf = std::mem::take(&mut self.pending);
            self.pending_offset = 0;
            self.pending_len = 0;
            if buf.capacity() > 0 {
                self.recycle_buf(buf);
            }
        }
    }

    impl TerminalInput for PtyBackend {
        fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
            if buf.is_empty() {
                return if self.is_closed() {
                    Err(TerminalIoError::Closed)
                } else {
                    Ok(0)
                };
            }

            let mut written = 0usize;
            while written < buf.len() {
                if self.pending_offset < self.pending_len {
                    let avail = self.pending_len - self.pending_offset;
                    let to_copy = avail.min(buf.len() - written);
                    buf[written..written + to_copy].copy_from_slice(
                        &self.pending[self.pending_offset..self.pending_offset + to_copy],
                    );
                    self.pending_offset += to_copy;
                    written += to_copy;
                    if self.pending_offset >= self.pending_len {
                        self.recycle_pending();
                    }
                } else {
                    if self.closed || self.pending_error.is_some() {
                        break;
                    }
                    let Some(rx) = self.rx.as_ref() else {
                        self.closed = true;
                        self.poll_child_exit_on_eof();
                        break;
                    };
                    match rx.try_recv() {
                        Ok(ReaderEvent::Data { buf: chunk, len }) => {
                            let clamped_len = len.min(chunk.len());
                            if clamped_len == 0 {
                                self.recycle_buf(chunk);
                            } else {
                                if !self.pending.is_empty() {
                                    self.recycle_pending();
                                }
                                self.pending = chunk;
                                self.pending_offset = 0;
                                self.pending_len = clamped_len;
                            }
                        }
                        Ok(ReaderEvent::Eof) | Err(TryRecvError::Disconnected) => {
                            self.closed = true;
                            self.poll_child_exit_on_eof();
                            break;
                        }
                        Ok(ReaderEvent::Error(msg)) => {
                            self.pending_error = Some(msg);
                            self.poll_child_exit_on_eof();
                            break;
                        }
                        Err(TryRecvError::Empty) => break,
                    }
                }
            }

            if written > 0 {
                Ok(written)
            } else if let Some(msg) = self.pending_error.take() {
                self.closed = true;
                Err(TerminalIoError::Message(msg))
            } else if self.closed {
                Err(TerminalIoError::Closed)
            } else {
                Ok(0)
            }
        }

        fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
            if self.is_closed() {
                return Err(TerminalIoError::Closed);
            }
            if bytes.is_empty() {
                return Ok(());
            }
            if self.writer_state.closed.load(Ordering::Acquire) {
                return Err(TerminalIoError::Closed);
            }
            {
                let guard = self.writer_state.error.lock().map_err(|_| {
                    TerminalIoError::message("PTY writer error state lock poisoned")
                })?;
                if let Some(err) = guard.as_ref() {
                    return Err(err.clone());
                }
            }

            let tx = {
                let guard = self
                    .writer_tx
                    .lock()
                    .map_err(|_| TerminalIoError::message("PTY writer channel lock poisoned"))?;
                let Some(tx) = guard.as_ref() else {
                    return Err(TerminalIoError::Closed);
                };
                tx.clone()
            };

            let buf = match self.writer_recycle_rx.try_recv() {
                Ok(mut recycled) => {
                    recycled.clear();
                    recycled.extend_from_slice(bytes);
                    recycled
                }
                Err(_) => bytes.to_vec(),
            };

            match tx.try_send(WriterCommand::Data(buf)) {
                Ok(()) => Ok(()),
                Err(TrySendError::Full(_)) => {
                    Err(TerminalIoError::message("PTY writer queue is full"))
                }
                Err(TrySendError::Disconnected(_)) => {
                    if self.writer_state.closed.load(Ordering::Acquire) {
                        return Err(TerminalIoError::Closed);
                    }
                    let guard = self.writer_state.error.lock().map_err(|_| {
                        TerminalIoError::message("PTY writer error state lock poisoned")
                    })?;
                    if let Some(err) = guard.as_ref() {
                        return Err(err.clone());
                    }
                    Err(TerminalIoError::Closed)
                }
            }
        }

        fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalIoError> {
            if self.is_closed() {
                return Err(TerminalIoError::Closed);
            }
            let clamped = clamp_size(size);
            if clamped == self.last_size {
                return Ok(());
            }
            let guard = self
                .master
                .lock()
                .map_err(|_| TerminalIoError::message("PTY master lock poisoned"))?;
            let Some(master) = guard.as_ref() else {
                return Err(TerminalIoError::Closed);
            };
            master.resize_pty(clamped)?;
            self.last_size = clamped;
            Ok(())
        }

        fn is_closed(&self) -> bool {
            self.closed && self.pending_len() == 0 && self.pending_error.is_none()
        }
    }

    impl Drop for PtyBackend {
        fn drop(&mut self) {
            // 1. Signal loops to exit before tearing down channels/fds.
            self.shutdown.store(true, Ordering::Release);
            // 2. Drop receiver so any blocked `sync_channel::send` unblocks immediately.
            drop(self.rx.take());
            // 3. Close writer channel so an idle writer thread exits promptly.
            close_writer_tx(&self.writer_tx);
            // 4. Terminate direct child if still running without blocking on grandchildren.
            self.child.kill_best_effort();
            // 5. Close master PTY (ClosePseudoConsole on Windows / master fd on Unix),
            //    unblocking both `reader.read` and any blocked `writer.write_all`.
            take_and_drop_master(&self.master);
            // 6. Bounded wait for background threads, then detach so Drop never hangs.
            let deadline = Instant::now() + DROP_JOIN_WAIT;
            wait_done(&self.reader_done, deadline);
            wait_done(&self.writer_done, deadline);
            self.child.wait_done(deadline);
        }
    }

    fn cleanup_child_and_master(child: &mut dyn Child, master: &SharedMaster) {
        if matches!(child.try_wait(), Ok(None)) {
            if child.kill().is_err() {
                // Best-effort termination during failed spawn cleanup.
            }
            if child.try_wait().is_err() {
                // Best-effort reap during failed spawn cleanup.
            }
        }
        take_and_drop_master(master);
    }

    fn close_writer_tx(writer_tx: &SharedWriterTx) {
        let tx = match writer_tx.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(tx) = tx {
            if tx.try_send(WriterCommand::Close).is_err() {
                // Queue full or writer already exited; dropping `tx` disconnects the channel.
            }
            drop(tx);
        }
    }

    fn take_and_drop_master(master: &SharedMaster) {
        let handle = match master.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        drop(handle);
    }

    fn wait_done(state: &(Mutex<bool>, Condvar), deadline: Instant) {
        let (lock, cvar) = state;
        let guard = match lock.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if *guard {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        if cvar
            .wait_timeout_while(guard, remaining, |done| !*done)
            .is_err()
        {
            // Teardown must not panic on condvar wait failure.
        }
    }

    #[cfg(any(windows, test))]
    fn waiter_loop(
        mut child: Box<dyn Child + Send + Sync>,
        shared_exit: &Arc<Mutex<Option<Result<u32, String>>>>,
        writer_tx: &SharedWriterTx,
        master: &SharedMaster,
        repaint: &SharedRepaint,
    ) {
        let exit_result = match child.wait() {
            Ok(status) => Ok(status.exit_code()),
            Err(err) => Err(format!("failed to wait on child process: {err}")),
        };
        {
            let mut guard = match shared_exit.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            *guard = Some(exit_result);
        }
        // On Windows ConPTY, `conhost.exe` keeps the output pipe open until
        // `ClosePseudoConsole` (`MasterPty::drop`) runs. Releasing `writer` and
        // `master` once the child has exited flushes remaining output and signals EOF.
        close_writer_tx(writer_tx);
        take_and_drop_master(master);
        invoke_repaint(repaint);
    }

    fn record_writer_error(state: &WriterSharedState, err: std::io::Error) {
        let io_err = TerminalIoError::from(err);
        if io_err.is_closed() {
            state.closed.store(true, Ordering::Release);
            return;
        }
        let mut guard = match state.error.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.is_none() {
            *guard = Some(io_err);
        }
    }

    #[cfg(windows)]
    fn record_cpr_send_error(state: &WriterSharedState, err: TrySendError<WriterCommand>) {
        match err {
            TrySendError::Disconnected(_) => state.closed.store(true, Ordering::Release),
            TrySendError::Full(_) => {
                let mut guard = match state.error.lock() {
                    Ok(g) => g,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if guard.is_none() {
                    *guard = Some(TerminalIoError::message(
                        "PTY writer queue full during ConPTY CPR reply",
                    ));
                }
            }
        }
    }

    fn recycle_writer_buf(recycle_tx: &SyncSender<Vec<u8>>, mut buf: Vec<u8>) {
        if buf.capacity() <= MAX_RECYCLED_CHUNK_BYTES {
            buf.clear();
            if recycle_tx.try_send(buf).is_err() {
                // Writer recycle pool full or backend dropped; free excess capacity.
            }
        }
    }

    fn writer_loop(
        mut writer: Box<dyn Write + Send>,
        rx: Receiver<WriterCommand>,
        recycle_tx: SyncSender<Vec<u8>>,
        state: &WriterSharedState,
        shutdown: &AtomicBool,
    ) {
        while !shutdown.load(Ordering::Acquire) {
            let first = match rx.recv() {
                Ok(WriterCommand::Data(buf)) => buf,
                Ok(WriterCommand::Close) | Err(_) => break,
            };

            if let Err(err) = writer.write_all(&first) {
                record_writer_error(state, err);
                break;
            }
            recycle_writer_buf(&recycle_tx, first);

            let mut should_exit = false;
            for _ in 0..WRITER_CHANNEL_CAPACITY {
                match rx.try_recv() {
                    Ok(WriterCommand::Data(buf)) => {
                        if let Err(err) = writer.write_all(&buf) {
                            record_writer_error(state, err);
                            should_exit = true;
                            break;
                        }
                        recycle_writer_buf(&recycle_tx, buf);
                    }
                    Ok(WriterCommand::Close) | Err(TryRecvError::Disconnected) => {
                        should_exit = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }

            if let Err(err) = writer.flush() {
                record_writer_error(state, err);
                break;
            }
            if should_exit {
                break;
            }
        }
    }

    fn acquire_read_buf(
        recycle_rx: &Receiver<Vec<u8>>,
        #[cfg(test)] test_stats: &TestPoolStats,
    ) -> Vec<u8> {
        let buf = match recycle_rx.try_recv() {
            Ok(mut recycled) => {
                if recycled.len() != READER_CHUNK_BYTES {
                    recycled.resize(READER_CHUNK_BYTES, 0);
                }
                #[cfg(test)]
                test_stats.read_reuses.fetch_add(1, Ordering::Relaxed);
                recycled
            }
            Err(_) => {
                #[cfg(test)]
                test_stats.read_allocs.fetch_add(1, Ordering::Relaxed);
                vec![0u8; READER_CHUNK_BYTES]
            }
        };
        #[cfg(test)]
        if let Ok(mut seen) = test_stats.seen_ptrs.lock() {
            seen.insert(buf.as_ptr() as usize);
        }
        buf
    }

    struct ReaderChannelContext {
        reader: Box<dyn Read + Send>,
        tx: SyncSender<ReaderEvent>,
        recycle_rx: Receiver<Vec<u8>>,
        repaint: SharedRepaint,
        shutdown: Arc<AtomicBool>,
    }

    fn reader_loop(
        mut ctx: ReaderChannelContext,
        #[cfg(windows)] mut cpr_writer_tx: Option<SyncSender<WriterCommand>>,
        #[cfg(windows)] writer_state: &WriterSharedState,
        #[cfg(test)] test_stats: &TestPoolStats,
    ) {
        let ReaderChannelContext {
            reader,
            tx,
            recycle_rx,
            repaint,
            shutdown,
        } = &mut ctx;
        let mut current_buf: Option<Vec<u8>> = None;
        #[cfg(windows)]
        let mut conpty_handshake_pending = true;
        #[cfg(windows)]
        let mut handshake_buf = Vec::<u8>::with_capacity(8);

        while !shutdown.load(Ordering::Acquire) {
            let mut buf = match current_buf.take() {
                Some(b) => b,
                None => acquire_read_buf(
                    recycle_rx,
                    #[cfg(test)]
                    test_stats,
                ),
            };

            match reader.read(&mut buf) {
                Ok(0) => {
                    #[cfg(windows)]
                    if !handshake_buf.is_empty() {
                        let forward = std::mem::take(&mut handshake_buf);
                        let len = forward.len();
                        if tx.send(ReaderEvent::Data { buf: forward, len }).is_err() {
                            break;
                        }
                    }
                    if tx.send(ReaderEvent::Eof).is_err() {
                        // Receiver already dropped during teardown.
                    }
                    invoke_repaint(repaint);
                    break;
                }
                Ok(n) => {
                    #[cfg(windows)]
                    if conpty_handshake_pending {
                        const CPR_QUERY: &[u8] = b"\x1b[6n";
                        handshake_buf.extend_from_slice(&buf[..n]);
                        if handshake_buf.starts_with(CPR_QUERY) {
                            conpty_handshake_pending = false;
                            if let Some(wtx) = cpr_writer_tx.take()
                                && let Err(err) =
                                    wtx.try_send(WriterCommand::Data(b"\x1b[1;1R".to_vec()))
                            {
                                record_cpr_send_error(writer_state, err);
                            }
                            let rest_len = handshake_buf.len() - CPR_QUERY.len();
                            if rest_len > 0 {
                                if rest_len <= buf.len() {
                                    buf[..rest_len]
                                        .copy_from_slice(&handshake_buf[CPR_QUERY.len()..]);
                                    handshake_buf.clear();
                                    if tx.send(ReaderEvent::Data { buf, len: rest_len }).is_err() {
                                        break;
                                    }
                                } else {
                                    let rest = handshake_buf.split_off(CPR_QUERY.len());
                                    handshake_buf.clear();
                                    current_buf = Some(buf);
                                    if tx
                                        .send(ReaderEvent::Data {
                                            buf: rest,
                                            len: rest_len,
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                invoke_repaint(repaint);
                            } else {
                                handshake_buf.clear();
                                current_buf = Some(buf);
                            }
                            continue;
                        }
                        if CPR_QUERY.starts_with(&handshake_buf) {
                            current_buf = Some(buf);
                            continue;
                        }
                        conpty_handshake_pending = false;
                        drop(cpr_writer_tx.take());
                        let forward_len = handshake_buf.len();
                        if forward_len <= buf.len() {
                            buf[..forward_len].copy_from_slice(&handshake_buf);
                            handshake_buf.clear();
                            if tx
                                .send(ReaderEvent::Data {
                                    buf,
                                    len: forward_len,
                                })
                                .is_err()
                            {
                                break;
                            }
                        } else {
                            let forward = std::mem::take(&mut handshake_buf);
                            current_buf = Some(buf);
                            if tx
                                .send(ReaderEvent::Data {
                                    buf: forward,
                                    len: forward_len,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        invoke_repaint(repaint);
                        continue;
                    }

                    if tx.send(ReaderEvent::Data { buf, len: n }).is_err() {
                        break;
                    }
                    invoke_repaint(repaint);
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => {
                    current_buf = Some(buf);
                }
                Err(err)
                    if matches!(err.kind(), ErrorKind::BrokenPipe | ErrorKind::UnexpectedEof) =>
                {
                    #[cfg(windows)]
                    if !handshake_buf.is_empty() {
                        let forward = std::mem::take(&mut handshake_buf);
                        let len = forward.len();
                        if tx.send(ReaderEvent::Data { buf: forward, len }).is_err() {
                            break;
                        }
                    }
                    if tx.send(ReaderEvent::Eof).is_err() {
                        // Receiver already dropped during teardown.
                    }
                    invoke_repaint(repaint);
                    break;
                }
                Err(err) => {
                    #[cfg(windows)]
                    if !handshake_buf.is_empty() {
                        let forward = std::mem::take(&mut handshake_buf);
                        let len = forward.len();
                        if tx.send(ReaderEvent::Data { buf: forward, len }).is_err() {
                            break;
                        }
                    }
                    if tx.send(ReaderEvent::Error(err.to_string())).is_err() {
                        // Receiver already dropped during teardown.
                    }
                    invoke_repaint(repaint);
                    break;
                }
            }
        }
    }

    fn invoke_repaint(repaint: &SharedRepaint) {
        let cb = {
            let guard = match repaint.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.as_ref().cloned()
        };
        if let Some(cb) = cb {
            cb();
        }
    }

    fn clamp_size(size: TerminalSize) -> TerminalSize {
        TerminalSize {
            cols: size.cols.clamp(1, MAX_PTY_DIM),
            rows: size.rows.clamp(1, MAX_PTY_DIM),
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        }
    }

    const fn to_pty_size(size: TerminalSize) -> PtySize {
        PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        }
    }

    #[cfg(test)]
    mod tests {
        use std::collections::VecDeque;
        use std::sync::atomic::AtomicUsize;

        use portable_pty::ExitStatus;

        use super::*;

        enum ReadStep {
            Data(Vec<u8>),
            Eof,
            Interrupted,
            Error(ErrorKind, &'static str),
        }

        type SharedReadQueue = Arc<(Mutex<(VecDeque<ReadStep>, bool)>, Condvar)>;

        struct FakeReader {
            queue: SharedReadQueue,
        }

        impl FakeReader {
            fn pair() -> (FakeReaderController, Box<dyn Read + Send>) {
                let queue = Arc::new((Mutex::new((VecDeque::new(), false)), Condvar::new()));
                (
                    FakeReaderController {
                        queue: Arc::clone(&queue),
                    },
                    Box::new(Self { queue }),
                )
            }
        }

        struct FakeReaderController {
            queue: SharedReadQueue,
        }

        impl FakeReaderController {
            fn push(&self, step: ReadStep) {
                let (lock, cvar) = &*self.queue;
                let mut guard = lock.lock().unwrap();
                guard.0.push_back(step);
                cvar.notify_all();
            }

            fn close(&self) {
                let (lock, cvar) = &*self.queue;
                let mut guard = lock.lock().unwrap();
                guard.1 = true;
                cvar.notify_all();
            }
        }

        impl Read for FakeReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let (lock, cvar) = &*self.queue;
                let mut guard = lock.lock().unwrap();
                loop {
                    if let Some(step) = guard.0.front_mut() {
                        match step {
                            ReadStep::Data(chunk) => {
                                let n = chunk.len().min(buf.len());
                                buf[..n].copy_from_slice(&chunk[..n]);
                                if n < chunk.len() {
                                    chunk.drain(..n);
                                } else {
                                    guard.0.pop_front();
                                }
                                return Ok(n);
                            }
                            ReadStep::Eof => {
                                guard.0.pop_front();
                                return Ok(0);
                            }
                            ReadStep::Interrupted => {
                                guard.0.pop_front();
                                return Err(std::io::Error::new(
                                    ErrorKind::Interrupted,
                                    "interrupted",
                                ));
                            }
                            ReadStep::Error(kind, msg) => {
                                let k = *kind;
                                let m = *msg;
                                guard.0.pop_front();
                                return Err(std::io::Error::new(k, m));
                            }
                        }
                    }
                    if guard.1 {
                        return Ok(0);
                    }
                    guard = cvar.wait(guard).unwrap();
                }
            }
        }

        struct FakeWriterState {
            written: Vec<u8>,
            flushes: usize,
            blocked: bool,
            fail_kind: Option<ErrorKind>,
            fail_notified: bool,
            dropped: bool,
        }

        struct FakeWriter {
            state: Arc<(Mutex<FakeWriterState>, Condvar)>,
        }

        struct FakeWriterController {
            state: Arc<(Mutex<FakeWriterState>, Condvar)>,
        }

        impl FakeWriter {
            fn pair(blocked: bool) -> (FakeWriterController, Box<dyn Write + Send>) {
                let state = Arc::new((
                    Mutex::new(FakeWriterState {
                        written: Vec::new(),
                        flushes: 0,
                        blocked,
                        fail_kind: None,
                        fail_notified: false,
                        dropped: false,
                    }),
                    Condvar::new(),
                ));
                (
                    FakeWriterController {
                        state: Arc::clone(&state),
                    },
                    Box::new(Self { state }),
                )
            }
        }

        impl FakeWriterController {
            fn set_blocked(&self, blocked: bool) {
                let (lock, cvar) = &*self.state;
                let mut guard = lock.lock().unwrap();
                guard.blocked = blocked;
                cvar.notify_all();
            }

            fn set_fail_kind(&self, kind: ErrorKind) {
                let (lock, cvar) = &*self.state;
                let mut guard = lock.lock().unwrap();
                guard.fail_kind = Some(kind);
                guard.blocked = false;
                cvar.notify_all();
            }

            fn wait_written_len(&self, expected: usize, timeout: Duration) -> Vec<u8> {
                let (lock, cvar) = &*self.state;
                let guard = lock.lock().unwrap();
                let (guard, _) = cvar
                    .wait_timeout_while(guard, timeout, |s| s.written.len() < expected)
                    .unwrap();
                guard.written.clone()
            }

            fn wait_flushes(&self, min_flushes: usize, timeout: Duration) -> usize {
                let (lock, cvar) = &*self.state;
                let guard = lock.lock().unwrap();
                let (guard, _) = cvar
                    .wait_timeout_while(guard, timeout, |s| s.flushes < min_flushes)
                    .unwrap();
                guard.flushes
            }

            fn wait_failure_observed(&self, timeout: Duration) -> bool {
                let (lock, cvar) = &*self.state;
                let guard = lock.lock().unwrap();
                let (guard, _) = cvar
                    .wait_timeout_while(guard, timeout, |s| !s.fail_notified)
                    .unwrap();
                guard.fail_notified
            }

            fn wait_dropped(&self, timeout: Duration) -> bool {
                let (lock, cvar) = &*self.state;
                let guard = lock.lock().unwrap();
                let (guard, _) = cvar
                    .wait_timeout_while(guard, timeout, |s| !s.dropped)
                    .unwrap();
                guard.dropped
            }
        }

        impl Drop for FakeWriter {
            fn drop(&mut self) {
                let (lock, cvar) = &*self.state;
                let mut guard = match lock.lock() {
                    Ok(g) => g,
                    Err(poisoned) => poisoned.into_inner(),
                };
                guard.dropped = true;
                cvar.notify_all();
            }
        }

        impl Write for FakeWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                let (lock, cvar) = &*self.state;
                let mut guard = lock.lock().unwrap();
                while guard.blocked {
                    guard = cvar.wait(guard).unwrap();
                }
                if let Some(kind) = guard.fail_kind {
                    guard.fail_notified = true;
                    cvar.notify_all();
                    return Err(std::io::Error::new(kind, "injected write error"));
                }
                guard.written.extend_from_slice(buf);
                cvar.notify_all();
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                let (lock, cvar) = &*self.state;
                let mut guard = lock.lock().unwrap();
                if let Some(kind) = guard.fail_kind {
                    guard.fail_notified = true;
                    cvar.notify_all();
                    return Err(std::io::Error::new(kind, "injected flush error"));
                }
                guard.flushes += 1;
                cvar.notify_all();
                Ok(())
            }
        }

        #[derive(Debug)]
        struct FakeChildState {
            exited: Option<u32>,
            killed: bool,
        }

        #[derive(Debug)]
        struct FakeChild {
            state: Arc<(Mutex<FakeChildState>, Condvar)>,
            try_wait_calls: Arc<AtomicUsize>,
        }

        #[derive(Debug)]
        struct FakeKiller {
            state: Arc<(Mutex<FakeChildState>, Condvar)>,
        }

        struct FakeChildController {
            state: Arc<(Mutex<FakeChildState>, Condvar)>,
            try_wait_calls: Arc<AtomicUsize>,
        }

        impl FakeChild {
            fn pair() -> (FakeChildController, Box<dyn Child + Send + Sync>) {
                let state = Arc::new((
                    Mutex::new(FakeChildState {
                        exited: None,
                        killed: false,
                    }),
                    Condvar::new(),
                ));
                let try_wait_calls = Arc::new(AtomicUsize::new(0));
                (
                    FakeChildController {
                        state: Arc::clone(&state),
                        try_wait_calls: Arc::clone(&try_wait_calls),
                    },
                    Box::new(Self {
                        state,
                        try_wait_calls,
                    }),
                )
            }
        }

        impl FakeChildController {
            fn set_exited(&self, code: u32) {
                let (lock, cvar) = &*self.state;
                let mut guard = lock.lock().unwrap();
                guard.exited = Some(code);
                cvar.notify_all();
            }

            fn try_wait_calls(&self) -> usize {
                self.try_wait_calls.load(Ordering::SeqCst)
            }

            fn was_killed(&self) -> bool {
                self.state.0.lock().unwrap().killed
            }
        }

        impl ChildKiller for FakeKiller {
            fn kill(&mut self) -> std::io::Result<()> {
                let (lock, cvar) = &*self.state;
                let mut guard = lock.lock().unwrap();
                guard.killed = true;
                if guard.exited.is_none() {
                    guard.exited = Some(137);
                }
                cvar.notify_all();
                Ok(())
            }

            fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
                Box::new(Self {
                    state: Arc::clone(&self.state),
                })
            }
        }

        impl ChildKiller for FakeChild {
            fn kill(&mut self) -> std::io::Result<()> {
                let (lock, cvar) = &*self.state;
                let mut guard = lock.lock().unwrap();
                guard.killed = true;
                if guard.exited.is_none() {
                    guard.exited = Some(137);
                }
                cvar.notify_all();
                Ok(())
            }

            fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
                Box::new(FakeKiller {
                    state: Arc::clone(&self.state),
                })
            }
        }

        impl Child for FakeChild {
            fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
                self.try_wait_calls.fetch_add(1, Ordering::SeqCst);
                let guard = self.state.0.lock().unwrap();
                Ok(guard.exited.map(ExitStatus::with_exit_code))
            }

            fn wait(&mut self) -> std::io::Result<ExitStatus> {
                let (lock, cvar) = &*self.state;
                let mut guard = lock.lock().unwrap();
                while guard.exited.is_none() {
                    guard = cvar.wait(guard).unwrap();
                }
                Ok(ExitStatus::with_exit_code(guard.exited.unwrap_or(0)))
            }

            fn process_id(&self) -> Option<u32> {
                Some(4242)
            }

            #[cfg(windows)]
            fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
                None
            }
        }

        struct FakeMasterHandles {
            dropped: Arc<AtomicBool>,
            master: Box<dyn PtyMaster>,
        }

        struct FakeMaster {
            dropped: Arc<AtomicBool>,
            resizes: Arc<Mutex<Vec<TerminalSize>>>,
        }

        impl FakeMaster {
            fn pair() -> FakeMasterHandles {
                let dropped = Arc::new(AtomicBool::new(false));
                let resizes = Arc::new(Mutex::new(Vec::new()));
                FakeMasterHandles {
                    dropped: Arc::clone(&dropped),
                    master: Box::new(Self { dropped, resizes }),
                }
            }
        }

        impl Drop for FakeMaster {
            fn drop(&mut self) {
                self.dropped.store(true, Ordering::SeqCst);
            }
        }

        impl PtyMaster for FakeMaster {
            fn resize_pty(&self, size: TerminalSize) -> Result<(), TerminalIoError> {
                self.resizes.lock().unwrap().push(size);
                Ok(())
            }
        }

        struct TestBackendHarness {
            backend: PtyBackend,
            reader: FakeReaderController,
            writer: FakeWriterController,
            child: FakeChildController,
            master_dropped: Arc<AtomicBool>,
            repaint_sig: Arc<(Mutex<usize>, Condvar)>,
            stats: Arc<TestPoolStats>,
        }

        fn make_repaint_signal() -> (Arc<(Mutex<usize>, Condvar)>, RepaintCallback) {
            let signal = Arc::new((Mutex::new(0usize), Condvar::new()));
            let cloned = Arc::clone(&signal);
            let cb: RepaintCallback = Box::new(move || {
                let (lock, cvar) = &*cloned;
                let mut count = lock.lock().unwrap();
                *count += 1;
                cvar.notify_all();
            });
            (signal, cb)
        }

        fn wait_repaint(signal: &(Mutex<usize>, Condvar), target: usize, timeout: Duration) {
            let (lock, cvar) = signal;
            let guard = lock.lock().unwrap();
            let (guard, res) = cvar
                .wait_timeout_while(guard, timeout, |c| *c < target)
                .unwrap();
            assert!(
                !res.timed_out(),
                "timed out waiting for repaint >= {target}, got {}",
                *guard
            );
        }

        fn spawn_test_backend(mode: ExitMonitorMode, writer_blocked: bool) -> TestBackendHarness {
            let (reader_ctrl, reader) = FakeReader::pair();
            let (writer_ctrl, writer) = FakeWriter::pair(writer_blocked);
            let (child_ctrl, child) = FakeChild::pair();
            let master_handles = FakeMaster::pair();
            let (repaint_sig, repaint_cb) = make_repaint_signal();
            let stats = Arc::new(TestPoolStats::default());
            let backend = PtyBackend::from_parts_inner(
                TransportHandles {
                    reader,
                    writer,
                    child,
                    master: master_handles.master,
                },
                TerminalSize::new(80, 24),
                Some(repaint_cb),
                mode,
                Arc::clone(&stats),
            )
            .expect("from_parts_inner succeeds");
            TestBackendHarness {
                backend,
                reader: reader_ctrl,
                writer: writer_ctrl,
                child: child_ctrl,
                master_dropped: master_handles.dropped,
                repaint_sig,
                stats,
            }
        }

        #[test]
        fn clamp_size_bounds_zero_and_oversized_dimensions() {
            let zero = clamp_size(TerminalSize::with_pixels(0, 0, 640, 480));
            assert_eq!(zero, TerminalSize::with_pixels(1, 1, 640, 480));

            let huge = clamp_size(TerminalSize::new(u16::MAX, u16::MAX));
            assert_eq!(huge, TerminalSize::new(MAX_PTY_DIM, MAX_PTY_DIM));

            let edge_cols = clamp_size(TerminalSize::new(32767, 24));
            assert_eq!(edge_cols, TerminalSize::new(MAX_PTY_DIM, 24));

            let edge_rows = clamp_size(TerminalSize::new(80, 32767));
            assert_eq!(edge_rows, TerminalSize::new(80, MAX_PTY_DIM));
        }

        #[test]
        fn read_buffer_pool_reuses_capacities_zero_steady_state_allocs() {
            let TestBackendHarness {
                mut backend,
                reader: reader_ctrl,
                repaint_sig,
                stats,
                ..
            } = spawn_test_backend(ExitMonitorMode::DirectOnEof, false);

            let mut scratch = vec![0u8; 64 * 1024];
            let chunk = vec![b'A'; READER_CHUNK_BYTES];

            // Warm up pool with 4 sequential chunks so at least 1 buffer is recycled cleanly.
            for i in 1..=4 {
                reader_ctrl.push(ReadStep::Data(chunk.clone()));
                wait_repaint(&repaint_sig, i, Duration::from_secs(2));
                assert_eq!(backend.poll_read(&mut scratch).unwrap(), READER_CHUNK_BYTES);
            }

            let allocs_after_warmup = stats.read_allocs.load(Ordering::Relaxed);
            stats.seen_ptrs.lock().unwrap().clear();

            // Stream 500 x 16 KiB chunks in steady state, draining each before pushing the next.
            for i in 1..=500 {
                reader_ctrl.push(ReadStep::Data(chunk.clone()));
                wait_repaint(&repaint_sig, 4 + i, Duration::from_secs(2));
                let n = backend.poll_read(&mut scratch).unwrap();
                assert_eq!(n, READER_CHUNK_BYTES);
                assert!(scratch[..n].iter().all(|&b| b == b'A'));
            }

            let steady_allocs = stats.read_allocs.load(Ordering::Relaxed) - allocs_after_warmup;
            let reuses = stats.read_reuses.load(Ordering::Relaxed);
            let unique_ptrs = stats.seen_ptrs.lock().unwrap().len();
            assert_eq!(
                steady_allocs, 0,
                "steady-state reader_loop must perform 0 heap allocations"
            );
            assert!(
                reuses >= 500,
                "expected >= 500 recycled buffer reuses, got {reuses}"
            );
            assert!(
                (1..=RECYCLE_POOL_CAPACITY).contains(&unique_ptrs),
                "unique steady-state buffer pointers {unique_ptrs} exceeded pool cap"
            );

            reader_ctrl.close();
        }

        #[test]
        fn read_buffer_pool_is_bounded_under_backpressure_and_teardown() {
            let TestBackendHarness {
                mut backend,
                reader: reader_ctrl,
                repaint_sig,
                stats,
                ..
            } = spawn_test_backend(ExitMonitorMode::DirectOnEof, false);

            let chunk = vec![b'Z'; READER_CHUNK_BYTES];
            // Push 512 chunks (> READER_CHANNEL_CAPACITY = 256) before reading anything.
            for _ in 0..512 {
                reader_ctrl.push(ReadStep::Data(chunk.clone()));
            }
            reader_ctrl.push(ReadStep::Eof);
            wait_repaint(&repaint_sig, 1, Duration::from_secs(2));

            let mut scratch = vec![0u8; 64 * 1024];
            let mut total = 0usize;
            loop {
                let seq = { *repaint_sig.0.lock().unwrap() };
                match backend.poll_read(&mut scratch) {
                    Ok(0) => {
                        let (lock, cvar) = &*repaint_sig;
                        let guard = lock.lock().unwrap();
                        let (_guard, res) = cvar
                            .wait_timeout_while(guard, Duration::from_secs(2), |c| *c == seq)
                            .unwrap();
                        assert!(!res.timed_out(), "timed out waiting for next chunk");
                    }
                    Ok(n) => total += n,
                    Err(TerminalIoError::Closed) => break,
                    Err(err) => panic!("unexpected error: {err}"),
                }
            }
            assert_eq!(total, 512 * READER_CHUNK_BYTES);
            assert!(backend.is_closed());

            // The recycle channel capacity is bounded to RECYCLE_POOL_CAPACITY; excess buffers drop.
            let returns = stats.recycled_returns.load(Ordering::Relaxed);
            let drops = stats.recycled_drops.load(Ordering::Relaxed);
            assert!(returns + drops >= 512);
            assert!(
                drops > 0 || returns <= RECYCLE_POOL_CAPACITY + 512,
                "pool accounting should stay consistent"
            );
        }

        #[test]
        fn poll_read_never_invokes_try_wait_while_stream_open() {
            let TestBackendHarness {
                mut backend,
                reader: reader_ctrl,
                child: child_ctrl,
                repaint_sig,
                ..
            } = spawn_test_backend(ExitMonitorMode::DirectOnEof, false);

            let mut scratch = [0u8; 1024];
            for i in 0..1000 {
                if i % 100 == 0 {
                    reader_ctrl.push(ReadStep::Data(b"ping".to_vec()));
                    wait_repaint(&repaint_sig, (i / 100) + 1, Duration::from_secs(2));
                }
                let _ = backend.poll_read(&mut scratch).unwrap();
            }
            assert_eq!(child_ctrl.try_wait_calls(), 0);

            reader_ctrl.close();
        }

        #[test]
        fn eof_surfaces_exit_code_and_transitions_is_closed_once_drained() {
            let TestBackendHarness {
                mut backend,
                reader: reader_ctrl,
                child: child_ctrl,
                repaint_sig,
                ..
            } = spawn_test_backend(ExitMonitorMode::DirectOnEof, false);

            child_ctrl.set_exited(42);
            reader_ctrl.push(ReadStep::Data(b"FINAL_CHUNK".to_vec()));
            reader_ctrl.push(ReadStep::Eof);
            wait_repaint(&repaint_sig, 2, Duration::from_secs(2));

            let mut half = [0u8; 5];
            assert_eq!(backend.poll_read(&mut half), Ok(5));
            assert_eq!(&half, b"FINAL");
            assert!(
                !backend.is_closed(),
                "is_closed must remain false while buffered bytes remain"
            );
            assert_eq!(child_ctrl.try_wait_calls(), 0);

            let mut rest = [0u8; 32];
            assert_eq!(backend.poll_read(&mut rest), Ok(6));
            assert_eq!(&rest[..6], b"_CHUNK");
            assert!(backend.is_closed());
            assert_eq!(backend.poll_read(&mut rest), Err(TerminalIoError::Closed));
            assert_eq!(backend.try_wait().unwrap(), Some(42));
            assert!(child_ctrl.try_wait_calls() <= 1);
        }

        #[test]
        fn waiter_thread_drops_writer_and_master_on_exit_and_surfaces_exit_code() {
            let TestBackendHarness {
                mut backend,
                reader: reader_ctrl,
                writer: writer_ctrl,
                child: child_ctrl,
                master_dropped,
                repaint_sig,
                ..
            } = spawn_test_backend(ExitMonitorMode::WaiterThread, false);

            assert!(!master_dropped.load(Ordering::SeqCst));
            reader_ctrl.push(ReadStep::Data(b"WIN_OUT".to_vec()));
            wait_repaint(&repaint_sig, 1, Duration::from_secs(2));

            let mut buf = [0u8; 64];
            assert_eq!(backend.poll_read(&mut buf), Ok(7));
            assert_eq!(&buf[..7], b"WIN_OUT");
            assert_eq!(backend.try_wait().unwrap(), None);

            // Simulate child process exiting: waiter surfaces exit code, closes writer, and drops
            // master before ConPTY unblocks EOF on the reader. Each thread emits its own repaint.
            child_ctrl.set_exited(9);
            wait_repaint(&repaint_sig, 2, Duration::from_secs(2));

            assert!(master_dropped.load(Ordering::SeqCst));
            assert!(
                writer_ctrl.wait_dropped(Duration::from_secs(2)),
                "writer thread must drop writer handle after child exit"
            );
            assert_eq!(
                backend.write_bytes(b"after_exit"),
                Err(TerminalIoError::Closed)
            );
            assert_eq!(
                backend.resize(TerminalSize::new(90, 30)),
                Err(TerminalIoError::Closed)
            );
            assert_eq!(backend.try_wait().unwrap(), Some(9));
            assert!(!backend.is_closed());

            // Now simulate master drop unblocking EOF on the reader.
            reader_ctrl.push(ReadStep::Eof);
            wait_repaint(&repaint_sig, 3, Duration::from_secs(2));

            assert_eq!(backend.poll_read(&mut buf), Err(TerminalIoError::Closed));
            assert!(backend.is_closed());
            assert!(master_dropped.load(Ordering::SeqCst));
            assert_eq!(backend.try_wait().unwrap(), Some(9));
            assert_eq!(child_ctrl.try_wait_calls(), 0);
        }

        #[test]
        fn waiter_thread_surfaces_exit_code_even_when_eof_arrives_first() {
            let TestBackendHarness {
                mut backend,
                reader: reader_ctrl,
                writer: writer_ctrl,
                child: child_ctrl,
                master_dropped,
                repaint_sig,
                ..
            } = spawn_test_backend(ExitMonitorMode::WaiterThread, false);

            reader_ctrl.push(ReadStep::Eof);
            wait_repaint(&repaint_sig, 1, Duration::from_secs(2));

            let mut buf = [0u8; 16];
            assert_eq!(backend.poll_read(&mut buf), Err(TerminalIoError::Closed));
            assert!(backend.is_closed());
            assert_eq!(backend.try_wait().unwrap(), None);
            assert!(!master_dropped.load(Ordering::SeqCst));

            child_ctrl.set_exited(11);
            wait_repaint(&repaint_sig, 2, Duration::from_secs(2));

            assert!(master_dropped.load(Ordering::SeqCst));
            assert!(
                writer_ctrl.wait_dropped(Duration::from_secs(2)),
                "writer thread must drop writer handle after child exit"
            );
            assert_eq!(backend.try_wait().unwrap(), Some(11));
            assert_eq!(child_ctrl.try_wait_calls(), 0);
        }

        #[test]
        fn waiter_thread_concurrent_exit_and_eof_both_notify_repaint() {
            let TestBackendHarness {
                mut backend,
                reader: reader_ctrl,
                writer: writer_ctrl,
                child: child_ctrl,
                master_dropped,
                repaint_sig,
                ..
            } = spawn_test_backend(ExitMonitorMode::WaiterThread, false);

            child_ctrl.set_exited(5);
            reader_ctrl.push(ReadStep::Eof);
            wait_repaint(&repaint_sig, 2, Duration::from_secs(2));

            let mut buf = [0u8; 16];
            assert_eq!(backend.poll_read(&mut buf), Err(TerminalIoError::Closed));
            assert!(backend.is_closed());
            assert!(master_dropped.load(Ordering::SeqCst));
            assert!(
                writer_ctrl.wait_dropped(Duration::from_secs(2)),
                "writer thread must drop writer handle after child exit"
            );
            assert_eq!(backend.try_wait().unwrap(), Some(5));
            assert_eq!(child_ctrl.try_wait_calls(), 0);
        }

        #[test]
        fn write_bytes_never_blocks_when_writer_thread_stalled() {
            let TestBackendHarness {
                mut backend,
                reader: reader_ctrl,
                writer: writer_ctrl,
                ..
            } = spawn_test_backend(ExitMonitorMode::DirectOnEof, true);

            let start = Instant::now();
            for i in 0..WRITER_CHANNEL_CAPACITY {
                let payload = [b'a' + (i % 26) as u8; 4];
                backend.write_bytes(&payload).unwrap();
            }
            assert!(
                start.elapsed() < Duration::from_millis(50),
                "write_bytes blocked UI thread when writer stalled: {:?}",
                start.elapsed()
            );

            writer_ctrl.set_blocked(false);
            let received =
                writer_ctrl.wait_written_len(WRITER_CHANNEL_CAPACITY * 4, Duration::from_secs(2));
            assert_eq!(received.len(), WRITER_CHANNEL_CAPACITY * 4);
            assert_eq!(&received[..4], b"aaaa");
            assert_eq!(&received[4..8], b"bbbb");
            assert!(writer_ctrl.wait_flushes(1, Duration::from_secs(2)) >= 1);

            reader_ctrl.close();
        }

        #[test]
        fn write_bytes_queue_full_returns_nonblocking_backpressure_error() {
            let TestBackendHarness {
                mut backend,
                reader: reader_ctrl,
                writer: writer_ctrl,
                ..
            } = spawn_test_backend(ExitMonitorMode::DirectOnEof, true);

            // First write enters writer_loop (or stays at head of queue) and stalls on FakeWriter.
            backend.write_bytes(b"first").unwrap();
            let mut queued_bytes = 5usize;
            // Fill the bounded sync_channel completely.
            let mut hit_full = false;
            let start = Instant::now();
            for _ in 0..(WRITER_CHANNEL_CAPACITY + 16) {
                match backend.write_bytes(b"x") {
                    Ok(()) => queued_bytes += 1,
                    Err(TerminalIoError::Message(msg)) => {
                        assert!(msg.contains("full"), "unexpected message: {msg}");
                        hit_full = true;
                        break;
                    }
                    Err(other) => panic!("expected Message backpressure error, got {other:?}"),
                }
            }
            assert!(hit_full, "expected queue-full backpressure error");
            assert!(start.elapsed() < Duration::from_millis(50));
            assert!(!backend.is_closed());

            // Release stalled writer; queue drains and subsequent writes succeed again.
            writer_ctrl.set_blocked(false);
            let drained = writer_ctrl.wait_written_len(queued_bytes, Duration::from_secs(2));
            assert_eq!(drained.len(), queued_bytes);
            backend.write_bytes(b"after_drain").unwrap();
            let final_written = writer_ctrl
                .wait_written_len(queued_bytes + b"after_drain".len(), Duration::from_secs(2));
            assert!(final_written.ends_with(b"after_drain"));

            reader_ctrl.close();
        }

        #[test]
        fn write_bytes_surfaces_broken_pipe_and_io_errors_on_subsequent_calls() {
            // Case A: BrokenPipe surfaces Err(Closed) on write_bytes WITHOUT prematurely
            // setting is_closed() == true while unread stdout remains.
            let TestBackendHarness {
                backend: mut backend_a,
                reader: reader_a,
                writer: writer_a,
                repaint_sig: sig_a,
                ..
            } = spawn_test_backend(ExitMonitorMode::DirectOnEof, false);
            reader_a.push(ReadStep::Data(b"STILL_UNREAD_STDOUT".to_vec()));
            wait_repaint(&sig_a, 1, Duration::from_secs(2));

            writer_a.set_fail_kind(ErrorKind::BrokenPipe);
            backend_a.write_bytes(b"trigger").unwrap();
            assert!(writer_a.wait_failure_observed(Duration::from_secs(2)));

            // Wait deterministically until writer thread records closure (empty write_bytes is Ok,
            // non-empty surfaces Closed).
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match backend_a.write_bytes(b"next") {
                    Err(TerminalIoError::Closed) => break,
                    Ok(()) if Instant::now() < deadline => thread::yield_now(),
                    other => panic!("expected BrokenPipe -> Closed, got {other:?}"),
                }
            }
            assert!(
                !backend_a.is_closed(),
                "writer BrokenPipe must not set is_closed() while unread stdout remains"
            );
            let mut out = [0u8; 32];
            assert_eq!(backend_a.poll_read(&mut out), Ok(19));
            assert_eq!(&out[..19], b"STILL_UNREAD_STDOUT");
            reader_a.close();

            // Case B: Other I/O error surfaces Err(Message(...)).
            let TestBackendHarness {
                backend: mut backend_b,
                reader: reader_b,
                writer: writer_b,
                ..
            } = spawn_test_backend(ExitMonitorMode::DirectOnEof, false);
            writer_b.set_fail_kind(ErrorKind::Other);
            backend_b.write_bytes(b"trigger").unwrap();
            assert!(writer_b.wait_failure_observed(Duration::from_secs(2)));

            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match backend_b.write_bytes(b"next") {
                    Err(TerminalIoError::Message(msg)) => {
                        assert!(msg.contains("injected write error"), "got: {msg}");
                        break;
                    }
                    Ok(()) if Instant::now() < deadline => thread::yield_now(),
                    other => panic!("expected Message error, got {other:?}"),
                }
            }
            assert!(!backend_b.is_closed());
            reader_b.close();
        }

        #[test]
        fn partial_reads_across_chunk_boundaries_preserve_exact_bytes() {
            let TestBackendHarness {
                mut backend,
                reader: reader_ctrl,
                repaint_sig,
                stats,
                ..
            } = spawn_test_backend(ExitMonitorMode::DirectOnEof, false);

            let mut expected = Vec::new();
            for (idx, size) in [1usize, 7, 16_384, 20_000].into_iter().enumerate() {
                let chunk: Vec<u8> = (0..size).map(|j| ((idx * 31 + j) % 251) as u8).collect();
                expected.extend_from_slice(&chunk);
                reader_ctrl.push(ReadStep::Data(chunk));
            }
            // Wait for all 5 read chunks (20_000 splits across 16_384 + 3_616) while reader is alive
            // so returned buffers enter `recycle_rx` before `reader_loop` exits on EOF.
            wait_repaint(&repaint_sig, 5, Duration::from_secs(2));

            let mut actual = Vec::new();
            let prime_sizes = [1usize, 3, 7, 64, 512];
            let mut step = 0usize;
            while actual.len() < expected.len() {
                assert_eq!(backend.poll_read(&mut []), Ok(0));
                let buf_len = prime_sizes[step % prime_sizes.len()];
                step += 1;
                let mut buf = vec![0u8; buf_len];
                match backend.poll_read(&mut buf) {
                    Ok(0) => {
                        assert!(!backend.is_closed());
                        thread::yield_now();
                    }
                    Ok(n) => {
                        actual.extend_from_slice(&buf[..n]);
                        if backend.pending_len() > 0 {
                            assert!(
                                !backend.is_closed(),
                                "is_closed must remain false while pending_len() > 0"
                            );
                        }
                    }
                    Err(err) => panic!("unexpected poll_read error before EOF: {err}"),
                }
            }
            assert_eq!(actual, expected);
            assert!(stats.recycled_returns.load(Ordering::Relaxed) >= 4);

            reader_ctrl.push(ReadStep::Interrupted);
            reader_ctrl.push(ReadStep::Data(b"TAIL".to_vec()));
            expected.extend_from_slice(b"TAIL");
            reader_ctrl.push(ReadStep::Eof);
            wait_repaint(&repaint_sig, 7, Duration::from_secs(2));

            loop {
                if backend.is_closed() {
                    assert_eq!(backend.poll_read(&mut []), Err(TerminalIoError::Closed));
                    assert_eq!(
                        backend.poll_read(&mut [0u8; 8]),
                        Err(TerminalIoError::Closed)
                    );
                    break;
                }
                assert_eq!(backend.poll_read(&mut []), Ok(0));
                let mut buf = [0u8; 3];
                match backend.poll_read(&mut buf) {
                    Ok(0) => thread::yield_now(),
                    Ok(n) => actual.extend_from_slice(&buf[..n]),
                    Err(TerminalIoError::Closed) => break,
                    Err(err) => panic!("unexpected poll_read error: {err}"),
                }
            }

            assert_eq!(actual, expected);
            assert!(backend.is_closed());
            assert_eq!(backend.poll_read(&mut []), Err(TerminalIoError::Closed));
        }

        #[test]
        fn drop_completes_quickly_with_pending_or_stalled_writer() {
            // Case A: pending small writes flush cleanly before drop when unblocked.
            let TestBackendHarness {
                backend: mut backend_a,
                writer: writer_a,
                child: child_a,
                ..
            } = spawn_test_backend(ExitMonitorMode::DirectOnEof, false);
            backend_a.write_bytes(b"hello_drop").unwrap();
            let written = writer_a.wait_written_len(10, Duration::from_secs(2));
            assert_eq!(written, b"hello_drop");
            drop(backend_a);
            assert!(child_a.was_killed());

            // Case B: permanently blocked FakeWriter::write must NOT hang Drop.
            let TestBackendHarness {
                backend: mut backend_b,
                writer: writer_b,
                child: child_b,
                master_dropped: master_dropped_b,
                ..
            } = spawn_test_backend(ExitMonitorMode::DirectOnEof, true);
            backend_b.write_bytes(b"stalled_payload").unwrap();

            let start = Instant::now();
            drop(backend_b);
            assert!(
                start.elapsed() < Duration::from_millis(200),
                "Drop took too long with stalled writer: {:?}",
                start.elapsed()
            );
            assert!(child_b.was_killed());
            assert!(master_dropped_b.load(Ordering::SeqCst));

            // Release FakeWriter after Drop so the detached test writer thread can exit cleanly.
            writer_b.set_blocked(false);
        }

        #[test]
        fn reader_error_drains_buffered_bytes_before_surfacing_and_closing() {
            let TestBackendHarness {
                mut backend,
                reader: reader_ctrl,
                child: child_ctrl,
                repaint_sig,
                ..
            } = spawn_test_backend(ExitMonitorMode::DirectOnEof, false);

            child_ctrl.set_exited(3);
            reader_ctrl.push(ReadStep::Data(b"BEFORE_ERR".to_vec()));
            reader_ctrl.push(ReadStep::Error(ErrorKind::Other, "device read fault"));
            wait_repaint(&repaint_sig, 2, Duration::from_secs(2));

            let mut out = [0u8; 32];
            assert_eq!(backend.poll_read(&mut out), Ok(10));
            assert_eq!(&out[..10], b"BEFORE_ERR");
            assert!(!backend.is_closed());

            let err = backend.poll_read(&mut out).unwrap_err();
            assert!(
                matches!(err, TerminalIoError::Message(ref msg) if msg.contains("device read fault")),
                "expected Message error, got {err:?}"
            );
            assert!(backend.is_closed());
            assert_eq!(backend.try_wait().unwrap(), Some(3));
        }

        #[test]
        fn wasm_stub_contract_parity() {
            // Compile-time signature parity checks between native and wasm_stub items.
            let _spawn_native: fn(PtyConfig) -> Result<PtyBackend, TerminalIoError> =
                PtyBackend::spawn;
            let _spawn_wasm: fn(
                crate::wasm_stub::PtyConfig,
            )
                -> Result<crate::wasm_stub::PtyBackend, TerminalIoError> =
                crate::wasm_stub::PtyBackend::spawn;
            let _spawn_repaint_wasm: fn(
                crate::wasm_stub::PtyConfig,
                crate::wasm_stub::RepaintCallback,
            )
                -> Result<crate::wasm_stub::PtyBackend, TerminalIoError> =
                crate::wasm_stub::PtyBackend::spawn_with_repaint;

            let cfg = crate::wasm_stub::PtyConfig {
                shell: Some("sh".to_owned()),
                args: vec!["-c".to_owned(), "echo hi".to_owned()],
                cwd: None,
                env: vec![("TERM".to_owned(), "xterm-256color".to_owned())],
                initial_size: TerminalSize::new(100, 40),
            };
            let err = crate::wasm_stub::PtyBackend::spawn(cfg.clone()).unwrap_err();
            assert!(!err.is_closed());
            assert!(!err.to_string().is_empty());

            let err2 =
                crate::wasm_stub::PtyBackend::spawn_with_repaint(cfg, Box::new(|| {})).unwrap_err();
            assert!(!err2.is_closed());

            let mut stub = crate::wasm_stub::PtyBackend::new_for_test(TerminalSize::new(80, 24));
            assert_eq!(stub.current_size(), TerminalSize::new(80, 24));
            stub.set_repaint_callback(Some(Box::new(|| {})));
            stub.set_repaint_callback(None);
            assert!(stub.is_closed());
            assert!(matches!(stub.try_wait(), Err(TerminalIoError::Message(_))));
            let mut buf = [0u8; 16];
            assert!(matches!(
                stub.poll_read(&mut buf),
                Err(TerminalIoError::Message(_))
            ));
            assert!(matches!(
                stub.write_bytes(b"hi"),
                Err(TerminalIoError::Message(_))
            ));
            assert!(matches!(
                stub.resize(TerminalSize::new(90, 30)),
                Err(TerminalIoError::Message(_))
            ));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub use native::PtyMaster;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{PtyBackend, PtyConfig};
