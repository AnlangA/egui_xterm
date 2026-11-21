//! Input stream traits and built-in adapters for driving a terminal session.
//!
//! Consumers implement [`TerminalInput::poll_read`] (the single required method)
//! to connect any non-blocking byte source—native PTYs, channels, ring buffers,
//! `WebSockets`, or mock streams—without `Send + Sync` constraints.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::sync::mpsc::{self, TryRecvError};

/// Terminal grid and optional pixel dimensions passed during resize notifications.
///
/// # Examples
///
/// ```
/// use egui_xterm_core::TerminalSize;
///
/// let size = TerminalSize::with_pixels(120, 40, 960, 640);
/// assert_eq!((size.cols, size.rows), (120, 40));
/// assert_eq!((size.pixel_width, size.pixel_height), (960, 640));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TerminalSize {
    /// Number of visible character columns.
    pub cols: u16,
    /// Number of visible character rows.
    pub rows: u16,
    /// Viewport width in physical pixels, or `0` if unspecified.
    pub pixel_width: u16,
    /// Viewport height in physical pixels, or `0` if unspecified.
    pub pixel_height: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self::new(80, 24)
    }
}

impl TerminalSize {
    /// Creates a character-grid size with pixel dimensions set to `0`.
    #[must_use]
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    /// Creates a size with both grid and physical pixel dimensions.
    #[must_use]
    pub const fn with_pixels(cols: u16, rows: u16, pixel_width: u16, pixel_height: u16) -> Self {
        Self {
            cols,
            rows,
            pixel_width,
            pixel_height,
        }
    }
}

/// Error returned by [`TerminalInput`] read, write, or resize operations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[must_use]
pub enum TerminalIoError {
    /// The underlying input stream has permanently ended (EOF or disconnected).
    Closed,
    /// A backend-specific I/O or transport error.
    Message(String),
}

impl TerminalIoError {
    /// Constructs a [`TerminalIoError::Message`] from any string-like value.
    pub fn message(msg: impl Into<String>) -> Self {
        Self::Message(msg.into())
    }

    /// Returns `true` if this error represents a permanently closed stream.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(self, Self::Closed)
    }
}

impl fmt::Display for TerminalIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("terminal input closed"),
            Self::Message(msg) => {
                if msg.is_empty() {
                    f.write_str("terminal I/O error")
                } else {
                    f.write_str(msg)
                }
            }
        }
    }
}

impl std::error::Error for TerminalIoError {}

impl From<io::Error> for TerminalIoError {
    fn from(err: io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof => Self::Closed,
            _ => Self::Message(err.to_string()),
        }
    }
}

/// Non-blocking byte stream and optional bidirectional control interface for a terminal.
///
/// Only [`TerminalInput::poll_read`] is required. Implementors that do not accept
/// keyboard input or resize notifications may leave [`write_bytes`](Self::write_bytes),
/// [`resize`](Self::resize), and [`is_closed`](Self::is_closed) at their default no-op
/// implementations.
///
/// This trait intentionally has no `Send` or `Sync` bound so UI-thread-affine and
/// `wasm32-unknown-unknown` sources can implement it directly.
///
/// # Examples
///
/// Implement only [`TerminalInput::poll_read`] for a read-only source:
///
/// ```
/// use egui_xterm_core::{TerminalInput, TerminalIoError, TerminalSize};
///
/// struct StaticBanner {
///     bytes: &'static [u8],
///     offset: usize,
/// }
///
/// impl TerminalInput for StaticBanner {
///     fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
///         let remaining = &self.bytes[self.offset..];
///         if remaining.is_empty() {
///             return Err(TerminalIoError::Closed);
///         }
///         let n = remaining.len().min(buf.len());
///         buf[..n].copy_from_slice(&remaining[..n]);
///         self.offset += n;
///         Ok(n)
///     }
/// }
///
/// let mut src = StaticBanner {
///     bytes: b"hi",
///     offset: 0,
/// };
/// let mut buf = [0u8; 4];
/// assert_eq!(src.poll_read(&mut buf), Ok(2));
/// assert_eq!(&buf[..2], b"hi");
/// assert_eq!(src.write_bytes(b"ignored"), Ok(()));
/// assert_eq!(src.resize(TerminalSize::new(80, 24)), Ok(()));
/// assert!(!src.is_closed());
/// assert_eq!(src.poll_read(&mut buf), Err(TerminalIoError::Closed));
/// ```
pub trait TerminalInput {
    /// Reads available incoming VT/ANSI bytes into `buf` without blocking.
    ///
    /// Returns `Ok(0)` when no bytes are currently available (or when `buf` is empty
    /// and the stream remains open), `Ok(n)` for `1..=buf.len()` bytes copied, or
    /// `Err(TerminalIoError::Closed)` once the stream has permanently ended and all
    /// buffered bytes have been drained.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalIoError::Closed`] on EOF/disconnect or
    /// [`TerminalIoError::Message`] on backend transport failure.
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError>;

