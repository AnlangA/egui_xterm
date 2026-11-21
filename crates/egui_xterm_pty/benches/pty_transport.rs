//! Criterion benchmarks for [`egui_xterm_pty::PtyBackend`] transport throughput and latency.

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
mod bench_impl {
    use std::hint::black_box;
    use std::io::{self, Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    use criterion::{Criterion, Throughput};
    use egui_xterm_pty::{
        PtyBackend, PtyMaster, RepaintCallback, TerminalInput, TerminalIoError, TerminalSize,
    };
    use portable_pty::{Child, ChildKiller, ExitStatus};

    const CHUNK_16K: usize = 16 * 1024;
    const SCRATCH_64K: usize = 64 * 1024;
    const WRITE_BURST_CHUNKS: usize = 256;
    const WRITE_CHUNK_BYTES: usize = 64;

    struct BenchReaderState {
        remaining_chunks: usize,
        closed: bool,
    }

    type SharedReaderState = Arc<(Mutex<BenchReaderState>, Condvar)>;
    type SharedCounterSignal = Arc<(Mutex<usize>, Condvar)>;

    struct BenchReader {
        state: SharedReaderState,
        pattern: Vec<u8>,
    }

    impl BenchReader {
        fn pair() -> (SharedReaderState, Box<dyn Read + Send>) {
            let state = Arc::new((
                Mutex::new(BenchReaderState {
                    remaining_chunks: 0,
                    closed: false,
                }),
                Condvar::new(),
            ));
            (
                Arc::clone(&state),
                Box::new(Self {
                    state,
                    pattern: vec![b'X'; CHUNK_16K],
                }),
            )
        }
    }

    impl Read for BenchReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let (lock, cvar) = &*self.state;
            let mut guard = match lock.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            while guard.remaining_chunks == 0 && !guard.closed {
                guard = match cvar.wait(guard) {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
            }
            if guard.remaining_chunks == 0 && guard.closed {
                return Ok(0);
            }
            guard.remaining_chunks -= 1;
            drop(guard);

            let n = self.pattern.len().min(buf.len());
            buf[..n].copy_from_slice(&self.pattern[..n]);
            Ok(n)
        }
    }

    struct BenchWriter {
        bytes_written: Arc<AtomicUsize>,
        done_signal: SharedCounterSignal,
    }

    impl BenchWriter {
        fn pair() -> (SharedCounterSignal, Box<dyn Write + Send>) {
            let bytes_written = Arc::new(AtomicUsize::new(0));
            let done_signal = Arc::new((Mutex::new(0usize), Condvar::new()));
            (
                Arc::clone(&done_signal),
                Box::new(Self {
                    bytes_written,
                    done_signal,
                }),
            )
        }
    }

