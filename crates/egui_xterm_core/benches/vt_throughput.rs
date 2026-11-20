//! Criterion benchmarks for `egui_xterm_core` VT stream parsing throughput
//! and 10,000-line scrollback resize reflow latency.

#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::hint::black_box;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use egui_xterm_core::{CoreConfig, CsiParams, OscTerminator, Parser, Terminal, VtHandler};

const STREAM_COLS: u16 = 120;
const STREAM_ROWS: u16 = 40;
const SCROLLBACK_TARGET: usize = 10_000;

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
        let _ = writeln!(
            out,
            "[2026-11-20T12:{:02}:{:02}Z INFO egui_xterm_core] frame={idx:05} status=ok bytes=4096\r",
            (idx / 60) % 60,
            idx % 60,
        );
        idx += 1;
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
        let _ = writeln!(out, "[{idx:05}] {sample} ok\r");
        idx += 1;
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
        let _ = write!(
            out,
            "\x1b[1;38;2;{r};{g};{b};48;2;{br};{bg};{bb}mRGBA[{idx:04}]\
             \x1b[3;4:3;38:2::{g}:{b}:{r};48:2::{bb}:{br}:{bg}m CURLY_ITALIC\x1b[0m\r\n",
        );
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
        let _ = write!(
            out,
            "\x1b]133;A{term}user@host:~/egui_xterm$ \
             \x1b]133;B{term}cargo test --release -- --job={idx}\r\n\
             \x1b]133;C{term}running 93 tests ... result=ok elapsed=0.12s\r\n\
             \x1b]133;D;{code}{term}",
        );
    }
    out.into_bytes()
}

fn build_reflow_narrow_payload(lines: usize) -> Vec<u8> {
    let mut out = String::with_capacity(lines * 116);
    for idx in 0..lines {
        if idx % 3 == 0 {
            let _ = writeln!(
                out,
                "line_{idx:05} alpha_bravo_charlie_delta_echo_foxtrot_golf_hotel_india 中文测试🚀 juliet_kilo_lima_mike\r",
            );
        } else {
            let _ = writeln!(
                out,
                "line_{idx:05} 0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_padding_segment_end_col105   \r",
            );
        }
    }
    out.into_bytes()
}

fn build_reflow_widen_payload(lines: usize) -> Vec<u8> {
    let mut out = String::with_capacity(lines * 120);
    for idx in 0..lines {
        if idx % 4 == 0 {
            let _ = writeln!(
                out,
                "wide_{idx:05} unwrap_logical_line_across_margin_boundary_segment_A 中文字段测试🚀 segment_B_0123456789abcdef\r",
            );
        } else {
            let _ = writeln!(
                out,
                "wide_{idx:05} unwrap_logical_line_across_margin_boundary_0123456789abcdefghijklmnopqrstuvwxyz_tail_padding\r",
            );
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
}

criterion_group!(
    benches,
    bench_vt_stream_throughput,
    bench_reflow_10k_scrollback
);
criterion_main!(benches);
