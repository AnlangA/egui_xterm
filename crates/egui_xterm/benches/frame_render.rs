//! Criterion benchmark measuring single-frame update + auto-layout + 4-stage cached render +
//! `epaint::Tessellator` latency for a `200x60` TrueColor terminal.

use std::fmt::Write as _;
use std::hint::black_box;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use egui::{CentralPanel, Context, Frame, Pos2, RawInput, Rect, Vec2, ViewportId};
use egui_xterm::{RingBufferInput, ScaleMode, TerminalConfig, TerminalRuntime};

const TARGET_COLS: u16 = 200;
const TARGET_ROWS: u16 = 60;

fn run_single_frame_and_tessellate(
    ctx: &Context,
    runtime: &mut TerminalRuntime<RingBufferInput>,
    screen_rect: Rect,
    sim_time: f64,
) {
    let mut raw_input = RawInput {
        screen_rect: Some(screen_rect),
        time: Some(sim_time),
        focused: true,
        ..Default::default()
    };
    raw_input
        .viewports
        .entry(ViewportId::ROOT)
        .or_default()
        .native_pixels_per_point = Some(1.0);

    let full_output = ctx.run_ui(raw_input, |ui| {
        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            let _ = runtime.show(ui);
        });
    });

    let shapes = full_output.shapes.clone();
    let ppp = full_output.pixels_per_point;
    full_output.drop_without_applying_deltas();
    let primitives = ctx.tessellate(shapes, ppp);
    let _ = black_box(primitives);
}

fn build_truecolor_screen(cols: u16, rows: u16, seed: u8) -> Vec<u8> {
    let mut out = String::with_capacity(usize::from(cols) * usize::from(rows) * 24);
    out.push_str("\x1b[H");
    for r in 0..rows {
        for c in 0..cols {
            let fg_r = ((c + u16::from(seed)) * 3) as u8;
            let fg_g = ((r + u16::from(seed)) * 5) as u8;
            let bg_b = ((c + r) % 40) as u8;
            let ch = (b'A' + ((c as u8).wrapping_add(seed)) % 26) as char;
            let _ = write!(out, "\x1b[38;2;{fg_r};{fg_g};220;48;2;12;16;{bg_b}m{ch}");
        }
        if r + 1 < rows {
            out.push_str("\r\n");
        }
    }
    out.into_bytes()
}

fn build_single_truecolor_row(row_1based: u16, cols: u16, seed: u8) -> Vec<u8> {
    let mut out = String::with_capacity(usize::from(cols) * 24 + 16);
    let _ = write!(out, "\x1b[{row_1based};1H");
    for c in 0..cols {
        let fg_r = ((c + u16::from(seed)) * 7) as u8;
        let bg_g = ((c + u16::from(seed)) % 48) as u8;
        let ch = (b'a' + ((c as u8).wrapping_add(seed)) % 26) as char;
        let _ = write!(out, "\x1b[38;2;{fg_r};200;160;48;2;18;{bg_g};24m{ch}");
    }
    out.into_bytes()
}

fn setup_200x60_harness() -> (Context, TerminalRuntime<RingBufferInput>, Rect) {
    let ctx = Context::default();
    let cfg = TerminalConfig {
        scale_mode: ScaleMode::ReflowGrid,
        resize_debounce_secs: 0.0,
        read_budget_bytes_per_frame: usize::MAX,
        cursor_blink: false,
        ..TerminalConfig::default()
    };
    let mut runtime = TerminalRuntime::with_config(RingBufferInput::new(), cfg);

    // Measure cell_size on a warm-up frame, then calibrate exact 200x60 viewport.
    let probe_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 960.0));
    let mut cell_size = Vec2::new(8.0, 16.0);
    for step in 0..2 {
        let mut raw = RawInput {
            screen_rect: Some(probe_rect),
            time: Some(step as f64 * 0.016),
            focused: true,
            ..Default::default()
        };
        raw.viewports
            .entry(ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(1.0);
        let full = ctx.run_ui(raw, |ui| {
            CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
                let resp = runtime.show(ui);
                cell_size = resp.cell_size;
            });
        });
        full.drop_without_applying_deltas();
    }

    let screen_rect = Rect::from_min_size(
        Pos2::ZERO,
        Vec2::new(
            f32::from(TARGET_COLS) * cell_size.x + 1.0,
            f32::from(TARGET_ROWS) * cell_size.y + 1.0,
        ),
    );
    let initial = build_truecolor_screen(TARGET_COLS, TARGET_ROWS, 0);
    runtime.input_mut().push(&initial);
    for step in 2..5 {
        run_single_frame_and_tessellate(&ctx, &mut runtime, screen_rect, step as f64 * 0.016);
    }
    assert_eq!(runtime.terminal().size(), (TARGET_COLS, TARGET_ROWS));
    (ctx, runtime, screen_rect)
}