    /// Sends outgoing bytes (keystrokes, pasted text, mouse reports, VT responses).
    ///
    /// The default implementation ignores all outgoing bytes and returns `Ok(())`.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalIoError`] if the backend rejects or fails to transmit bytes.
    fn write_bytes(&mut self, _bytes: &[u8]) -> Result<(), TerminalIoError> {
        Ok(())
    }

    /// Notifies the backend that the terminal viewport dimensions changed.
    ///
    /// The default implementation is a no-op returning `Ok(())`.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalIoError`] if the backend cannot apply the new size.
    fn resize(&mut self, _size: TerminalSize) -> Result<(), TerminalIoError> {
        Ok(())
    }

    /// Returns `true` once the input stream has permanently closed and all buffered data
    /// has been drained.
    fn is_closed(&self) -> bool {
        false
    }
}

impl<T: TerminalInput + ?Sized> TerminalInput for Box<T> {
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
        (**self).poll_read(buf)
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
        (**self).write_bytes(bytes)
    }

    fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalIoError> {
        (**self).resize(size)
    }

    fn is_closed(&self) -> bool {
        (**self).is_closed()
    }
}

impl<T: TerminalInput + ?Sized> TerminalInput for &mut T {
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
        (**self).poll_read(buf)
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
        (**self).write_bytes(bytes)
    }

    fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalIoError> {
        (**self).resize(size)
    }

    fn is_closed(&self) -> bool {
        (**self).is_closed()
    }
}

/// Adapts any `FnMut(&mut [u8]) -> Result<usize, TerminalIoError>` closure as a [`TerminalInput`].
///
/// # Examples
///
/// ```
/// use egui_xterm_core::{FnInput, TerminalInput, TerminalIoError};
///
/// let mut sent = false;
/// let mut io = FnInput::new(|buf: &mut [u8]| {
///     if sent {
///         return Err(TerminalIoError::Closed);
///     }
///     sent = true;
///     let n = buf.len().min(2);
///     buf[..n].copy_from_slice(&b"ok"[..n]);
///     Ok(n)
/// });
///
/// let mut buf = [0u8; 4];
/// assert_eq!(io.poll_read(&mut buf), Ok(2));
/// assert_eq!(&buf[..2], b"ok");
/// assert_eq!(io.poll_read(&mut buf), Err(TerminalIoError::Closed));
/// ```
pub struct FnInput<F> {
    func: F,
}

impl<F> fmt::Debug for FnInput<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FnInput").finish_non_exhaustive()
    }
}

impl<F> FnInput<F> {
    /// Wraps a non-blocking read closure in a [`FnInput`].
    #[must_use]
    pub const fn new(func: F) -> Self {
        Self { func }
    }

    /// Unwraps and returns the underlying closure.
    #[must_use]
    pub fn into_inner(self) -> F {
        self.func
    }
}

