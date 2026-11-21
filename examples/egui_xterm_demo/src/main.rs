//! Interactive `eframe` 0.37 demo application showcasing `egui_xterm`.
//!
//! Demonstrates desktop PTY sessions (`egui_xterm::pty`), cross-platform trait-driven
//! consoles ([`demo_shell::DemoShellInput`]), split-pane auto-layout inside [`egui::Frame`]
//! containers, [`ScaleMode::ReflowGrid`] vs [`ScaleMode::AutoScaleToFit`], themes,
//! cursor customization, and optional features (`search`, `shell-integration`, `metrics`).

#![forbid(unsafe_code)]

mod demo_shell;

use demo_shell::{DemoShellInput, SHOWCASE_COMMANDS};
use eframe::egui;
#[cfg(not(target_arch = "wasm32"))]
use egui_xterm::pty::{PtyBackend, PtyConfig};
use egui_xterm::{
    CursorShape, ScaleMode, TerminalConfig, TerminalEvent, TerminalInput, TerminalResponse,
    TerminalRuntime, Theme,
};

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1240.0, 780.0])
            .with_min_inner_size([640.0, 420.0]),
        ..eframe::NativeOptions::default()
    };

    eframe::run_native(
        "egui_xterm Demo",
        options,
        Box::new(|cc| Ok(Box::new(EguiXtermDemoApp::new(cc)))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {
    // Keep wasm32 target compilation checking the entire eframe App implementation cleanly.
    let _creator: fn(&eframe::CreationContext<'_>) -> EguiXtermDemoApp = EguiXtermDemoApp::new;
    let _ui_fn: fn(&mut EguiXtermDemoApp, &mut egui::Ui, &mut eframe::Frame) =
        <EguiXtermDemoApp as eframe::App>::ui;
}

enum PaneBackend {
    #[cfg(not(target_arch = "wasm32"))]
    Pty(TerminalRuntime<PtyBackend>),
    Trait(TerminalRuntime<DemoShellInput>),
    #[cfg(not(target_arch = "wasm32"))]
    Error(String),
}

struct TerminalTab {
    id: u64,
    label: String,
    is_pty: bool,
    config: TerminalConfig,
    backend: PaneBackend,
    last_response: Option<PaneMetricsSnapshot>,
    last_notice: Option<String>,
}

#[derive(Clone)]
struct PaneMetricsSnapshot {
    cols: u16,
    rows: u16,
    cell_size: egui::Vec2,
    effective_font_size: f32,
    title: String,
    cwd: Option<String>,
    display_offset: usize,
    scrollback_len: usize,
    alt_screen: bool,
    sync_output: bool,
    #[cfg(feature = "shell-integration")]
    zone_count: usize,
    #[cfg(feature = "metrics")]
    metrics: egui_xterm::TerminalMetrics,
    closed: bool,
}

impl TerminalTab {
    fn show_in_frame(&mut self, ui: &mut egui::Ui, is_active: bool) -> bool {
        let scheme = &self.config.theme.scheme;
        let bg = egui::Color32::from_rgb(
            scheme.background[0],
            scheme.background[1],
            scheme.background[2],
        );
        let border_color = if is_active {
            ui.visuals().selection.stroke.color
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke.color
        };

        let mut clicked = false;
        let frame = egui::Frame::new()
            .fill(bg)
            .stroke(egui::Stroke::new(
                if is_active { 1.5 } else { 1.0 },
                border_color,
            ))
            .inner_margin(egui::Margin::same(3));

        frame.show(ui, |ui| {
            // Ensure the frame fills the available panel space so TerminalRuntime auto-sizes to it.
            let avail = ui.available_size();
            ui.set_min_size(avail);

            match &mut self.backend {
                #[cfg(not(target_arch = "wasm32"))]
                PaneBackend::Pty(rt) => {
                    let resp = rt.show_with_config(ui, &mut self.config);
                    clicked = resp.response.clicked()
                        || resp.response.drag_started()
                        || resp.response.gained_focus();
                    let snapshot = Self::capture_snapshot(rt, &resp);
                    self.apply_events(&resp.events);
                    self.last_response = Some(snapshot);
                }
                PaneBackend::Trait(rt) => {
                    let resp = rt.show_with_config(ui, &mut self.config);
                    clicked = resp.response.clicked()
                        || resp.response.drag_started()
                        || resp.response.gained_focus();
                    let snapshot = Self::capture_snapshot(rt, &resp);
                    self.apply_events(&resp.events);
                    self.last_response = Some(snapshot);
                }
                #[cfg(not(target_arch = "wasm32"))]
                PaneBackend::Error(err_msg) => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        ui.colored_label(
                            egui::Color32::from_rgb(240, 100, 100),
                            "Failed to launch native PTY session",
                        );
                        ui.add_space(6.0);
                        ui.monospace(err_msg.as_str());
                    });
                }
            }
        });
        clicked
    }

    fn capture_snapshot<I: TerminalInput>(
        rt: &TerminalRuntime<I>,
        resp: &TerminalResponse,
    ) -> PaneMetricsSnapshot {
        let term = rt.terminal();
        PaneMetricsSnapshot {
            cols: resp.cols,
            rows: resp.rows,
            cell_size: resp.cell_size,
            effective_font_size: resp.effective_font_size,
            title: term.title().to_owned(),
            cwd: term.cwd().map(str::to_owned),
            display_offset: term.display_offset(),
            scrollback_len: term.scrollback_len(),
            alt_screen: term.is_alt_screen(),
            sync_output: term.is_synchronized_output(),
            #[cfg(feature = "shell-integration")]
            zone_count: term.shell_zones().len(),
            #[cfg(feature = "metrics")]
            metrics: *term.metrics(),
            closed: rt.input().is_closed(),
        }
    }

    fn apply_events(&mut self, events: &[TerminalEvent]) {
        for event in events {
            match event {
                TerminalEvent::TitleChanged(title) if !title.trim().is_empty() => {
                    self.label.clone_from(title);
                }
                TerminalEvent::Bell => {
                    self.last_notice = Some("Bell".to_owned());
                }
                TerminalEvent::CommandFinished { exit_code } => {
                    self.last_notice = Some(match exit_code {
                        Some(code) => format!("Exit {code}"),
                        None => "Command finished".to_owned(),
                    });
                }
                _ => {}
            }
        }
    }

    #[cfg(feature = "search")]
    fn toggle_search(&mut self) {
        match &mut self.backend {
            #[cfg(not(target_arch = "wasm32"))]
            PaneBackend::Pty(rt) => {
                if rt.search_bar_mut().open {
                    rt.close_search();
                } else {
                    rt.open_search();
                }
            }
            PaneBackend::Trait(rt) => {
                if rt.search_bar_mut().open {
                    rt.close_search();
                } else {
                    rt.open_search();
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            PaneBackend::Error(_) => {}
        }
    }

    #[cfg(feature = "shell-integration")]
    fn jump_to_prompt(&mut self, delta: i32) {
        match &mut self.backend {
            #[cfg(not(target_arch = "wasm32"))]
            PaneBackend::Pty(rt) => {
                rt.terminal_mut().jump_to_prompt(delta);
            }
            PaneBackend::Trait(rt) => {
                rt.terminal_mut().jump_to_prompt(delta);
            }
            #[cfg(not(target_arch = "wasm32"))]
            PaneBackend::Error(_) => {}
        }
    }
}

struct EguiXtermDemoApp {
    tabs: Vec<TerminalTab>,
    active_tab_idx: usize,
    split_view: bool,
    split_secondary_idx: usize,
    show_showcase_panel: bool,
    next_tab_id: u64,
    auto_cols_draft: u16,
    auto_rows_draft: u16,
}

impl EguiXtermDemoApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self {
            tabs: Vec::new(),
            active_tab_idx: 0,
            split_view: false,
            split_secondary_idx: 1,
            show_showcase_panel: true,
            next_tab_id: 1,
            auto_cols_draft: 80,
            auto_rows_draft: 24,
        };

        #[cfg(not(target_arch = "wasm32"))]
        {
            app.add_pty_tab(&cc.egui_ctx, TerminalConfig::default());
            app.add_trait_tab("Trait Demo", TerminalConfig::default());
        }

        #[cfg(target_arch = "wasm32")]
        {
            let _unused_cc = cc;
            app.add_trait_tab("Trait Demo", TerminalConfig::default());
            app.add_trait_tab("Trait Console #2", TerminalConfig::default());
        }

        app
    }

    fn alloc_tab_id(&mut self) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id = self.next_tab_id.saturating_add(1);
        id
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_pty_backend(ctx: &egui::Context, config: &TerminalConfig) -> PaneBackend {
        let repaint_ctx = ctx.clone();
        match PtyBackend::spawn_with_repaint(
            PtyConfig::default(),
            Box::new(move || repaint_ctx.request_repaint()),
        ) {
            Ok(pty) => PaneBackend::Pty(TerminalRuntime::with_config(pty, config.clone())),
            Err(err) => PaneBackend::Error(err.to_string()),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn add_pty_tab(&mut self, ctx: &egui::Context, config: TerminalConfig) {
        let id = self.alloc_tab_id();
        let backend = Self::spawn_pty_backend(ctx, &config);
        self.tabs.push(TerminalTab {
            id,
            label: format!("Local PTY #{id}"),
            is_pty: true,
            config,
            backend,
            last_response: None,
            last_notice: None,
        });
        let new_idx = self.tabs.len() - 1;
        self.select_tab(new_idx);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn restart_active_pty(&mut self, ctx: &egui::Context) {
        if let Some(tab) = self.tabs.get_mut(self.active_tab_idx)
            && tab.is_pty
        {
            tab.backend = Self::spawn_pty_backend(ctx, &tab.config);
            tab.last_notice = Some("PTY restarted".to_owned());
        }
    }

    fn add_trait_tab(&mut self, title: &str, config: TerminalConfig) {
        let id = self.alloc_tab_id();
        let shell = DemoShellInput::new(title);
        self.tabs.push(TerminalTab {
            id,
            label: title.to_owned(),
            is_pty: false,
            config: config.clone(),
            backend: PaneBackend::Trait(TerminalRuntime::with_config(shell, config)),
            last_response: None,
            last_notice: None,
        });
    }

    fn select_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        self.active_tab_idx = idx;
        if self.split_view && idx != 0 {
            self.split_secondary_idx = idx;
        }
        self.sync_draft_from_active();
    }

    fn toggle_split_view(&mut self) {
        if self.tabs.len() < 2 {
            return;
        }
        self.split_view = !self.split_view;
        if self.split_view {
            if self.active_tab_idx != 0 {
                self.split_secondary_idx = self.active_tab_idx;
            } else if self.split_secondary_idx == 0 || self.split_secondary_idx >= self.tabs.len() {
                self.split_secondary_idx = 1;
            }
        }
    }

    fn split_pane_indices(&self) -> (usize, usize) {
        let left_idx = 0usize;
        let right_idx = if self.split_secondary_idx == 0 {
            1
        } else {
            self.split_secondary_idx
                .min(self.tabs.len().saturating_sub(1))
        };
        (left_idx, right_idx)
    }

    fn run_showcase_on_trait_pane(&mut self, command: &str) {
        // Prefer the active tab if it is a Trait pane; otherwise target the first Trait pane.
        let target_idx = self
            .tabs
            .get(self.active_tab_idx)
            .filter(|t| matches!(t.backend, PaneBackend::Trait(_)))
            .map(|_| self.active_tab_idx)
            .or_else(|| {
                self.tabs
                    .iter()
                    .position(|t| matches!(t.backend, PaneBackend::Trait(_)))
            });

        if let Some(idx) = target_idx
            && let PaneBackend::Trait(rt) = &mut self.tabs[idx].backend
        {
            rt.input_mut().run_showcase(command);
            if self.split_view {
                if idx != 0 && self.split_secondary_idx != idx {
                    self.split_secondary_idx = idx;
                }
                if self.active_tab_idx != 0 && self.active_tab_idx != idx {
                    self.active_tab_idx = idx;
                    self.sync_draft_from_active();
                }
            } else {
                self.active_tab_idx = idx;
                self.sync_draft_from_active();
            }
        }
    }

    fn sync_draft_from_active(&mut self) {
        if let Some(tab) = self.tabs.get(self.active_tab_idx)
            && let ScaleMode::AutoScaleToFit { cols, rows } = tab.config.scale_mode
        {
            self.auto_cols_draft = cols;
            self.auto_rows_draft = rows;
        }
    }

    fn draw_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            self.draw_tab_strip(ui);
            ui.separator();
            self.draw_layout_and_font_controls(ui);
            ui.separator();
            self.draw_theme_and_cursor_controls(ui);
            ui.separator();
            self.draw_feature_controls(ui);
        });
    }

    fn draw_tab_strip(&mut self, ui: &mut egui::Ui) {
        for idx in 0..self.tabs.len() {
            let selected = idx == self.active_tab_idx;
            let clicked = ui
                .push_id(self.tabs[idx].id, |ui| {
                    ui.selectable_label(selected, &self.tabs[idx].label)
                        .clicked()
                })
                .inner;
            if clicked {
                self.select_tab(idx);
            }
        }

        ui.separator();

        let split_label = if self.split_view {
            "Single Tab"
        } else {
            "Split View"
        };
        if ui.button(split_label).clicked() {
            self.toggle_split_view();
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            if ui.button("New PTY").clicked() {
                let base_cfg = self
                    .tabs
                    .get(self.active_tab_idx)
                    .map(|t| t.config.clone())
                    .unwrap_or_default();
                let ctx = ui.ctx().clone();
                self.add_pty_tab(&ctx, base_cfg);
            }
            let is_pty_active = self.tabs.get(self.active_tab_idx).is_some_and(|t| t.is_pty);
            if is_pty_active && ui.button("Restart PTY").clicked() {
                let ctx = ui.ctx().clone();
                self.restart_active_pty(&ctx);
            }
        }
    }

    fn draw_layout_and_font_controls(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = self.tabs.get_mut(self.active_tab_idx) else {
            return;
        };
        let cfg = &mut tab.config;
        let is_reflow = matches!(cfg.scale_mode, ScaleMode::ReflowGrid);
        if ui.selectable_label(is_reflow, "ReflowGrid").clicked() {
            cfg.scale_mode = ScaleMode::ReflowGrid;
        }
        if ui.selectable_label(!is_reflow, "AutoScale").clicked() {
            cfg.scale_mode = ScaleMode::AutoScaleToFit {
                cols: self.auto_cols_draft.max(1),
                rows: self.auto_rows_draft.max(1),
            };
        }
        if let ScaleMode::AutoScaleToFit { cols, rows } = &mut cfg.scale_mode {
            let mut c = *cols;
            let mut r = *rows;
            ui.add(egui::DragValue::new(&mut c).range(10..=240).suffix("c"));
            ui.add(egui::DragValue::new(&mut r).range(4..=100).suffix("r"));
            *cols = c;
            *rows = r;
            self.auto_cols_draft = c;
            self.auto_rows_draft = r;
        }

        ui.separator();

        if ui.button("A-").clicked() {
            cfg.font_size = (cfg.font_size - 1.0).clamp(8.0, 36.0);
        }
        ui.label(format!("{:.0}pt", cfg.font_size));
        if ui.button("A+").clicked() {
            cfg.font_size = (cfg.font_size + 1.0).clamp(8.0, 36.0);
        }

        if ui.button("Zoom-").clicked() {
            cfg.zoom_factor = (cfg.zoom_factor - 0.1).clamp(0.5, 2.5);
        }
        if ui
            .button(format!("{:.0}%", cfg.zoom_factor * 100.0))
            .clicked()
        {
            cfg.zoom_factor = 1.0;
        }
        if ui.button("Zoom+").clicked() {
            cfg.zoom_factor = (cfg.zoom_factor + 0.1).clamp(0.5, 2.5);
        }
    }

    fn draw_theme_and_cursor_controls(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = self.tabs.get_mut(self.active_tab_idx) else {
            return;
        };
        let cfg = &mut tab.config;
        let dark = Theme::dark();
        let is_dark = cfg.theme.scheme == dark.scheme;
        if ui.selectable_label(is_dark, "Dark").clicked() {
            cfg.theme = Theme::dark();
        }
        if ui.selectable_label(!is_dark, "Light").clicked() {
            cfg.theme = Theme::light();
        }

        ui.separator();

        let shape_text = match cfg.cursor_shape {
            CursorShape::Block => "Block",
            CursorShape::Underline => "Underline",
            CursorShape::Bar => "Bar",
        };
        if ui.button(format!("Cursor: {shape_text}")).clicked() {
            cfg.cursor_shape = match cfg.cursor_shape {
                CursorShape::Block => CursorShape::Underline,
                CursorShape::Underline => CursorShape::Bar,
                CursorShape::Bar => CursorShape::Block,
            };
        }
        ui.checkbox(&mut cfg.cursor_blink, "Blink");
    }

    fn draw_feature_controls(&mut self, ui: &mut egui::Ui) {
        #[cfg(feature = "search")]
        if ui.button("Find").clicked()
            && let Some(tab) = self.tabs.get_mut(self.active_tab_idx)
        {
            tab.toggle_search();
        }

        #[cfg(feature = "shell-integration")]
        {
            if ui.button("Prev Prompt").clicked()
                && let Some(tab) = self.tabs.get_mut(self.active_tab_idx)
            {
                tab.jump_to_prompt(-1);
            }
            if ui.button("Next Prompt").clicked()
                && let Some(tab) = self.tabs.get_mut(self.active_tab_idx)
            {
                tab.jump_to_prompt(1);
            }
        }

        ui.toggle_value(&mut self.show_showcase_panel, "Showcase");
    }

    fn draw_showcase_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.heading("Trait Showcase");
        ui.label("Runs inside DemoShellInput:");
        ui.add_space(6.0);

        for (cmd, desc) in SHOWCASE_COMMANDS {
            if ui.button(*cmd).on_hover_text(*desc).clicked() {
                self.run_showcase_on_trait_pane(cmd);
            }
        }

        ui.separator();
        ui.small("Shortcuts:");
        ui.small("Ctrl+Shift+C : Copy");
        ui.small("Ctrl+Shift+V : Paste");
        ui.small("Ctrl+Shift+F : Search");
        ui.small("Shift+Drag   : Select");
    }

    fn draw_status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let Some(tab) = self.tabs.get(self.active_tab_idx) else {
                ui.label("No active terminal");
                return;
            };
            ui.strong(&tab.label);

            if let Some(snap) = &tab.last_response {
                ui.separator();
                ui.monospace(format!("{}x{}", snap.cols, snap.rows));
                ui.separator();
                ui.monospace(format!("{:.1}pt", snap.effective_font_size));
                ui.separator();
                ui.monospace(format!("{:.1}x{:.1}px", snap.cell_size.x, snap.cell_size.y));
                ui.separator();
                let mode_text = match tab.config.scale_mode {
                    ScaleMode::ReflowGrid => "ReflowGrid".to_owned(),
                    ScaleMode::AutoScaleToFit { cols, rows } => {
                        format!("AutoScale({cols}x{rows})")
                    }
                };
                ui.label(mode_text);
                ui.separator();
                ui.label(format!(
                    "Scroll {}/{}",
                    snap.display_offset, snap.scrollback_len
                ));
                if !snap.title.is_empty() {
                    ui.separator();
                    ui.label(format!("Title: {}", snap.title));
                }
                if let Some(cwd) = &snap.cwd {
                    ui.separator();
                    ui.label(format!("CWD: {cwd}"));
                }
                if snap.alt_screen {
                    ui.separator();
                    ui.colored_label(egui::Color32::LIGHT_BLUE, "ALT");
                }
                if snap.sync_output {
                    ui.separator();
                    ui.colored_label(egui::Color32::YELLOW, "SYNC");
                }
                if tab.is_pty {
                    ui.separator();
                    let status = if snap.closed { "Exited" } else { "Running" };
                    ui.label(format!("PTY: {status}"));
                }
                #[cfg(feature = "shell-integration")]
                {
                    ui.separator();
                    ui.label(format!("Zones: {}", snap.zone_count));
                }
                #[cfg(feature = "metrics")]
                {
                    ui.separator();
                    ui.monospace(format!(
                        "bytes={} frames={} dirty={} reflows={}",
                        snap.metrics.bytes_processed,
                        snap.metrics.frames_rendered,
                        snap.metrics.dirty_rows_rendered,
                        snap.metrics.reflow_count,
                    ));
                }
            }
            if let Some(notice) = &tab.last_notice {
                ui.separator();
                ui.label(notice);
            }
        });
    }
}

