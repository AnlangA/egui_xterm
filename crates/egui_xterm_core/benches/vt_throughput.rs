//! Criterion benchmarks for `egui_xterm_core` VT stream parsing throughput,
//! saturated 10k-line ingestion, DEC Mode 2026 sync output, OSC 8 + grapheme handling,
//! input adapters, and 10,000-line scrollback resize reflow latency.

#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use egui_xterm_core::{
    ChannelInput, CoreConfig, CsiParams, OscTerminator, Parser, RingBufferInput, Terminal,
    TerminalInput, VtHandler,
};

const STREAM_COLS: u16 = 120;
const STREAM_ROWS: u16 = 40;
const SCROLLBACK_TARGET: usize = 10_000;
const CHUNK_SIZE_16K: usize = 16 * 1024;
const SCRATCH_READ_64K: usize = 64 * 1024;

/// Zero-allocation parser sink that keeps all VT callbacks live while
/// overriding `print_ascii_run` so the SWAR ASCII fast-path is measured directly.
#[derive(Default)]
struct CountingHandler {
    chars: u64,
    ascii_bytes: u64,
    controls: u64,
    csi_count: u64,
    osc_count: u64,
    esc_count: u64,
    dcs_bytes: u64,
    checksum: u64,
}

impl CountingHandler {
    fn summary(&self) -> (u64, u64, u64, u64, u64, u64, u64, u64) {
        (
            self.chars,
            self.ascii_bytes,
            self.controls,
            self.csi_count,
            self.osc_count,
            self.esc_count,
            self.dcs_bytes,
            self.checksum,
        )
    }
}

impl VtHandler for CountingHandler {
    fn print(&mut self, ch: char) {
        self.chars = self.chars.wrapping_add(1);
        self.checksum = self.checksum.wrapping_add(u64::from(ch));
    }

    fn print_ascii_run(&mut self, bytes: &[u8]) {
        self.ascii_bytes = self.ascii_bytes.wrapping_add(bytes.len() as u64);
        self.checksum = self
            .checksum
            .wrapping_add(bytes.len() as u64 + u64::from(bytes.first().copied().unwrap_or(0)));
    }

    fn execute(&mut self, byte: u8) {
        self.controls = self.controls.wrapping_add(1);
        self.checksum = self.checksum.wrapping_add(u64::from(byte));
    }

    fn hook(&mut self, params: &CsiParams, intermediates: &[u8], action: u8) {
        self.dcs_bytes = self
            .dcs_bytes
            .wrapping_add(params.len() as u64 + intermediates.len() as u64);
        self.checksum = self.checksum.wrapping_add(u64::from(action));
    }

    fn put(&mut self, byte: u8) {
        self.dcs_bytes = self.dcs_bytes.wrapping_add(1);
        self.checksum = self.checksum.wrapping_add(u64::from(byte));
    }

    fn unhook(&mut self) {
        self.controls = self.controls.wrapping_add(1);
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], terminator: OscTerminator) {
        self.osc_count = self.osc_count.wrapping_add(1);
        let first_len = params.first().map_or(0, |p| p.len() as u64);
        let term_bit = u64::from(matches!(terminator, OscTerminator::St));
        self.checksum = self
            .checksum
            .wrapping_add(params.len() as u64 + first_len + term_bit);
    }

    fn csi_dispatch(&mut self, params: &CsiParams, intermediates: &[u8], action: u8) {
        self.csi_count = self.csi_count.wrapping_add(1);
        self.checksum = self
            .checksum
            .wrapping_add(params.len() as u64 + intermediates.len() as u64 + u64::from(action));
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], action: u8) {
        self.esc_count = self.esc_count.wrapping_add(1);
        self.checksum = self
            .checksum
            .wrapping_add(intermediates.len() as u64 + u64::from(action));
    }
}