impl<F> TerminalInput for FnInput<F>
where
    F: FnMut(&mut [u8]) -> Result<usize, TerminalIoError>,
{
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
        (self.func)(buf)
    }
}

/// Non-blocking [`std::sync::mpsc`] channel adapter implementing [`TerminalInput`].
///
/// Coalesces partial chunk reads across calls and guarantees all buffered chunks
/// are drained before reporting [`TerminalIoError::Closed`].
///
/// # Examples
///
/// ```
/// use egui_xterm_core::{ChannelInput, TerminalInput};
///
/// let (to_term, from_term, mut io) = ChannelInput::duplex_pair();
/// to_term.send(b"pong".to_vec())?;
///
/// let mut buf = [0u8; 8];
/// assert_eq!(io.poll_read(&mut buf)?, 4);
/// assert_eq!(&buf[..4], b"pong");
///
/// io.write_bytes(b"ping\r")?;
/// assert_eq!(from_term.recv()?, b"ping\r");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct ChannelInput {
    rx: mpsc::Receiver<Vec<u8>>,
    tx: Option<mpsc::Sender<Vec<u8>>>,
    on_resize: Option<Box<dyn FnMut(TerminalSize)>>,
    pending: Vec<u8>,
    pending_offset: usize,
    closed: bool,
}

impl fmt::Debug for ChannelInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelInput")
            .field("pending_len", &(self.pending.len() - self.pending_offset))
            .field("has_tx", &self.tx.is_some())
            .field("has_resize_callback", &self.on_resize.is_some())
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl ChannelInput {
    /// Creates a read-only [`ChannelInput`] fed by `rx`.
    #[must_use]
    pub fn new(rx: mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            rx,
            tx: None,
            on_resize: None,
            pending: Vec::new(),
            pending_offset: 0,
            closed: false,
        }
    }

    /// Creates a bidirectional [`ChannelInput`] with incoming `rx` and outgoing `tx`.
    #[must_use]
    pub fn bidirectional(rx: mpsc::Receiver<Vec<u8>>, tx: mpsc::Sender<Vec<u8>>) -> Self {
        Self::new(rx).with_sender(tx)
    }

    /// Attaches an outgoing sender for [`TerminalInput::write_bytes`].
    #[must_use]
    pub fn with_sender(mut self, tx: mpsc::Sender<Vec<u8>>) -> Self {
        self.tx = Some(tx);
        self
    }

    /// Attaches a callback invoked on [`TerminalInput::resize`].
    #[must_use]
    pub fn with_resize_callback<F>(mut self, f: F) -> Self
    where
        F: FnMut(TerminalSize) + 'static,
    {
        self.on_resize = Some(Box::new(f));
        self
    }

    /// Creates a `(sender, ChannelInput)` pair for feeding bytes into the terminal.
    #[must_use]
    pub fn pair() -> (mpsc::Sender<Vec<u8>>, Self) {
        let (tx, rx) = mpsc::channel();
        (tx, Self::new(rx))
    }

    /// Creates a duplex `(to_terminal_tx, from_terminal_rx, ChannelInput)` tuple.
    #[must_use]
    pub fn duplex_pair() -> (mpsc::Sender<Vec<u8>>, mpsc::Receiver<Vec<u8>>, Self) {
        let (in_tx, in_rx) = mpsc::channel();
        let (out_tx, out_rx) = mpsc::channel();
        (in_tx, out_rx, Self::bidirectional(in_rx, out_tx))
    }
}