impl EguiXtermDemoApp {
    fn show_ui(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("demo_toolbar").show(ui, |ui| {
            self.draw_toolbar(ui);
        });

        egui::Panel::bottom("demo_status_bar").show(ui, |ui| {
            self.draw_status_bar(ui);
        });

        if self.show_showcase_panel {
            egui::Panel::right("showcase_panel")
                .resizable(false)
                .default_size(170.0)
                .show(ui, |ui| {
                    self.draw_showcase_sidebar(ui);
                });
        }

        if self.split_view && self.tabs.len() >= 2 {
            let (left_idx, right_idx) = self.split_pane_indices();

            let avail_w = ui.available_width();
            let mut activate_left = false;
            let mut activate_right = false;

            egui::Panel::left("split_left_pane")
                .resizable(true)
                .default_size((avail_w * 0.5).max(240.0))
                .min_size(180.0)
                .show(ui, |ui| {
                    if let Some(tab) = self.tabs.get_mut(left_idx) {
                        activate_left = tab.show_in_frame(ui, self.active_tab_idx == left_idx);
                    }
                });

            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    if let Some(tab) = self.tabs.get_mut(right_idx) {
                        activate_right = tab.show_in_frame(ui, self.active_tab_idx == right_idx);
                    }
                });

            if activate_left {
                self.select_tab(left_idx);
            } else if activate_right {
                self.select_tab(right_idx);
            }
        } else {
            let active = self.active_tab_idx.min(self.tabs.len().saturating_sub(1));
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    if let Some(tab) = self.tabs.get_mut(active) {
                        tab.show_in_frame(ui, true);
                    }
                });
        }
    }
}