fn build_ascii_plain_payload(target_bytes: usize) -> Vec<u8> {
    let mut out = String::with_capacity(target_bytes + 128);
    let mut idx = 0usize;
    while out.len() < target_bytes {
        if writeln!(
            out,
            "[2026-11-20T12:{:02}:{:02}Z INFO egui_xterm_core] frame={idx:05} status=ok bytes=4096\r",
            (idx / 60) % 60,
            idx % 60,
        )
        .is_err()
        {
            // `String`'s `fmt::Write` impl is infallible.
        }
        idx += 1;
    }
    out.into_bytes()
}

fn build_ascii_lines_with_colons(lines: usize) -> Vec<u8> {
    let mut out = String::with_capacity(lines * 96);
    for idx in 0..lines {
        if writeln!(
            out,
            "[2026-11-20T12:{:02}:{:02}Z INFO egui_xterm_core] frame={idx:05} status=ok bytes=4096\r",
            (idx / 60) % 60,
            idx % 60,
        )
        .is_err()
        {
            // `String`'s `fmt::Write` impl is infallible.
        }
    }
    out.into_bytes()
}

fn build_ascii_lines_no_colons(lines: usize) -> Vec<u8> {
    let mut out = String::with_capacity(lines * 96);
    for idx in 0..lines {
        if writeln!(
            out,
            "[2026-11-20 12-{:02}-{:02}Z INFO egui_xterm_core] frame={idx:05} status=ok bytes=4096\r",
            (idx / 60) % 60,
            idx % 60,
        )
        .is_err()
        {
            // `String`'s `fmt::Write` impl is infallible.
        }
    }
    out.into_bytes()
}

fn build_utf8_mixed_payload(target_bytes: usize) -> Vec<u8> {
    const SAMPLES: [&str; 4] = [
        "中文排版测试 mixed ASCII latency=0.42ms 🚀🎉",
        "résumé café naïve — re\u{0301}sume\u{0301} ❤️\u{200D}🔥",
        "👩‍🔬 Unicode grapheme cluster & 寬字元對齊检查 #",
        "Terminal throughput: αβγδ ∑∏∫ 漢字とかな ✨ #",
    ];
    let mut out = String::with_capacity(target_bytes + 128);
    let mut idx = 0usize;
    while out.len() < target_bytes {
        let sample = SAMPLES[idx % SAMPLES.len()];
        if writeln!(out, "[{idx:05}] {sample} ok\r").is_err() {
            // `String`'s `fmt::Write` impl is infallible.
        }
        idx += 1;
    }
    out.into_bytes()
}

fn build_utf8_mixed_lines(lines: usize) -> Vec<u8> {
    const SAMPLES: [&str; 4] = [
        "中文排版测试 mixed ASCII latency=0.42ms 🚀🎉",
        "résumé café naïve — re\u{0301}sume\u{0301} ❤️\u{200D}🔥",
        "👩‍🔬 Unicode grapheme cluster & 寬字元對齊检查 #",
        "Terminal throughput: αβγδ ∑∏∫ 漢字とかな ✨ #",
    ];
    let mut out = String::with_capacity(lines * 84);
    for idx in 0..lines {
        let sample = SAMPLES[idx % SAMPLES.len()];
        if writeln!(out, "[{idx:05}] {sample} ok\r").is_err() {
            // `String`'s `fmt::Write` impl is infallible.
        }
    }
    out.into_bytes()
}

fn build_truecolor_sgr_payload(target_bytes: usize) -> Vec<u8> {
    let mut out = String::with_capacity(target_bytes + 256);
    let mut idx = 0usize;
    while out.len() < target_bytes {
        let r = ((idx * 17) % 256) as u8;
        let g = ((idx * 31) % 256) as u8;
        let b = ((idx * 47) % 256) as u8;
        let br = ((idx * 11) % 48) as u8;
        let bg = ((idx * 19) % 48) as u8;
        let bb = ((idx * 23) % 48) as u8;
        if write!(
            out,
            "\x1b[1;38;2;{r};{g};{b};48;2;{br};{bg};{bb}mRGBA[{idx:04}]\
             \x1b[3;4:3;38:2::{g}:{b}:{r};48:2::{bb}:{br}:{bg}m CURLY_ITALIC\x1b[0m\r\n",
        )
        .is_err()
        {
            // `String`'s `fmt::Write` impl is infallible.
        }
        idx += 1;
    }
    out.into_bytes()
}