impl TerminalInput for ChannelInput {
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
                if self.closed {
                    break;
                }
                match self.rx.try_recv() {
                    Ok(chunk) => {
                        if !chunk.is_empty() {
                            self.pending = chunk;
                            self.pending_offset = 0;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.closed = true;
                        break;
                    }
                }
            }
        }

        if written > 0 {
            Ok(written)
        } else if self.closed {
            Err(TerminalIoError::Closed)
        } else {
            Ok(0)
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
        if bytes.is_empty() {
            return Ok(());
        }
        if let Some(tx) = &self.tx {
            tx.send(bytes.to_vec())
                .map_err(|_| TerminalIoError::Closed)?;
        }
        Ok(())
    }

    fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalIoError> {
        if let Some(callback) = &mut self.on_resize {
            callback(size);
        }
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed && self.pending_offset >= self.pending.len()
    }
}

type WriteHook = Box<dyn FnMut(&[u8]) -> Result<(), TerminalIoError>>;
type ResizeHook = Box<dyn FnMut(TerminalSize) -> Result<(), TerminalIoError>>;

/// In-memory FIFO ring buffer implementing [`TerminalInput`].
///
/// Useful for `WebSockets`, in-process virtual shells, FFI bridges, and deterministic tests.
///
/// # Examples
///
/// ```
/// use egui_xterm_core::{RingBufferInput, TerminalInput, TerminalIoError};
///
/// let mut io = RingBufferInput::from_bytes(b"hello");
/// let mut buf = [0u8; 8];
/// assert_eq!(io.poll_read(&mut buf)?, 5);
/// assert_eq!(&buf[..5], b"hello");
///
/// io.write_bytes(b"ls\r")?;
/// assert_eq!(io.drain_written(), b"ls\r");
///
/// io.close();
/// assert!(io.is_closed());
/// assert_eq!(io.poll_read(&mut buf), Err(TerminalIoError::Closed));
/// # Ok::<(), TerminalIoError>(())
/// ```
pub struct RingBufferInput {
    rx_buf: VecDeque<u8>,
    tx_buf: Vec<u8>,
    on_write: Option<WriteHook>,
    on_resize: Option<ResizeHook>,
    closed: bool,
    last_size: Option<TerminalSize>,
}

impl fmt::Debug for RingBufferInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RingBufferInput")
            .field("rx_len", &self.rx_buf.len())
            .field("tx_len", &self.tx_buf.len())
            .field("has_write_hook", &self.on_write.is_some())
            .field("has_resize_hook", &self.on_resize.is_some())
            .field("closed", &self.closed)
            .field("last_size", &self.last_size)
            .finish_non_exhaustive()
    }
}

impl Default for RingBufferInput {
    fn default() -> Self {
        Self::new()
    }
}

impl RingBufferInput {
    /// Creates an empty [`RingBufferInput`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            rx_buf: VecDeque::new(),
            tx_buf: Vec::new(),
            on_write: None,
            on_resize: None,
            closed: false,
            last_size: None,
        }
    }

    /// Creates an empty [`RingBufferInput`] with preallocated incoming capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            rx_buf: VecDeque::with_capacity(capacity),
            tx_buf: Vec::new(),
            on_write: None,
            on_resize: None,
            closed: false,
            last_size: None,
        }
    }

    /// Creates a [`RingBufferInput`] preloaded with `bytes`.
    #[must_use]
    pub fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let slice = bytes.as_ref();
        let mut input = Self::with_capacity(slice.len());
        input.push(slice);
        input
    }

    /// Appends incoming bytes to be read by [`TerminalInput::poll_read`].
    pub fn push(&mut self, bytes: &[u8]) {
        self.rx_buf.extend(bytes);
    }

    /// Appends UTF-8 text bytes to be read by [`TerminalInput::poll_read`].
    pub fn push_str(&mut self, text: &str) {
        self.push(text.as_bytes());
    }

    /// Marks the stream as permanently closed once remaining buffered bytes are drained.
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// Takes all outgoing bytes recorded by [`TerminalInput::write_bytes`] when no write
    /// hook is installed.
    pub fn drain_written(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx_buf)
    }

    /// Returns the number of unread incoming bytes currently buffered.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.rx_buf.len()
    }

    /// Returns the most recent size passed to [`TerminalInput::resize`], if any.
    #[must_use]
    pub const fn last_size(&self) -> Option<TerminalSize> {
        self.last_size
    }

    /// Installs a custom write hook replacing default buffering in `tx_buf`.
    #[must_use]
    pub fn with_write_hook<F>(mut self, hook: F) -> Self
    where
        F: FnMut(&[u8]) -> Result<(), TerminalIoError> + 'static,
    {
        self.on_write = Some(Box::new(hook));
        self
    }

    /// Installs a custom resize hook invoked after updating `last_size`.
    #[must_use]
    pub fn with_resize_hook<F>(mut self, hook: F) -> Self
    where
        F: FnMut(TerminalSize) -> Result<(), TerminalIoError> + 'static,
    {
        self.on_resize = Some(Box::new(hook));
        self
    }
}

