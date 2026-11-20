//! Desktop PTY backend for `egui_xterm` implementing [`TerminalInput`] via `portable-pty`.
//!
//! On `wasm32` targets only the core input contract types ([`TerminalInput`],
//! [`TerminalIoError`], [`TerminalSize`]) are re-exported so crates depending on
//! `egui_xterm` remain `wasm32-unknown-unknown` compatible.

#![forbid(unsafe_code)]

pub use egui_xterm_core::{TerminalInput, TerminalIoError, TerminalSize};

#[cfg(not(target_arch = "wasm32"))]
pub mod shell;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::fmt;
    use std::io::{ErrorKind, Read, Write};
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::time::Duration;

    use egui_xterm_core::{TerminalInput, TerminalIoError, TerminalSize};
    use portable_pty::{Child, MasterPty, PtySize, native_pty_system};

    use crate::shell;

    /// Bounded number of chunk messages buffered between the PTY reader thread and UI thread.
    const READER_CHANNEL_CAPACITY: usize = 256;
    /// Read buffer size per OS pipe read on the background PTY reader thread.
    const READER_CHUNK_BYTES: usize = 16 * 1024;
    /// Maximum time [`PtyBackend::drop`] waits for the reader thread to observe EOF before detaching.
    const DROP_JOIN_WAIT: Duration = Duration::from_millis(50);
    /// Safe upper bound for PTY columns and rows.
    ///
    /// Windows `ConPTY` crashes the pseudoconsole (`0xC000013A`) on `ResizePseudoConsole`
    /// when either dimension reaches `i16::MAX` (`32767`); `4096` stays well below that
    /// threshold while accommodating any practical terminal viewport.
    const MAX_PTY_DIM: u16 = 4096;

    /// Callback invoked from the background PTY reader thread when output arrives or EOF occurs.
    pub type RepaintCallback = Box<dyn Fn() + Send + Sync>;

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

    #[derive(Debug)]
    enum ReaderEvent {
        Data(Vec<u8>),
        Error(String),
        Eof,
    }

    struct ReaderDoneSignal {
        state: Arc<(Mutex<bool>, Condvar)>,
    }

    impl Drop for ReaderDoneSignal {
        fn drop(&mut self) {
            let (lock, cvar) = &*self.state;
            if let Ok(mut done) = lock.lock() {
                *done = true;
                cvar.notify_all();
            }
        }
    }

    type SharedRepaint = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;
    /// Shared PTY stdin handle.
    ///
    /// On Windows, `reader_loop` acquires this lock at most once during `ConPTY` startup to
    /// send the 6-byte `\x1b[1;1R` CPR reply. Callers of `write_bytes` are assumed not to
    /// saturate the OS input pipe before initial output drains; `Drop` avoids deadlock by
    /// closing `writer`/`master` and bounding its join wait to `DROP_JOIN_WAIT`.
    type SharedWriter = Arc<Mutex<Option<Box<dyn Write + Send>>>>;

    /// Native desktop PTY session implementing non-blocking [`TerminalInput`].
    ///
    /// A background reader thread forwards child output through a bounded channel so
    /// [`TerminalInput::poll_read`] never blocks the UI thread, and triggers an optional
    /// [`RepaintCallback`] whenever new bytes arrive or the child exits.
    pub struct PtyBackend {
        master: Option<Box<dyn MasterPty + Send>>,
        writer: SharedWriter,
        child: Option<Box<dyn Child + Send + Sync>>,
        rx: Option<Receiver<ReaderEvent>>,
        pending: Vec<u8>,
        pending_offset: usize,
        pending_error: Option<String>,
        closed: bool,
        exit_code: Option<u32>,
        last_size: TerminalSize,
        repaint: SharedRepaint,
        reader_done: Arc<(Mutex<bool>, Condvar)>,
    }

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

            let (tx, rx) = mpsc::sync_channel::<ReaderEvent>(READER_CHANNEL_CAPACITY);
            let repaint_slot: SharedRepaint = Arc::new(Mutex::new(repaint.map(Arc::from)));
            let shared_writer: SharedWriter = Arc::new(Mutex::new(Some(writer)));
            let reader_done = Arc::new((Mutex::new(false), Condvar::new()));

            let thread_repaint = Arc::clone(&repaint_slot);
            let thread_writer = Arc::clone(&shared_writer);
            let thread_done = Arc::clone(&reader_done);
            thread::Builder::new()
                .name("egui_xterm_pty_reader".to_owned())
                .spawn(move || {
                    let _done_guard = ReaderDoneSignal { state: thread_done };
                    reader_loop(reader, &tx, &thread_repaint, &thread_writer);
                })
                .map_err(|err| {
                    TerminalIoError::message(format!("failed to spawn PTY reader thread: {err}"))
                })?;

            Ok(Self {
                master: Some(pair.master),
                writer: shared_writer,
                child: Some(child),
                rx: Some(rx),
                pending: Vec::new(),
                pending_offset: 0,
                pending_error: None,
                closed: false,
                exit_code: None,
                last_size: clamped,
                repaint: repaint_slot,
                reader_done,
            })
        }

        /// Replaces or clears the repaint callback invoked when PTY output or EOF arrives.
        pub fn set_repaint_callback(&self, repaint: Option<RepaintCallback>) {
            if let Ok(mut guard) = self.repaint.lock() {
                *guard = repaint.map(Arc::from);
            }
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
            let Some(child) = self.child.as_mut() else {
                return Ok(self.exit_code);
            };
            match child.try_wait() {
                Ok(Some(status)) => {
                    let code = status.exit_code();
                    self.on_child_exited(code);
                    Ok(Some(code))
                }
                Ok(None) => Ok(None),
                Err(err) => Err(TerminalIoError::message(format!(
                    "failed to query child process status: {err}"
                ))),
            }
        }

        fn on_child_exited(&mut self, code: u32) {
            self.exit_code = Some(code);
            // On Windows ConPTY, `conhost.exe` keeps the output pipe open until
            // `ClosePseudoConsole` (`MasterPty::drop`) runs. Releasing `writer` and
            // `master` once the child has exited flushes remaining output and signals EOF.
            #[cfg(windows)]
            {
                if let Ok(mut guard) = self.writer.lock() {
                    drop(guard.take());
                }
                drop(self.master.take());
            }
        }

        fn poll_child_exit(&mut self) {
            if self.exit_code.is_some() {
                return;
            }
            let _ = self.try_wait();
        }

        fn pending_len(&self) -> usize {
            self.pending.len().saturating_sub(self.pending_offset)
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

            if !self.closed {
                self.poll_child_exit();
            }

            let mut written = 0usize;
            while written < buf.len() {
                if self.pending_offset < self.pending.len() {
                    let avail = self.pending.len() - self.pending_offset;
                    let to_copy = avail.min(buf.len() - written);
                    buf[written..written + to_copy].copy_from_slice(
                        &self.pending[self.pending_offset..self.pending_offset + to_copy],
                    );
                    self.pending_offset += to_copy;
                    written += to_copy;
                    if self.pending_offset >= self.pending.len() {
                        self.pending.clear();
                        self.pending_offset = 0;
                    }
                } else {
                    if self.closed || self.pending_error.is_some() {
                        break;
                    }
                    let Some(rx) = self.rx.as_ref() else {
                        self.closed = true;
                        break;
                    };
                    match rx.try_recv() {
                        Ok(ReaderEvent::Data(chunk)) => {
                            if !chunk.is_empty() {
                                self.pending = chunk;
                                self.pending_offset = 0;
                            }
                        }
                        Ok(ReaderEvent::Eof) | Err(TryRecvError::Disconnected) => {
                            self.closed = true;
                            break;
                        }
                        Ok(ReaderEvent::Error(msg)) => {
                            self.pending_error = Some(msg);
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
            let Ok(mut guard) = self.writer.lock() else {
                return Err(TerminalIoError::Closed);
            };
            let Some(writer) = guard.as_mut() else {
                return Err(TerminalIoError::Closed);
            };
            if let Err(err) = writer.write_all(bytes).and_then(|()| writer.flush()) {
                let io_err = TerminalIoError::from(err);
                if io_err.is_closed() {
                    *guard = None;
                }
                return Err(io_err);
            }
            Ok(())
        }

        fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalIoError> {
            if self.is_closed() {
                return Err(TerminalIoError::Closed);
            }
            let clamped = clamp_size(size);
            if clamped == self.last_size {
                return Ok(());
            }
            let Some(master) = self.master.as_ref() else {
                return Err(TerminalIoError::Closed);
            };
            master
                .resize(to_pty_size(clamped))
                .map_err(|err| TerminalIoError::message(format!("failed to resize PTY: {err}")))?;
            self.last_size = clamped;
            Ok(())
        }

        fn is_closed(&self) -> bool {
            self.closed && self.pending_len() == 0 && self.pending_error.is_none()
        }
    }

    impl Drop for PtyBackend {
        fn drop(&mut self) {
            // 1. Drop receiver first so any blocked `sync_channel::send` unblocks immediately.
            drop(self.rx.take());
            // 2. Close writer pipe.
            if let Ok(mut guard) = self.writer.lock() {
                drop(guard.take());
            }
            // 3. Terminate direct child if still running without blocking on grandchildren.
            if let Some(mut child) = self.child.take()
                && matches!(child.try_wait(), Ok(None))
            {
                let _ = child.kill();
                let _ = child.try_wait();
            }
            // 4. Close master PTY (invokes ClosePseudoConsole on Windows / closes master fd on Unix).
            drop(self.master.take());
            // 5. Wait briefly for clean reader exit, then detach so Drop never hangs on grandchildren.
            let (lock, cvar) = &*self.reader_done;
            if let Ok(guard) = lock.lock()
                && !*guard
            {
                let _ = cvar.wait_timeout_while(guard, DROP_JOIN_WAIT, |done| !*done);
            }
        }
    }

    fn reader_loop(
        mut reader: Box<dyn Read + Send>,
        tx: &SyncSender<ReaderEvent>,
        repaint: &SharedRepaint,
        writer: &SharedWriter,
    ) {
        #[cfg(not(windows))]
        let _ = writer;

        let mut buf = vec![0u8; READER_CHUNK_BYTES];
        #[cfg(windows)]
        let mut conpty_handshake_pending = true;
        #[cfg(windows)]
        let mut handshake_buf = Vec::<u8>::with_capacity(8);

        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    #[cfg(windows)]
                    if !handshake_buf.is_empty() {
                        let _ = tx.send(ReaderEvent::Data(std::mem::take(&mut handshake_buf)));
                    }
                    let _ = tx.send(ReaderEvent::Eof);
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
                            if let Ok(mut guard) = writer.lock()
                                && let Some(w) = guard.as_mut()
                            {
                                let _ = w.write_all(b"\x1b[1;1R").and_then(|()| w.flush());
                            }
                            let rest = handshake_buf.split_off(CPR_QUERY.len());
                            handshake_buf.clear();
                            if !rest.is_empty() {
                                if tx.send(ReaderEvent::Data(rest)).is_err() {
                                    break;
                                }
                                invoke_repaint(repaint);
                            }
                            continue;
                        }
                        if CPR_QUERY.starts_with(&handshake_buf) {
                            continue;
                        }
                        conpty_handshake_pending = false;
                        let forward = std::mem::take(&mut handshake_buf);
                        if tx.send(ReaderEvent::Data(forward)).is_err() {
                            break;
                        }
                        invoke_repaint(repaint);
                        continue;
                    }

                    if tx.send(ReaderEvent::Data(buf[..n].to_vec())).is_err() {
                        break;
                    }
                    invoke_repaint(repaint);
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err)
                    if matches!(err.kind(), ErrorKind::BrokenPipe | ErrorKind::UnexpectedEof) =>
                {
                    #[cfg(windows)]
                    if !handshake_buf.is_empty() {
                        let _ = tx.send(ReaderEvent::Data(std::mem::take(&mut handshake_buf)));
                    }
                    let _ = tx.send(ReaderEvent::Eof);
                    invoke_repaint(repaint);
                    break;
                }
                Err(err) => {
                    #[cfg(windows)]
                    if !handshake_buf.is_empty() {
                        let _ = tx.send(ReaderEvent::Data(std::mem::take(&mut handshake_buf)));
                    }
                    let _ = tx.send(ReaderEvent::Error(err.to_string()));
                    invoke_repaint(repaint);
                    break;
                }
            }
        }
    }

    fn invoke_repaint(repaint: &SharedRepaint) {
        let cb = repaint
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned());
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
        use super::*;

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
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{PtyBackend, PtyConfig, RepaintCallback};