fn build_osc133_dense_payload(commands: usize) -> Vec<u8> {
    let mut out = String::with_capacity(commands * 120);
    for idx in 0..commands {
        let code = usize::from(idx % 5 == 4);
        let use_st = idx % 8 == 7;
        let term = if use_st { "\x1b\\" } else { "\x07" };
        if write!(
            out,
            "\x1b]133;A{term}user@host:~/egui_xterm$ \
             \x1b]133;B{term}cargo test --release -- --job={idx}\r\n\
             \x1b]133;C{term}running 93 tests ... result=ok elapsed=0.12s\r\n\
             \x1b]133;D;{code}{term}",
        )
        .is_err()
        {
            // `String`'s `fmt::Write` impl is infallible.
        }
    }
    out.into_bytes()
}

#[cfg(feature = "shell-integration")]
fn build_osc133_reflow_payload(commands: usize) -> Vec<u8> {
    let mut out = String::with_capacity(commands * 360);
    for idx in 0..commands {
        let code = usize::from(idx % 5 == 4);
        let term = if idx % 8 == 7 { "\x1b\\" } else { "\x07" };
        if write!(
            out,
            "\x1b]133;A{term}user@host:~/egui_xterm$ \
             \x1b]133;B{term}cargo test --release --job={idx}\r\n\
             \x1b]133;C{term}cmd_{idx:05} alpha_bravo_charlie_delta_echo_foxtrot_golf_hotel_india_juliet_kilo_lima_mike_november_oscar_papa_quebec\r\n\
             cmd_{idx:05} line_two_0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_padding\r\n\
             \x1b]133;D;{code}{term}",
        )
        .is_err()
        {
            // `String`'s `fmt::Write` impl is infallible.
        }
    }
    out.into_bytes()
}

fn build_sync_2026_200x60_frames(frames: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut payload = String::with_capacity(200 * 60 * 24 + 32);
        payload.push_str("\x1b[?2026h\x1b[H");
        for r in 0..60u16 {
            for c in 0..200u16 {
                let fg_r = ((c as usize + f * 7) % 256) as u8;
                let fg_g = ((r as usize + f * 11) % 256) as u8;
                if c % 40 == 39 {
                    if write!(payload, "\x1b[38;2;{fg_r};{fg_g};220me\u{0301}").is_err() {
                        // `String`'s `fmt::Write` impl is infallible.
                    }
                } else {
                    let ch = (b'A' + ((c as u8).wrapping_add(f as u8)) % 26) as char;
                    if write!(payload, "\x1b[38;2;{fg_r};{fg_g};220m{ch}").is_err() {
                        // `String`'s `fmt::Write` impl is infallible.
                    }
                }
            }
            if r + 1 < 60 {
                payload.push_str("\r\n");
            }
        }
        payload.push_str("\x1b[?2026l");
        out.push(payload.into_bytes());
    }
    out
}

#[cfg(feature = "hyperlinks")]
fn build_osc8_graphemes_payload(target_bytes: usize) -> Vec<u8> {
    let mut out = String::with_capacity(target_bytes + 512);
    let mut idx = 0usize;
    while out.len() < target_bytes {
        // Use >512 distinct URIs across the 512 KiB stream so `HyperlinkPool` exceeds
        // the 256-entry GC threshold in `Terminal::maybe_gc_hyperlink_pool`.
        let link_id = idx % 768;
        if writeln!(
            out,
            "[{idx:05}] \x1b]8;;https://example.com/item/{link_id}\x07\
             re\u{0301}sume\u{0301}_👩‍🔬_❤️\u{200D}🔥_{link_id:03}\
             \x1b]8;;\x07 ok\r",
        )
        .is_err()
        {
            // `String`'s `fmt::Write` impl is infallible.
        }
        idx += 1;
    }
    out.into_bytes()
}