impl TerminalInput for RingBufferInput {
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
        if buf.is_empty() {
            return if self.is_closed() {
                Err(TerminalIoError::Closed)
            } else {
                Ok(0)
            };
        }
        if self.rx_buf.is_empty() {
            return if self.closed {
                Err(TerminalIoError::Closed)
            } else {
                Ok(0)
            };
        }

        let (first, second) = self.rx_buf.as_slices();
        let n1 = first.len().min(buf.len());
        buf[..n1].copy_from_slice(&first[..n1]);

        let remaining = buf.len() - n1;
        let n2 = if remaining > 0 {
            let count = second.len().min(remaining);
            buf[n1..n1 + count].copy_from_slice(&second[..count]);
            count
        } else {
            0
        };

        let total = n1 + n2;
        self.rx_buf.drain(..total);
        Ok(total)
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
        if self.closed {
            return Err(TerminalIoError::Closed);
        }
        if let Some(hook) = &mut self.on_write {
            hook(bytes)
        } else {
            self.tx_buf.extend_from_slice(bytes);
            Ok(())
        }
    }

    fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalIoError> {
        self.last_size = Some(size);
        if let Some(hook) = &mut self.on_resize {
            hook(size)
        } else {
            Ok(())
        }
    }

    fn is_closed(&self) -> bool {
        self.closed && self.rx_buf.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_size_constructors_and_default() {
        assert_eq!(
            TerminalSize::default(),
            TerminalSize {
                cols: 80,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            }
        );
        assert_eq!(
            TerminalSize::new(120, 40),
            TerminalSize::with_pixels(120, 40, 0, 0)
        );
        let px = TerminalSize::with_pixels(100, 30, 800, 480);
        assert_eq!(px.cols, 100);
        assert_eq!(px.rows, 30);
        assert_eq!(px.pixel_width, 800);
        assert_eq!(px.pixel_height, 480);
    }

    #[test]
    fn terminal_io_error_display_and_std_error() {
        let closed = TerminalIoError::Closed;
        assert!(closed.is_closed());
        assert_eq!(closed.to_string(), "terminal input closed");

        let msg = TerminalIoError::message("pipe stalled");
        assert!(!msg.is_closed());
        assert_eq!(msg.to_string(), "pipe stalled");

        let empty_msg = TerminalIoError::message("");
        assert_eq!(empty_msg.to_string(), "terminal I/O error");

        let err_ref: &dyn std::error::Error = &msg;
        assert!(err_ref.source().is_none());

        let eof = TerminalIoError::from(io::Error::new(io::ErrorKind::UnexpectedEof, "eof"));
        assert_eq!(eof, TerminalIoError::Closed);

        let broken = TerminalIoError::from(io::Error::new(io::ErrorKind::BrokenPipe, "broken"));
        assert_eq!(broken, TerminalIoError::Closed);

        let other = TerminalIoError::from(io::Error::other("custom failure"));
        assert!(matches!(other, TerminalIoError::Message(s) if s.contains("custom failure")));
    }

    #[test]
    fn trait_defaults_and_blanket_impls() {
        struct MinimalSource {
            data: &'static [u8],
            pos: usize,
        }

        impl TerminalInput for MinimalSource {
            fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
                let rem = &self.data[self.pos..];
                let n = rem.len().min(buf.len());
                buf[..n].copy_from_slice(&rem[..n]);
                self.pos += n;
                Ok(n)
            }
        }

        let mut src = MinimalSource {
            data: b"hello",
            pos: 0,
        };
        assert_eq!(src.write_bytes(b"ignored"), Ok(()));
        assert_eq!(src.resize(TerminalSize::new(90, 30)), Ok(()));
        assert!(!src.is_closed());

        let ref_src: &mut MinimalSource = &mut src;
        let mut buf = [0u8; 3];
        assert_eq!(TerminalInput::poll_read(&mut *ref_src, &mut buf), Ok(3));
        assert_eq!(&buf, b"hel");
        assert_eq!(TerminalInput::write_bytes(&mut *ref_src, b"x"), Ok(()));
        assert_eq!(
            TerminalInput::resize(&mut *ref_src, TerminalSize::default()),
            Ok(())
        );
        assert!(!TerminalInput::is_closed(&ref_src));

        let mut boxed: Box<dyn TerminalInput> = Box::new(src);
        let mut tail = [0u8; 4];
        assert_eq!(boxed.poll_read(&mut tail), Ok(2));
        assert_eq!(&tail[..2], b"lo");
        assert_eq!(boxed.write_bytes(b"y"), Ok(()));
        assert_eq!(boxed.resize(TerminalSize::new(80, 25)), Ok(()));
        assert!(!boxed.is_closed());
    }

    #[test]
    fn fn_input_read_and_close() {
        let mut step = 0usize;
        let mut input = FnInput::new(|buf: &mut [u8]| {
            step += 1;
            match step {
                1 => {
                    buf[..2].copy_from_slice(b"hi");
                    Ok(2)
                }
                2 => Ok(0),
                _ => Err(TerminalIoError::Closed),
            }
        });
        let dbg = format!("{input:?}");
        assert!(dbg.contains("FnInput"));

        let mut buf = [0u8; 8];
        assert_eq!(input.poll_read(&mut buf), Ok(2));
        assert_eq!(&buf[..2], b"hi");
        assert_eq!(input.poll_read(&mut buf), Ok(0));
        assert_eq!(input.poll_read(&mut buf), Err(TerminalIoError::Closed));

        let _inner = input.into_inner();
    }

    #[test]
    fn channel_input_partial_reads_and_coalescing() {
        let (tx, mut input) = ChannelInput::pair();
        tx.send(vec![1, 2, 3, 4, 5]).unwrap();
        tx.send(Vec::new()).unwrap();
        tx.send(vec![6, 7]).unwrap();

        let mut empty = [];
        assert_eq!(input.poll_read(&mut empty), Ok(0));

        let mut buf = [0u8; 2];
        assert_eq!(input.poll_read(&mut buf), Ok(2));
        assert_eq!(buf, [1, 2]);
        assert_eq!(input.poll_read(&mut buf), Ok(2));
        assert_eq!(buf, [3, 4]);
        assert_eq!(input.poll_read(&mut buf), Ok(2));
        assert_eq!(buf, [5, 6]);
        assert_eq!(input.poll_read(&mut buf), Ok(1));
        assert_eq!(buf[..1], [7]);
        assert_eq!(input.poll_read(&mut buf), Ok(0));
    }

    #[test]
    fn channel_input_drain_before_closed() {
        let (tx, mut input) = ChannelInput::pair();
        tx.send(vec![10, 20, 30]).unwrap();
        drop(tx);

        let mut buf = [0u8; 2];
        assert_eq!(input.poll_read(&mut buf), Ok(2));
        assert_eq!(buf, [10, 20]);
        assert!(!input.is_closed());

        assert_eq!(input.poll_read(&mut buf), Ok(1));
        assert_eq!(buf[..1], [30]);
        assert!(input.is_closed());

        assert_eq!(input.poll_read(&mut buf), Err(TerminalIoError::Closed));
    }

    #[test]
    fn channel_input_duplex_write_and_resize() {
        use std::cell::Cell;
        use std::rc::Rc;

        let resized = Rc::new(Cell::new(None));
        let resized_clone = Rc::clone(&resized);

        let (_in_tx, out_rx, input) = ChannelInput::duplex_pair();
        let mut input = input.with_resize_callback(move |sz| resized_clone.set(Some(sz)));

        assert_eq!(input.write_bytes(b""), Ok(()));
        assert_eq!(input.write_bytes(b"ls\r"), Ok(()));
        assert_eq!(out_rx.recv().unwrap(), b"ls\r");

        let sz = TerminalSize::with_pixels(132, 43, 1056, 688);
        assert_eq!(input.resize(sz), Ok(()));
        assert_eq!(resized.get(), Some(sz));

        drop(out_rx);
        assert_eq!(input.write_bytes(b"a"), Err(TerminalIoError::Closed));
    }

    #[test]
    fn ring_buffer_input_wraparound_push_drain_close() {
        let mut ring = RingBufferInput::with_capacity(8);
        ring.push(b"123456");
        let mut buf = [0u8; 4];
        assert_eq!(ring.poll_read(&mut buf), Ok(4));
        assert_eq!(&buf, b"1234");
        // Push more to force VecDeque wraparound across as_slices().
        ring.push(b"abcdef");
        assert_eq!(ring.pending_len(), 8);

        let mut out = [0u8; 10];
        assert_eq!(ring.poll_read(&mut out), Ok(8));
        assert_eq!(&out[..8], b"56abcdef");
        assert_eq!(ring.pending_len(), 0);
        assert_eq!(ring.poll_read(&mut out), Ok(0));

        ring.push_str("tail");
        ring.close();
        assert!(!ring.is_closed());
        assert_eq!(ring.poll_read(&mut out[..2]), Ok(2));
        assert_eq!(&out[..2], b"ta");
        assert!(!ring.is_closed());
        assert_eq!(ring.poll_read(&mut out), Ok(2));
        assert_eq!(&out[..2], b"il");
        assert!(ring.is_closed());
        assert_eq!(ring.poll_read(&mut out), Err(TerminalIoError::Closed));
        assert_eq!(ring.write_bytes(b"x"), Err(TerminalIoError::Closed));

        let mut rw = RingBufferInput::from_bytes(b"seed");
        assert_eq!(rw.write_bytes(b"abc"), Ok(()));
        assert_eq!(rw.drain_written(), b"abc");
        assert!(rw.drain_written().is_empty());
        assert_eq!(rw.resize(TerminalSize::new(90, 35)), Ok(()));
        assert_eq!(rw.last_size(), Some(TerminalSize::new(90, 35)));

        let mut hooked = RingBufferInput::new()
            .with_write_hook(|bytes| {
                if bytes == b"bad" {
                    Err(TerminalIoError::message("rejected"))
                } else {
                    Ok(())
                }
            })
            .with_resize_hook(|sz| {
                if sz.cols == 0 {
                    Err(TerminalIoError::message("zero cols"))
                } else {
                    Ok(())
                }
            });
        assert_eq!(hooked.write_bytes(b"ok"), Ok(()));
        assert_eq!(
            hooked.write_bytes(b"bad"),
            Err(TerminalIoError::message("rejected"))
        );
        assert_eq!(
            hooked.resize(TerminalSize::new(0, 24)),
            Err(TerminalIoError::message("zero cols"))
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_bounds_terminal_size() {
        const fn assert_serde<T: serde::Serialize + for<'de> serde::Deserialize<'de>>() {}
        assert_serde::<TerminalSize>();
    }
}