    impl Write for BenchWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.bytes_written.fetch_add(buf.len(), Ordering::Relaxed);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            let total = self.bytes_written.load(Ordering::Acquire);
            let (lock, cvar) = &*self.done_signal;
            let mut guard = match lock.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            *guard = total;
            cvar.notify_all();
            Ok(())
        }
    }

    #[derive(Debug)]
    struct BenchChild {
        killed: Arc<AtomicBool>,
    }

    #[derive(Debug)]
    struct BenchKiller {
        killed: Arc<AtomicBool>,
    }

    impl ChildKiller for BenchKiller {
        fn kill(&mut self) -> io::Result<()> {
            self.killed.store(true, Ordering::Release);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(Self {
                killed: Arc::clone(&self.killed),
            })
        }
    }

    impl ChildKiller for BenchChild {
        fn kill(&mut self) -> io::Result<()> {
            self.killed.store(true, Ordering::Release);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(BenchKiller {
                killed: Arc::clone(&self.killed),
            })
        }
    }

    impl Child for BenchChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            if self.killed.load(Ordering::Acquire) {
                Ok(Some(ExitStatus::with_exit_code(0)))
            } else {
                Ok(None)
            }
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            while !self.killed.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            Some(1)
        }

        #[cfg(windows)]
        fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
            None
        }
    }

    struct BenchMaster;

    impl PtyMaster for BenchMaster {
        fn resize_pty(&self, _size: TerminalSize) -> Result<(), TerminalIoError> {
            Ok(())
        }
    }

    struct RepaintTracker {
        seq: Arc<AtomicUsize>,
        signal: SharedCounterSignal,
        callback: RepaintCallback,
    }

    fn make_repaint_tracker() -> RepaintTracker {
        let seq = Arc::new(AtomicUsize::new(0));
        let signal = Arc::new((Mutex::new(0usize), Condvar::new()));
        let seq_cb = Arc::clone(&seq);
        let sig_cb = Arc::clone(&signal);
        let callback: RepaintCallback = Box::new(move || {
            let next = seq_cb.fetch_add(1, Ordering::Release) + 1;
            let (lock, cvar) = &*sig_cb;
            let mut guard = match lock.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            *guard = next;
            cvar.notify_all();
        });
        RepaintTracker {
            seq,
            signal,
            callback,
        }
    }

    fn wait_repaint_advance(signal: &SharedCounterSignal, observed_seq: usize) {
        let (lock, cvar) = &**signal;
        let guard = match lock.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if *guard != observed_seq {
            return;
        }
        let (_guard, _timeout) = match cvar.wait_timeout_while(
            guard,
            Duration::from_millis(2),
            |s| *s == observed_seq,
        ) {
            Ok(pair) => pair,
            Err(poisoned) => poisoned.into_inner(),
        };
    }

    struct BenchBackendHarness {
        backend: PtyBackend,
        reader_state: SharedReaderState,
        writer_sig: SharedCounterSignal,
        repaint_seq: Arc<AtomicUsize>,
        repaint_sig: SharedCounterSignal,
    }

    fn build_bench_backend() -> BenchBackendHarness {
        let (reader_state, reader) = BenchReader::pair();
        let (writer_sig, writer) = BenchWriter::pair();
        let child = Box::new(BenchChild {
            killed: Arc::new(AtomicBool::new(false)),
        });
        let master = Box::new(BenchMaster);
        let tracker = make_repaint_tracker();

        let backend = match PtyBackend::from_parts(
            reader,
            writer,
            child,
            master,
            TerminalSize::new(80, 24),
            Some(tracker.callback),
        ) {
            Ok(b) => b,
            Err(err) => panic!("failed to construct benchmark PtyBackend: {err}"),
        };

        BenchBackendHarness {
            backend,
            reader_state,
            writer_sig,
            repaint_seq: tracker.seq,
            repaint_sig: tracker.signal,
        }
    }

    fn close_reader(reader_state: &SharedReaderState) {
        let (lock, cvar) = &**reader_state;
        let mut guard = match lock.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.closed = true;
        cvar.notify_all();
    }

    fn bench_read_throughput(c: &mut Criterion) {
        let mut group = c.benchmark_group("pty_transport_read");
        for (label, num_chunks) in [("512k_16k_chunks", 32usize), ("4m_16k_chunks", 256usize)] {
            let total_bytes = num_chunks * CHUNK_16K;
            group.throughput(Throughput::Bytes(total_bytes as u64));

            let BenchBackendHarness {
                mut backend,
                reader_state,
                repaint_seq,
                repaint_sig,
                ..
            } = build_bench_backend();
            let mut scratch = vec![0u8; SCRATCH_64K];

            group.bench_function(label, |b| {
                b.iter_custom(|iters| {
                    let mut total_elapsed = Duration::ZERO;
                    for _ in 0..iters {
                        let start = Instant::now();
                        {
                            let (lock, cvar) = &*reader_state;
                            let mut guard = match lock.lock() {
                                Ok(g) => g,
                                Err(p) => p.into_inner(),
                            };
                            guard.remaining_chunks = num_chunks;
                            cvar.notify_all();
                        }

                        let mut read_so_far = 0usize;
                        while read_so_far < total_bytes {
                            let seq_before = repaint_seq.load(Ordering::Acquire);
                            match backend.poll_read(&mut scratch) {
                                Ok(0) => wait_repaint_advance(&repaint_sig, seq_before),
                                Ok(n) => {
                                    black_box(&scratch[..n]);
                                    read_so_far += n;
                                }
                                Err(err) => panic!("unexpected benchmark poll_read error: {err}"),
                            }
                        }
                        total_elapsed += start.elapsed();
                    }
                    total_elapsed
                });
            });

            close_reader(&reader_state);
            drop(backend);
        }
        group.finish();
    }

    fn bench_idle_poll_read(c: &mut Criterion) {
        let mut group = c.benchmark_group("pty_transport_idle");
        group.throughput(Throughput::Elements(10_000));

        let BenchBackendHarness {
            mut backend,
            reader_state,
            ..
        } = build_bench_backend();
        let mut scratch = vec![0u8; SCRATCH_64K];

        group.bench_function("10k_poll_read_calls", |b| {
            b.iter(|| {
                for _ in 0..10_000 {
                    match backend.poll_read(&mut scratch) {
                        Ok(n) => {
                            black_box(n);
                        }
                        Err(err) => panic!("unexpected idle poll_read error: {err}"),
                    }
                }
            });
        });

        close_reader(&reader_state);
        drop(backend);
        group.finish();
    }

    fn bench_write_burst(c: &mut Criterion) {
        let mut group = c.benchmark_group("pty_transport_write");
        let burst_bytes = WRITE_BURST_CHUNKS * WRITE_CHUNK_BYTES;
        group.throughput(Throughput::Bytes(burst_bytes as u64));

        let BenchBackendHarness {
            mut backend,
            reader_state,
            writer_sig,
            ..
        } = build_bench_backend();
        let payload = [b'k'; WRITE_CHUNK_BYTES];
        let mut expected_total = 0usize;

        group.bench_function("256x64b_burst", |b| {
            b.iter_custom(|iters| {
                let mut total_elapsed = Duration::ZERO;
                for _ in 0..iters {
                    let start = Instant::now();
                    for _ in 0..WRITE_BURST_CHUNKS {
                        if let Err(err) = backend.write_bytes(black_box(&payload)) {
                            panic!("unexpected benchmark write_bytes error: {err}");
                        }
                    }
                    total_elapsed += start.elapsed();

                    expected_total += burst_bytes;
                    let (lock, cvar) = &*writer_sig;
                    let guard = match lock.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    let (_guard, _timeout) =
                        match cvar.wait_timeout_while(guard, Duration::from_secs(2), |total| {
                            *total < expected_total
                        }) {
                            Ok(pair) => pair,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                }
                total_elapsed
            });
        });

        close_reader(&reader_state);
        drop(backend);
        group.finish();
    }

    pub fn run() {
        let mut criterion = Criterion::default().configure_from_args();
        bench_read_throughput(&mut criterion);
        bench_idle_poll_read(&mut criterion);
        bench_write_burst(&mut criterion);
        criterion.final_summary();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    bench_impl::run();
}