fn build_reflow_narrow_payload(lines: usize) -> Vec<u8> {
    let mut out = String::with_capacity(lines * 116);
    for idx in 0..lines {
        if idx % 3 == 0 {
            if writeln!(
                out,
                "line_{idx:05} alpha_bravo_charlie_delta_echo_foxtrot_golf_hotel_india 中文测试🚀 juliet_kilo_lima_mike\r",
            )
            .is_err()
            {
                // `String`'s `fmt::Write` impl is infallible.
            }
        } else if writeln!(
            out,
            "line_{idx:05} 0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_padding_segment_end_col105   \r",
        )
        .is_err()
        {
            // `String`'s `fmt::Write` impl is infallible.
        }
    }
    out.into_bytes()
}

fn build_reflow_widen_payload(lines: usize) -> Vec<u8> {
    let mut out = String::with_capacity(lines * 120);
    for idx in 0..lines {
        if idx % 4 == 0 {
            if writeln!(
                out,
                "wide_{idx:05} unwrap_logical_line_across_margin_boundary_segment_A 中文字段测试🚀 segment_B_0123456789abcdef\r",
            )
            .is_err()
            {
                // `String`'s `fmt::Write` impl is infallible.
            }
        } else if writeln!(
            out,
            "wide_{idx:05} unwrap_logical_line_across_margin_boundary_0123456789abcdefghijklmnopqrstuvwxyz_tail_padding\r",
        )
        .is_err()
        {
            // `String`'s `fmt::Write` impl is infallible.
        }
    }
    out.into_bytes()
}

fn run_parser_bench(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: &str,
    payload: &[u8],
) {
    group.throughput(Throughput::Bytes(payload.len() as u64));
    group.bench_function(name, |b| {
        b.iter_batched_ref(
            || (Parser::new(), CountingHandler::default()),
            |(parser, handler)| {
                parser.advance(black_box(payload), handler);
                black_box(handler.summary());
            },
            BatchSize::LargeInput,
        );
    });
}

fn run_terminal_bench(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: &str,
    payload: &[u8],
) {
    group.throughput(Throughput::Bytes(payload.len() as u64));
    group.bench_function(name, |b| {
        b.iter_batched_ref(
            || Terminal::new(STREAM_COLS, STREAM_ROWS, CoreConfig::default()),
            |term| {
                term.advance(black_box(payload));
                black_box((
                    term.cursor_pos(),
                    term.scrollback_len(),
                    term.damage_epoch(),
                    term.drain_events().len(),
                ));
            },
            BatchSize::LargeInput,
        );
    });
}

#[cfg(feature = "shell-integration")]
fn run_terminal_osc133_bench(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: &str,
    payload: &[u8],
) {
    group.throughput(Throughput::Bytes(payload.len() as u64));
    group.bench_function(name, |b| {
        b.iter_batched_ref(
            || Terminal::new(STREAM_COLS, STREAM_ROWS, CoreConfig::default()),
            |term| {
                term.advance(black_box(payload));
                black_box((
                    term.cursor_pos(),
                    term.scrollback_len(),
                    term.damage_epoch(),
                    term.shell_zones().len(),
                    term.drain_events().len(),
                ));
            },
            BatchSize::LargeInput,
        );
    });
}

fn bench_vt_stream_throughput(c: &mut Criterion) {
    let ascii_plain = build_ascii_plain_payload(512 * 1024);
    let utf8_mixed = build_utf8_mixed_payload(512 * 1024);
    let truecolor_sgr = build_truecolor_sgr_payload(512 * 1024);
    let osc133_dense = build_osc133_dense_payload(2_500);

    let mut group = c.benchmark_group("vt_stream_throughput");

    run_parser_bench(&mut group, "parser_only/ascii_plain", &ascii_plain);
    run_parser_bench(&mut group, "parser_only/utf8_mixed", &utf8_mixed);
    run_parser_bench(&mut group, "parser_only/truecolor_sgr", &truecolor_sgr);
    run_parser_bench(&mut group, "parser_only/osc133_dense", &osc133_dense);

    run_terminal_bench(&mut group, "terminal/ascii_plain", &ascii_plain);
    run_terminal_bench(&mut group, "terminal/utf8_mixed", &utf8_mixed);
    run_terminal_bench(&mut group, "terminal/truecolor_sgr", &truecolor_sgr);

    #[cfg(feature = "shell-integration")]
    run_terminal_osc133_bench(&mut group, "terminal/osc133_dense", &osc133_dense);

    group.finish();
}