impl eframe::App for EguiXtermDemoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show_ui(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app_with_tabs(count: usize) -> EguiXtermDemoApp {
        let mut app = EguiXtermDemoApp {
            tabs: Vec::new(),
            active_tab_idx: 0,
            split_view: false,
            split_secondary_idx: 1,
            show_showcase_panel: true,
            next_tab_id: 1,
            auto_cols_draft: 80,
            auto_rows_draft: 24,
        };
        for idx in 0..count {
            app.add_trait_tab(&format!("Tab #{idx}"), TerminalConfig::default());
        }
        app
    }

    #[test]
    fn split_view_invariants_preserve_third_tab_visibility() {
        let mut app = test_app_with_tabs(3);

        // Toggling split view on while tab 2 is active makes tab 2 the right split pane.
        app.select_tab(2);
        app.toggle_split_view();
        assert!(app.split_view);
        assert_eq!(app.active_tab_idx, 2);
        assert_eq!(app.split_secondary_idx, 2);
        assert_eq!(app.split_pane_indices(), (0, 2));

        // Selecting tab 1 in split view moves secondary pane to 1; selecting tab 0 keeps 1 on right.
        app.select_tab(1);
        assert_eq!(app.active_tab_idx, 1);
        assert_eq!(app.split_pane_indices(), (0, 1));

        app.select_tab(0);
        assert_eq!(app.active_tab_idx, 0);
        assert_eq!(app.split_pane_indices(), (0, 1));

        // Selecting tab 2 in split view updates secondary pane to 2.
        app.select_tab(2);
        assert_eq!(app.active_tab_idx, 2);
        assert_eq!(app.split_pane_indices(), (0, 2));
    }

    #[test]
    fn split_view_showcase_brings_trait_pane_into_view_without_hidden_active_tab() {
        let mut app = test_app_with_tabs(3);
        // Simulate tab 0 and tab 2 as non-Trait tabs so tab 1 is the only Trait target.
        #[cfg(not(target_arch = "wasm32"))]
        {
            app.tabs[0].backend = PaneBackend::Error("mock non-trait 0".to_owned());
            app.tabs[2].backend = PaneBackend::Error("mock non-trait 2".to_owned());
        }
        app.select_tab(2);
        app.toggle_split_view();
        assert_eq!(app.split_pane_indices(), (0, 2));

        app.run_showcase_on_trait_pane("colors");
        assert_eq!(app.split_pane_indices(), (0, 1));
        assert!(
            app.active_tab_idx == 0 || app.active_tab_idx == 1,
            "active tab {} must remain visible in split view",
            app.active_tab_idx
        );
    }

    #[test]
    fn split_view_toolbar_tab_switch_not_overwritten_by_focused_left_pane() {
        let mut app = test_app_with_tabs(2);
        app.toggle_split_view();
        assert!(app.split_view);

        let ctx = egui::Context::default();
        let make_input = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1240.0, 780.0),
            )),
            focused: true,
            events,
            ..Default::default()
        };

        // Frame 1: initial layout.
        let mut out = ctx.run_ui(make_input(vec![]), |ui| app.show_ui(ui));
        out.textures_delta.clear();

        // Frame 2: click inside the left pane to focus it.
        let left_click_pos = egui::pos2(120.0, 260.0);
        let mut out = ctx.run_ui(
            make_input(vec![
                egui::Event::PointerMoved(left_click_pos),
                egui::Event::PointerButton {
                    pos: left_click_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: left_click_pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]),
            |ui| app.show_ui(ui),
        );
        out.textures_delta.clear();
        assert_eq!(app.active_tab_idx, 0);

        // Simulate toolbar selecting Tab #1 while left pane still holds keyboard focus.
        app.select_tab(1);
        assert_eq!(app.active_tab_idx, 1);

        // Frame 3 & 4: subsequent frames must NOT revert active_tab_idx back to 0.
        let mut out = ctx.run_ui(make_input(vec![]), |ui| app.show_ui(ui));
        out.textures_delta.clear();
        assert_eq!(
            app.active_tab_idx, 1,
            "focused left pane must not overwrite toolbar tab switch"
        );
        let mut out = ctx.run_ui(make_input(vec![]), |ui| app.show_ui(ui));
        out.textures_delta.clear();
        assert_eq!(app.active_tab_idx, 1);
    }
}