fn bench_frame_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_render_200x60_truecolor");
    group.throughput(Throughput::Elements(
        u64::from(TARGET_COLS) * u64::from(TARGET_ROWS),
    ));

    // 1. Idle frame (100% RowRenderCache hit)
    {
        let (ctx, mut runtime, screen_rect) = setup_200x60_harness();
        let mut t = 1.0;
        group.bench_function("idle", |b| {
            b.iter(|| {
                t += 1.0 / 60.0;
                run_single_frame_and_tessellate(&ctx, &mut runtime, screen_rect, t);
            });
        });
    }

    // 2. 1 dirty row per frame
    {
        let (ctx, mut runtime, screen_rect) = setup_200x60_harness();
        let row_even = build_single_truecolor_row(30, TARGET_COLS, 1);
        let row_odd = build_single_truecolor_row(30, TARGET_COLS, 2);
        let mut t = 1.0;
        let mut toggle = false;
        group.bench_function("1_dirty_row", |b| {
            b.iter_batched(
                || {
                    toggle = !toggle;
                    if toggle { &row_even } else { &row_odd }
                },
                |payload| {
                    runtime.input_mut().push(payload);
                    t += 1.0 / 60.0;
                    run_single_frame_and_tessellate(&ctx, &mut runtime, screen_rect, t);
                },
                BatchSize::SmallInput,
            );
        });
    }

    // 3. Scroll 1 line per frame
    {
        let (ctx, mut runtime, screen_rect) = setup_200x60_harness();
        let mut line_payload = Vec::with_capacity(usize::from(TARGET_COLS) * 24 + 8);
        line_payload.extend_from_slice(b"\r\n");
        let row_bytes = build_single_truecolor_row(TARGET_ROWS, TARGET_COLS, 5);
        line_payload.extend_from_slice(&row_bytes);
        let mut t = 1.0;
        group.bench_function("scroll_1_line", |b| {
            b.iter_batched(
                || &line_payload,
                |payload| {
                    runtime.input_mut().push(payload);
                    t += 1.0 / 60.0;
                    run_single_frame_and_tessellate(&ctx, &mut runtime, screen_rect, t);
                },
                BatchSize::SmallInput,
            );
        });
    }

    // 4. Worst-case full 200x60 TrueColor redraw
    {
        let (ctx, mut runtime, screen_rect) = setup_200x60_harness();
        let screen_a = build_truecolor_screen(TARGET_COLS, TARGET_ROWS, 11);
        let screen_b = build_truecolor_screen(TARGET_COLS, TARGET_ROWS, 29);
        let mut t = 1.0;
        let mut toggle = false;
        group.bench_function("full_redraw", |b| {
            b.iter_batched(
                || {
                    toggle = !toggle;
                    if toggle { &screen_a } else { &screen_b }
                },
                |payload| {
                    runtime.input_mut().push(payload);
                    t += 1.0 / 60.0;
                    run_single_frame_and_tessellate(&ctx, &mut runtime, screen_rect, t);
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

criterion_group!(benches, bench_frame_render);
criterion_main!(benches);