fn run_saturated_10k_bench(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: &str,
    prefill: &[u8],
    stream: &[u8],
) {
    group.throughput(Throughput::Bytes(stream.len() as u64));
    group.bench_function(name, |b| {
        b.iter_batched_ref(
            || {
                let mut term = Terminal::new(STREAM_COLS, STREAM_ROWS, CoreConfig::default());
                term.advance(prefill);
                assert_eq!(term.scrollback_len(), SCROLLBACK_TARGET);
                term
            },
            |term| {
                for chunk in black_box(stream).chunks(CHUNK_SIZE_16K) {
                    term.advance(chunk);
                }
                black_box((
                    term.cursor_pos(),
                    term.scrollback_len(),
                    term.damage_epoch(),
                    term.drain_events().len(),
                ));
            },
            BatchSize::PerIteration,
        );
    });
}

fn bench_terminal_saturated_10k(c: &mut Criterion) {
    let prefill_colons = build_ascii_lines_with_colons(10_500);
    let stream_colons = build_ascii_lines_with_colons(4_096);

    let prefill_no_colons = build_ascii_lines_no_colons(10_500);
    let stream_no_colons = build_ascii_lines_no_colons(4_096);

    let prefill_utf8 = build_utf8_mixed_lines(10_500);
    let stream_utf8 = build_utf8_mixed_lines(4_096);

    let mut group = c.benchmark_group("terminal_saturated_10k");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(4));

    run_saturated_10k_bench(
        &mut group,
        "ascii_with_colons",
        &prefill_colons,
        &stream_colons,
    );
    run_saturated_10k_bench(
        &mut group,
        "ascii_no_colons",
        &prefill_no_colons,
        &stream_no_colons,
    );

    #[cfg(feature = "shell-integration")]
    {
        let prefill_osc133 = build_osc133_dense_payload(5_200);
        let stream_osc133 = build_osc133_dense_payload(2_048);
        group.throughput(Throughput::Bytes(stream_osc133.len() as u64));
        group.bench_function("osc133_dense", |b| {
            b.iter_batched_ref(
                || {
                    let mut term = Terminal::new(STREAM_COLS, STREAM_ROWS, CoreConfig::default());
                    term.advance(&prefill_osc133);
                    assert_eq!(term.scrollback_len(), SCROLLBACK_TARGET);
                    term
                },
                |term| {
                    for chunk in black_box(&stream_osc133).chunks(CHUNK_SIZE_16K) {
                        term.advance(chunk);
                    }
                    black_box((
                        term.cursor_pos(),
                        term.scrollback_len(),
                        term.damage_epoch(),
                        term.shell_zones().len(),
                        term.drain_events().len(),
                    ));
                },
                BatchSize::PerIteration,
            );
        });
    }

    run_saturated_10k_bench(&mut group, "utf8_mixed", &prefill_utf8, &stream_utf8);

    group.finish();
}

fn bench_chunked_16k_advance(c: &mut Criterion) {
    let ascii_plain = build_ascii_plain_payload(512 * 1024);
    let truecolor_sgr = build_truecolor_sgr_payload(512 * 1024);

    let mut group = c.benchmark_group("chunked_16k_advance");
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(4));

    for (name, payload) in [
        ("ascii_plain", &ascii_plain),
        ("truecolor_sgr", &truecolor_sgr),
    ] {
        group.throughput(Throughput::Bytes(payload.len() as u64));
        group.bench_function(name, |b| {
            b.iter_batched_ref(
                || Terminal::new(STREAM_COLS, STREAM_ROWS, CoreConfig::default()),
                |term| {
                    for chunk in black_box(payload.as_slice()).chunks(CHUNK_SIZE_16K) {
                        term.advance(chunk);
                    }
                    black_box((
                        term.cursor_pos(),
                        term.scrollback_len(),
                        term.damage_epoch(),
                    ));
                },
                BatchSize::LargeInput,
            );
        });
    }

    #[cfg(feature = "shell-integration")]
    {
        let osc133_dense = build_osc133_dense_payload(2_500);
        group.throughput(Throughput::Bytes(osc133_dense.len() as u64));
        group.bench_function("osc133_dense", |b| {
            b.iter_batched_ref(
                || Terminal::new(STREAM_COLS, STREAM_ROWS, CoreConfig::default()),
                |term| {
                    for chunk in black_box(osc133_dense.as_slice()).chunks(CHUNK_SIZE_16K) {
                        term.advance(chunk);
                    }
                    black_box((
                        term.cursor_pos(),
                        term.scrollback_len(),
                        term.shell_zones().len(),
                    ));
                },
                BatchSize::LargeInput,
            );
        });
    }

    group.finish();
}

fn bench_sync_output_and_osc8(c: &mut Criterion) {
    let frames = build_sync_2026_200x60_frames(4);
    let total_bytes: u64 = frames.iter().map(|f| f.len() as u64).sum();

    let mut sync_group = c.benchmark_group("sync_output_dec2026_200x60");
    sync_group.sample_size(50);
    sync_group.warm_up_time(Duration::from_secs(2));
    sync_group.measurement_time(Duration::from_secs(4));
    sync_group.throughput(Throughput::Bytes(total_bytes));
    sync_group.bench_function("truecolor_combining_frames", |b| {
        b.iter_batched_ref(
            || Terminal::new(200, 60, CoreConfig::default()),
            |term| {
                let mut t = 1.0_f64;
                for frame in black_box(&frames) {
                    term.advance_with_time(frame, t);
                    t += 0.016;
                    black_box((
                        term.is_synchronized_output(),
                        term.active_snapshot().visible_rows.len(),
                    ));
                }
            },
            BatchSize::LargeInput,
        );
    });
    sync_group.finish();

    #[cfg(feature = "hyperlinks")]
    {
        let payload = build_osc8_graphemes_payload(512 * 1024);
        let mut osc8_group = c.benchmark_group("osc8_and_graphemes");
        osc8_group.sample_size(50);
        osc8_group.warm_up_time(Duration::from_secs(2));
        osc8_group.measurement_time(Duration::from_secs(4));
        osc8_group.throughput(Throughput::Bytes(payload.len() as u64));
        osc8_group.bench_function("512k_distinct_links", |b| {
            b.iter_batched_ref(
                || Terminal::new(STREAM_COLS, STREAM_ROWS, CoreConfig::default()),
                |term| {
                    term.advance(black_box(&payload));
                    black_box((term.hyperlink_pool().len(), term.damage_epoch()));
                },
                BatchSize::LargeInput,
            );
        });
        osc8_group.finish();
    }
}

fn bench_reflow_10k_scrollback(c: &mut Criterion) {
    let narrow_payload = build_reflow_narrow_payload(10_500);
    let widen_payload = build_reflow_widen_payload(5_200);

    // One-time sanity checks ensuring both setups saturate the 10,000-line scrollback buffer.
    {
        let mut check_narrow = Terminal::new(120, 40, CoreConfig::default());
        check_narrow.advance(&narrow_payload);
        assert_eq!(
            check_narrow.scrollback_len(),
            SCROLLBACK_TARGET,
            "narrow setup must saturate 10k scrollback"
        );

        let mut check_widen = Terminal::new(80, 40, CoreConfig::default());
        check_widen.advance(&widen_payload);
        assert_eq!(
            check_widen.scrollback_len(),
            SCROLLBACK_TARGET,
            "widen setup must saturate 10k scrollback"
        );
    }

    let mut group = c.benchmark_group("reflow_10k_scrollback");
    group.sample_size(20);
    group.throughput(Throughput::Elements(SCROLLBACK_TARGET as u64));

    group.bench_function("narrow_120x40_to_80x40", |b| {
        b.iter_batched_ref(
            || {
                let mut term = Terminal::new(120, 40, CoreConfig::default());
                term.advance(&narrow_payload);
                term
            },
            |term| {
                term.resize(80, 40);
                black_box((
                    term.size(),
                    term.scrollback_len(),
                    term.total_lines(),
                    term.cursor_pos(),
                    term.damage_epoch(),
                ));
            },
            BatchSize::PerIteration,
        );
    });

    group.bench_function("widen_80x40_to_160x40", |b| {
        b.iter_batched_ref(
            || {
                let mut term = Terminal::new(80, 40, CoreConfig::default());
                term.advance(&widen_payload);
                term
            },
            |term| {
                term.resize(160, 40);
                black_box((
                    term.size(),
                    term.scrollback_len(),
                    term.total_lines(),
                    term.cursor_pos(),
                    term.damage_epoch(),
                ));
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();

    #[cfg(feature = "shell-integration")]
    {
        let osc133_reflow_payload = build_osc133_reflow_payload(3_400);
        {
            let mut check = Terminal::new(120, 40, CoreConfig::default());
            check.advance(&osc133_reflow_payload);
            assert_eq!(
                check.scrollback_len(),
                SCROLLBACK_TARGET,
                "osc133 reflow setup must saturate 10k scrollback"
            );
            assert!(
                check.shell_zones().len() >= 2_500,
                "osc133 reflow setup must retain >= 2500 shell zones"
            );
        }

        let mut zone_group = c.benchmark_group("reflow_10k_with_2500_osc133_zones");
        zone_group.sample_size(10);
        zone_group.warm_up_time(Duration::from_secs(1));
        zone_group.measurement_time(Duration::from_secs(5));
        zone_group.throughput(Throughput::Elements(SCROLLBACK_TARGET as u64));
        zone_group.bench_function("narrow_120x40_to_80x40", |b| {
            b.iter_batched_ref(
                || {
                    let mut term = Terminal::new(120, 40, CoreConfig::default());
                    term.advance(&osc133_reflow_payload);
                    term
                },
                |term| {
                    term.resize(80, 40);
                    black_box((
                        term.size(),
                        term.scrollback_len(),
                        term.shell_zones().len(),
                        term.damage_epoch(),
                    ));
                },
                BatchSize::PerIteration,
            );
        });
        zone_group.finish();
    }
}

fn bench_input_adapters(c: &mut Criterion) {
    const TOTAL_BYTES: usize = 512 * 1024;
    let chunk_16k = vec![b'A'; CHUNK_SIZE_16K];
    let num_chunks = TOTAL_BYTES / CHUNK_SIZE_16K;

    let mut group = c.benchmark_group("input_adapters");
    group.sample_size(100);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    group.throughput(Throughput::Bytes(TOTAL_BYTES as u64));

    group.bench_function("ring_buffer", |b| {
        b.iter_batched_ref(
            || {
                (
                    RingBufferInput::with_capacity(TOTAL_BYTES),
                    vec![0u8; SCRATCH_READ_64K],
                )
            },
            |(ring, scratch)| {
                for _ in 0..num_chunks {
                    ring.push(black_box(&chunk_16k));
                }
                let mut total = 0usize;
                while let Ok(n) = ring.poll_read(scratch) {
                    if n == 0 {
                        break;
                    }
                    total += n;
                }
                black_box(total);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("channel", |b| {
        let prebuilt_chunks: Vec<Vec<u8>> = (0..num_chunks).map(|_| chunk_16k.clone()).collect();
        b.iter_batched_ref(
            || {
                let (tx, rx) = ChannelInput::pair();
                (tx, rx, prebuilt_chunks.clone(), vec![0u8; SCRATCH_READ_64K])
            },
            |(tx, rx, chunks, scratch)| {
                for chunk in chunks.drain(..) {
                    if tx.send(chunk).is_err() {
                        break;
                    }
                }
                let mut total = 0usize;
                while let Ok(n) = rx.poll_read(scratch) {
                    if n == 0 {
                        break;
                    }
                    total += n;
                }
                black_box(total);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_vt_stream_throughput,
    bench_terminal_saturated_10k,
    bench_chunked_16k_advance,
    bench_sync_output_and_osc8,
    bench_reflow_10k_scrollback,
    bench_input_adapters,
);
criterion_main!(benches);
