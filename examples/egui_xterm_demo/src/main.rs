//! Interactive `eframe` 0.37 demo application showcasing `egui_xterm`.
//!
//! Demonstrates desktop PTY sessions (`egui_xterm::pty`), cross-platform trait-driven
//! consoles ([`demo_shell::DemoShellInput`]), split-pane auto-layout inside [`egui::Frame`]
//! containers, [`ScaleMode::ReflowGrid`] vs [`ScaleMode::AutoScaleToFit`], themes,
//! cursor customization, and optional features (`search`, `shell-integration`, `metrics`).

#![forbid(unsafe_code)]

mod demo_shell;

use std::borrow::Cow;

use demo_shell::{DemoShellInput, SHOWCASE_COMMANDS};
use eframe::egui;
#[cfg(not(target_arch = "wasm32"))]
use egui_xterm::pty::{PtyBackend, PtyConfig};
use egui_xterm::{
    ClipboardSelection, CursorShape, Osc52ReadPolicy, ProgressIndicator, ScaleMode, TerminalConfig,
    TerminalEvent, TerminalInput, TerminalIoError, TerminalResponse, TerminalRuntime, Theme,
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

trait AnyRuntime {
    fn show_with_config(
        &mut self,
        ui: &mut egui::Ui,
        config: &mut TerminalConfig,
    ) -> TerminalResponse;
    fn snapshot(&self, resp: &TerminalResponse) -> PaneMetricsSnapshot;
    fn last_io_error_string(&self) -> Option<String>;
    fn take_io_error(&mut self) -> Option<TerminalIoError>;
    fn pending_osc52_read(&self) -> Option<ClipboardSelection>;
    fn approve_osc52_read(&mut self, text: &str) -> bool;
    fn deny_osc52_read(&mut self) -> bool;
    fn provide_clipboard_read(&mut self, selection: ClipboardSelection, text: &str);
    #[cfg(feature = "search")]
    fn toggle_search(&mut self);
    #[cfg(feature = "shell-integration")]
    fn jump_to_prompt(&mut self, delta: i32);
}

impl<I: TerminalInput> AnyRuntime for TerminalRuntime<I> {
    fn show_with_config(
        &mut self,
        ui: &mut egui::Ui,
        config: &mut TerminalConfig,
    ) -> TerminalResponse {
        TerminalRuntime::show_with_config(self, ui, config)
    }

    fn snapshot(&self, resp: &TerminalResponse) -> PaneMetricsSnapshot {
        let term = self.terminal();
        PaneMetricsSnapshot {
            cols: resp.cols,
            rows: resp.rows,
            cell_size: resp.cell_size,
            effective_font_size: resp.effective_font_size,
            title: term.title().to_owned(),
            cwd: term.cwd().map(str::to_owned),
            document_uri: term.document_uri().map(str::to_owned),
            display_offset: term.display_offset(),
            scrollback_len: term.scrollback_len(),
            alt_screen: term.is_alt_screen(),
            sync_output: term.is_synchronized_output(),
            progress: self.progress(),
            last_io_error: self.last_io_error().map(ToString::to_string),
            #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
            image_count: term.image_placements().len(),
            #[cfg(feature = "shell-integration")]
            zone_count: term.shell_zones().len(),
            #[cfg(feature = "metrics")]
            metrics: *term.metrics(),
            closed: self.input().is_closed(),
        }
    }

    fn last_io_error_string(&self) -> Option<String> {
        self.last_io_error().map(ToString::to_string)
    }

    fn take_io_error(&mut self) -> Option<TerminalIoError> {
        TerminalRuntime::take_io_error(self)
    }

    fn pending_osc52_read(&self) -> Option<ClipboardSelection> {
        TerminalRuntime::pending_osc52_read(self)
    }

    fn approve_osc52_read(&mut self, text: &str) -> bool {
        TerminalRuntime::approve_osc52_read(self, text)
    }

    fn deny_osc52_read(&mut self) -> bool {
        TerminalRuntime::deny_osc52_read(self)
    }

    fn provide_clipboard_read(&mut self, selection: ClipboardSelection, text: &str) {
        self.terminal_mut().provide_clipboard_read(selection, text);
    }

    #[cfg(feature = "search")]
    fn toggle_search(&mut self) {
        if self.search_bar_mut().open {
            self.close_search();
        } else {
            self.open_search();
        }
    }

    #[cfg(feature = "shell-integration")]
    fn jump_to_prompt(&mut self, delta: i32) {
        self.terminal_mut().jump_to_prompt(delta);
    }
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
    last_notification: Option<String>,
}

#[derive(Clone)]
struct PaneMetricsSnapshot {
    cols: u16,
    rows: u16,
    cell_size: egui::Vec2,
    effective_font_size: f32,
    title: String,
    cwd: Option<String>,
    document_uri: Option<String>,
    display_offset: usize,
    scrollback_len: usize,
    alt_screen: bool,
    sync_output: bool,
    progress: Option<ProgressIndicator>,
    last_io_error: Option<String>,
    #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
    image_count: usize,
    #[cfg(feature = "shell-integration")]
    zone_count: usize,
    #[cfg(feature = "metrics")]
    metrics: egui_xterm::TerminalMetrics,
    closed: bool,
}

const fn clipboard_selection_label(selection: ClipboardSelection) -> &'static str {
    match selection {
        ClipboardSelection::Clipboard => "Clipboard",
        ClipboardSelection::Primary => "Primary",
        ClipboardSelection::Secondary => "Secondary",
        ClipboardSelection::Select => "Select",
    }
}

impl TerminalTab {
    fn runtime(&self) -> Option<&dyn AnyRuntime> {
        match &self.backend {
            #[cfg(not(target_arch = "wasm32"))]
            PaneBackend::Pty(rt) => Some(rt),
            PaneBackend::Trait(rt) => Some(rt),
            #[cfg(not(target_arch = "wasm32"))]
            PaneBackend::Error(_) => None,
        }
    }

    fn runtime_mut(&mut self) -> Option<&mut dyn AnyRuntime> {
        match &mut self.backend {
            #[cfg(not(target_arch = "wasm32"))]
            PaneBackend::Pty(rt) => Some(rt),
            PaneBackend::Trait(rt) => Some(rt),
            #[cfg(not(target_arch = "wasm32"))]
            PaneBackend::Error(_) => None,
        }
    }

    fn show_in_frame(
        &mut self,
        ui: &mut egui::Ui,
        is_active: bool,
        osc52_reply_draft: &mut String,
    ) -> bool {
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

            let had_io_error = self
                .last_response
                .as_ref()
                .is_some_and(|s| s.last_io_error.is_some());
            let had_osc52 = self.pending_osc52_read().is_some();

            let rendered = {
                let rt: Option<&mut dyn AnyRuntime> = match &mut self.backend {
                    #[cfg(not(target_arch = "wasm32"))]
                    PaneBackend::Pty(rt) => Some(rt),
                    PaneBackend::Trait(rt) => Some(rt),
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
                        None
                    }
                };
                rt.map(|runtime| {
                    let resp = runtime.show_with_config(ui, &mut self.config);
                    let snapshot = runtime.snapshot(&resp);
                    (resp, snapshot)
                })
            };

            if let Some((resp, snapshot)) = rendered {
                clicked = resp.response.clicked()
                    || resp.response.drag_started()
                    || resp.response.gained_focus();
                let allow_reads = self.apply_events(&resp.events, osc52_reply_draft);
                if !allow_reads.is_empty()
                    && let Some(rt) = self.runtime_mut()
                {
                    for selection in allow_reads {
                        rt.provide_clipboard_read(selection, osc52_reply_draft);
                    }
                    ui.ctx().request_repaint();
                }
                let has_io_error = snapshot.last_io_error.is_some();
                self.last_response = Some(snapshot);
                let has_osc52 = self.pending_osc52_read().is_some();
                if (!had_io_error && has_io_error) || (!had_osc52 && has_osc52) {
                    ui.ctx().request_repaint();
                }
            }
        });
        clicked
    }

    fn pending_io_error(&self) -> Option<Cow<'_, str>> {
        if let Some(msg) = self
            .last_response
            .as_ref()
            .and_then(|s| s.last_io_error.as_deref())
        {
            return Some(Cow::Borrowed(msg));
        }
        self.runtime()
            .and_then(AnyRuntime::last_io_error_string)
            .map(Cow::Owned)
    }

    fn sync_io_error_from_runtime(&mut self) {
        let err = self.runtime().and_then(AnyRuntime::last_io_error_string);
        if let Some(snap) = &mut self.last_response {
            snap.last_io_error = err;
        }
    }

    fn dismiss_io_error(&mut self) {
        if let Some(rt) = self.runtime_mut() {
            drop(rt.take_io_error());
        }
        if let Some(snap) = &mut self.last_response {
            snap.last_io_error = None;
        }
    }

    fn pending_osc52_read(&self) -> Option<ClipboardSelection> {
        self.runtime().and_then(AnyRuntime::pending_osc52_read)
    }

    fn approve_osc52_read(&mut self, text: &str) -> bool {
        let approved = self
            .runtime_mut()
            .is_some_and(|rt| rt.approve_osc52_read(text));
        if approved {
            self.sync_io_error_from_runtime();
        }
        approved
    }

    fn deny_osc52_read(&mut self) -> bool {
        self.runtime_mut().is_some_and(AnyRuntime::deny_osc52_read)
    }

    fn apply_events(
        &mut self,
        events: &[TerminalEvent],
        osc52_reply_draft: &mut String,
    ) -> Vec<ClipboardSelection> {
        let mut allow_reads = Vec::new();
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
                TerminalEvent::ClipboardWrite(text) if self.config.osc52_write_enabled => {
                    if !text.is_empty() {
                        osc52_reply_draft.clone_from(text);
                    }
                    self.last_notice = Some("OSC 52 write".to_owned());
                }
                TerminalEvent::ClipboardReadRequested { selection } => {
                    allow_reads.push(*selection);
                    self.last_notice = Some(format!(
                        "OSC 52 read ({})",
                        clipboard_selection_label(*selection)
                    ));
                }
                TerminalEvent::ClipboardReadPrompt { selection } => {
                    self.last_notice = Some(format!(
                        "OSC 52 prompt ({})",
                        clipboard_selection_label(*selection)
                    ));
                }
                TerminalEvent::Notification { title, body } => {
                    let msg = if title.is_empty() {
                        body.clone()
                    } else if body.is_empty() {
                        title.clone()
                    } else {
                        format!("{title}: {body}")
                    };
                    if !msg.is_empty() {
                        self.last_notification = Some(msg);
                    }
                }
                TerminalEvent::NotificationClosed { id } => {
                    self.last_notification = Some(format!("Closed ({id})"));
                }
                TerminalEvent::IoError(err) => {
                    self.last_notice = Some(format!("I/O error: {err}"));
                }
                _ => {}
            }
        }
        allow_reads
    }

    #[cfg(feature = "search")]
    fn toggle_search(&mut self) {
        if let Some(rt) = self.runtime_mut() {
            rt.toggle_search();
        }
    }

    #[cfg(feature = "shell-integration")]
    fn jump_to_prompt(&mut self, delta: i32) {
        if let Some(rt) = self.runtime_mut() {
            rt.jump_to_prompt(delta);
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
    osc52_reply_draft: String,
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
            osc52_reply_draft: "egui_xterm clipboard".to_owned(),
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
            last_notification: None,
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
            last_notification: None,
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

        if let Some(tab) = self.tabs.get_mut(self.active_tab_idx) {
            let policy_label = match tab.config.osc52_read_policy {
                Osc52ReadPolicy::Deny => "Deny",
                Osc52ReadPolicy::Prompt => "Prompt",
                Osc52ReadPolicy::Allow => "Allow",
            };
            if ui
                .button(format!("OSC 52 Read: {policy_label}"))
                .on_hover_text("Cycle OSC 52 clipboard read policy (Deny -> Prompt -> Allow)")
                .clicked()
            {
                tab.config.osc52_read_policy = match tab.config.osc52_read_policy {
                    Osc52ReadPolicy::Deny => Osc52ReadPolicy::Prompt,
                    Osc52ReadPolicy::Prompt => Osc52ReadPolicy::Allow,
                    Osc52ReadPolicy::Allow => Osc52ReadPolicy::Deny,
                };
            }
            ui.checkbox(&mut tab.config.osc52_write_enabled, "OSC 52 Write")
                .on_hover_text("Allow OSC 52 sequences to write to the host clipboard");
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
                if let Some(doc_uri) = &snap.document_uri {
                    ui.separator();
                    ui.label(format!("Doc: {doc_uri}"));
                }
                if let Some(ind) = snap.progress {
                    ui.separator();
                    let (label, color) = match ind.state {
                        1 => (
                            format!("Progress: {}%", ind.progress),
                            ui.visuals().text_color(),
                        ),
                        2 => (
                            format!("Progress Err: {}%", ind.progress),
                            ui.visuals().error_fg_color,
                        ),
                        3 => (
                            "Progress: Busy".to_owned(),
                            egui::Color32::from_rgb(100, 181, 246),
                        ),
                        4 => (
                            format!("Progress Paused: {}%", ind.progress),
                            egui::Color32::from_rgb(255, 183, 77),
                        ),
                        _ => (
                            format!("Progress: {}%", ind.progress),
                            ui.visuals().text_color(),
                        ),
                    };
                    ui.colored_label(color, label);
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
                #[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
                if snap.image_count > 0 {
                    ui.separator();
                    ui.label(format!("Images: {}", snap.image_count));
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
            if let Some(selection) = tab.pending_osc52_read() {
                ui.separator();
                ui.colored_label(
                    egui::Color32::from_rgb(255, 193, 7),
                    format!("OSC 52 Pending ({})", clipboard_selection_label(selection)),
                );
            }
            if let Some(notification) = &tab.last_notification {
                ui.separator();
                ui.label(format!("Notify: {notification}"));
            }
            if let Some(notice) = &tab.last_notice {
                ui.separator();
                ui.label(notice);
            }
        });
    }
}

impl EguiXtermDemoApp {
    fn find_pending_io_error(&self) -> Option<(usize, String)> {
        if let Some(tab) = self.tabs.get(self.active_tab_idx)
            && let Some(msg) = tab.pending_io_error()
        {
            return Some((self.active_tab_idx, msg.into_owned()));
        }
        self.tabs
            .iter()
            .enumerate()
            .find_map(|(idx, tab)| tab.pending_io_error().map(|msg| (idx, msg.into_owned())))
    }

    fn find_pending_osc52_read(&self) -> Option<(usize, ClipboardSelection)> {
        if let Some(tab) = self.tabs.get(self.active_tab_idx)
            && let Some(sel) = tab.pending_osc52_read()
        {
            return Some((self.active_tab_idx, sel));
        }
        self.tabs
            .iter()
            .enumerate()
            .find_map(|(idx, tab)| tab.pending_osc52_read().map(|sel| (idx, sel)))
    }

    fn draw_io_error_banner(&mut self, ui: &mut egui::Ui) {
        let Some((err_idx, err_msg)) = self.find_pending_io_error() else {
            return;
        };
        let tab_label = self
            .tabs
            .get(err_idx)
            .map_or("Terminal", |t| t.label.as_str());
        let banner_text = format!("I/O error ({tab_label}): {err_msg}");
        egui::Panel::top("demo_io_error_banner").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(ui.visuals().error_fg_color, banner_text);
                if ui.button("Dismiss").clicked()
                    && let Some(tab) = self.tabs.get_mut(err_idx)
                {
                    tab.dismiss_io_error();
                }
            });
        });
    }

    fn draw_osc52_banner(&mut self, ui: &mut egui::Ui) {
        let Some((tab_idx, selection)) = self.find_pending_osc52_read() else {
            return;
        };
        let selection_label = clipboard_selection_label(selection);
        let tab_label = self
            .tabs
            .get(tab_idx)
            .map_or("Terminal", |t| t.label.as_str());
        let prompt_text = format!("OSC 52 clipboard read ({tab_label} · {selection_label}):");

        egui::Panel::top("demo_osc52_banner").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(egui::Color32::from_rgb(255, 193, 7), prompt_text);
                ui.add(
                    egui::TextEdit::singleline(&mut self.osc52_reply_draft)
                        .desired_width(180.0)
                        .hint_text("Clipboard reply text"),
                );
                if ui.button("Approve").clicked()
                    && let Some(tab) = self.tabs.get_mut(tab_idx)
                    && tab.approve_osc52_read(&self.osc52_reply_draft)
                {
                    tab.last_notice = Some(format!("OSC 52 read approved ({selection_label})"));
                }
                if ui.button("Deny").clicked()
                    && let Some(tab) = self.tabs.get_mut(tab_idx)
                    && tab.deny_osc52_read()
                {
                    tab.last_notice = Some(format!("OSC 52 read denied ({selection_label})"));
                }
            });
        });
    }

    fn show_ui(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("demo_toolbar").show(ui, |ui| {
            self.draw_toolbar(ui);
        });

        self.draw_io_error_banner(ui);
        self.draw_osc52_banner(ui);

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
                        activate_left = tab.show_in_frame(
                            ui,
                            self.active_tab_idx == left_idx,
                            &mut self.osc52_reply_draft,
                        );
                    }
                });

            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    if let Some(tab) = self.tabs.get_mut(right_idx) {
                        activate_right = tab.show_in_frame(
                            ui,
                            self.active_tab_idx == right_idx,
                            &mut self.osc52_reply_draft,
                        );
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
                        tab.show_in_frame(ui, true, &mut self.osc52_reply_draft);
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
            osc52_reply_draft: "egui_xterm clipboard".to_owned(),
        };
        for idx in 0..count {
            app.add_trait_tab(&format!("Tab #{idx}"), TerminalConfig::default());
        }
        app
    }

    fn make_raw_input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1240.0, 780.0),
            )),
            focused: true,
            events,
            ..Default::default()
        }
    }

    fn find_text_rect_in_shapes(
        shapes: &[egui::epaint::ClippedShape],
        needle: &str,
    ) -> Option<egui::Rect> {
        fn visit(shape: &egui::Shape, needle: &str) -> Option<egui::Rect> {
            match shape {
                egui::Shape::Text(ts) if ts.galley.job.text.contains(needle) => {
                    Some(ts.galley.rect.translate(ts.pos.to_vec2()))
                }
                egui::Shape::Vec(children) => {
                    children.iter().find_map(|child| visit(child, needle))
                }
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| visit(&cs.shape, needle))
    }

    fn click_at(
        ctx: &egui::Context,
        app: &mut EguiXtermDemoApp,
        pos: egui::Pos2,
    ) -> egui::FullOutput {
        ctx.run_ui(
            make_raw_input(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]),
            |ui| app.show_ui(ui),
        )
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

    #[test]
    fn status_bar_and_io_error_banner_surface_modern_osc_and_dismiss() {
        let mut app = test_app_with_tabs(1);
        app.run_showcase_on_trait_pane("modern");

        let ctx = egui::Context::default();
        let make_input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1240.0, 780.0),
            )),
            focused: true,
            ..Default::default()
        };

        // Frame 1: showcase emits OSC 6, 9;4, 22, 99 -> captured into snapshot & status bar.
        let mut out1 = ctx.run_ui(make_input(), |ui| app.show_ui(ui));
        out1.textures_delta.clear();
        let snap = app.tabs[0].last_response.as_ref().expect("snapshot");
        assert_eq!(
            snap.document_uri.as_deref(),
            Some("file://localhost/workspace/demo/README.md")
        );
        assert_eq!(
            snap.progress,
            Some(ProgressIndicator {
                state: 1,
                progress: 65,
            })
        );
        assert!(
            app.tabs[0]
                .last_notification
                .as_deref()
                .is_some_and(|n| n.contains("egui_xterm Demo") && n.contains("OSC 9;4"))
        );

        // Simulated I/O error on snapshot renders banner and dismiss clears it cleanly.
        if let Some(snap_mut) = &mut app.tabs[0].last_response {
            snap_mut.last_io_error = Some("simulated transport fault".to_owned());
        }
        let mut out2 = ctx.run_ui(make_input(), |ui| app.show_ui(ui));
        out2.textures_delta.clear();
        let has_error_text = out2.shapes.iter().any(|cs| {
            matches!(
                &cs.shape,
                egui::Shape::Text(ts) if ts.galley.job.text.contains("simulated transport fault")
            )
        });
        assert!(has_error_text);

        app.tabs[0].dismiss_io_error();
        assert!(
            app.tabs[0]
                .last_response
                .as_ref()
                .and_then(|s| s.last_io_error.as_ref())
                .is_none()
        );
    }

    #[test]
    fn osc52_pending_read_banner_approve_and_deny_controls_roundtrip() {
        let mut app = test_app_with_tabs(1);
        app.tabs[0].config.osc52_read_policy = Osc52ReadPolicy::Prompt;
        app.run_showcase_on_trait_pane("osc52");

        let ctx = egui::Context::default();
        // Frame 1 processes `osc52` output -> sets pending_osc52_read() and copies write payload.
        let mut out1 = ctx.run_ui(make_raw_input(vec![]), |ui| app.show_ui(ui));
        out1.textures_delta.clear();
        assert_eq!(
            app.tabs[0].pending_osc52_read(),
            Some(ClipboardSelection::Clipboard)
        );
        assert_eq!(app.osc52_reply_draft, "egui_xterm OSC 52");

        // Frame 2 draws `demo_osc52_banner` with Approve / Deny buttons.
        app.osc52_reply_draft = "approved-secret".to_owned();
        let mut out2 = ctx.run_ui(make_raw_input(vec![]), |ui| app.show_ui(ui));
        let approve_rect = find_text_rect_in_shapes(&out2.shapes, "Approve")
            .expect("Approve button must render in OSC 52 banner");
        assert!(
            find_text_rect_in_shapes(&out2.shapes, "OSC 52 clipboard read").is_some(),
            "expected OSC 52 prompt label in banner"
        );
        out2.textures_delta.clear();

        // Click Approve button -> clears pending read and delivers reply to DemoShellInput.
        let mut out3 = click_at(&ctx, &mut app, approve_rect.center());
        out3.textures_delta.clear();
        assert_eq!(app.tabs[0].pending_osc52_read(), None);
        assert_eq!(
            app.tabs[0].last_notice.as_deref(),
            Some("OSC 52 read approved (Clipboard)")
        );
        let PaneBackend::Trait(rt) = &app.tabs[0].backend else {
            panic!("expected Trait backend");
        };
        assert!(
            rt.terminal()
                .full_text()
                .contains("[OSC 52 reply (c)] approved-secret"),
            "expected approved OSC 52 reply in terminal text: {:?}",
            rt.terminal().full_text()
        );

        // Trigger a second OSC 52 Primary read prompt and click Deny.
        let PaneBackend::Trait(rt) = &mut app.tabs[0].backend else {
            panic!("expected Trait backend");
        };
        rt.terminal_mut().advance(b"\x1b]52;p;?\x07");
        let mut out4 = ctx.run_ui(make_raw_input(vec![]), |ui| app.show_ui(ui));
        out4.textures_delta.clear();
        assert_eq!(
            app.tabs[0].pending_osc52_read(),
            Some(ClipboardSelection::Primary)
        );

        let mut out5 = ctx.run_ui(make_raw_input(vec![]), |ui| app.show_ui(ui));
        let deny_rect = find_text_rect_in_shapes(&out5.shapes, "Deny")
            .expect("Deny button must render in OSC 52 banner");
        out5.textures_delta.clear();

        let mut out6 = click_at(&ctx, &mut app, deny_rect.center());
        out6.textures_delta.clear();
        assert_eq!(app.tabs[0].pending_osc52_read(), None);
        assert_eq!(
            app.tabs[0].last_notice.as_deref(),
            Some("OSC 52 read denied (Primary)")
        );
        let PaneBackend::Trait(rt) = &app.tabs[0].backend else {
            panic!("expected Trait backend");
        };
        assert!(!rt.terminal().full_text().contains("[OSC 52 reply (p)]"));
    }

    #[test]
    fn real_io_error_surfaces_in_banner_and_dismiss_button_clears_runtime() {
        let mut app = test_app_with_tabs(1);
        let PaneBackend::Trait(rt) = &mut app.tabs[0].backend else {
            panic!("expected Trait backend");
        };
        rt.input_mut()
            .inject_io_error(TerminalIoError::message("simulated pipe fault"));

        let ctx = egui::Context::default();
        // Frame 1 polls DemoShellInput -> runtime records `last_io_error`.
        let mut out1 = ctx.run_ui(make_raw_input(vec![]), |ui| app.show_ui(ui));
        out1.textures_delta.clear();
        assert!(
            app.tabs[0]
                .pending_io_error()
                .is_some_and(|e| e.contains("simulated pipe fault"))
        );

        // Frame 2 displays `demo_io_error_banner` with Dismiss button.
        let mut out2 = ctx.run_ui(make_raw_input(vec![]), |ui| app.show_ui(ui));
        assert!(
            find_text_rect_in_shapes(&out2.shapes, "simulated pipe fault").is_some(),
            "expected real I/O error text in banner"
        );
        let dismiss_rect = find_text_rect_in_shapes(&out2.shapes, "Dismiss")
            .expect("Dismiss button must render in I/O error banner");
        out2.textures_delta.clear();

        // Clicking Dismiss clears both runtime `last_io_error()` and cached snapshot error.
        let mut out3 = click_at(&ctx, &mut app, dismiss_rect.center());
        out3.textures_delta.clear();
        assert!(app.tabs[0].pending_io_error().is_none());
        let PaneBackend::Trait(rt) = &app.tabs[0].backend else {
            panic!("expected Trait backend");
        };
        assert!(rt.last_io_error().is_none());
    }

    #[cfg(all(feature = "kitty-graphics", feature = "sixel"))]
    #[test]
    fn graphics_showcase_updates_image_count_and_renders_textured_meshes() {
        let mut app = test_app_with_tabs(1);
        app.run_showcase_on_trait_pane("graphics");

        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(make_raw_input(vec![]), |ui| app.show_ui(ui));
        let snap = app.tabs[0].last_response.as_ref().expect("snapshot");
        assert_eq!(snap.image_count, 2);

        let has_textured_mesh = out.shapes.iter().any(|cs| {
            matches!(
                &cs.shape,
                egui::Shape::Mesh(m) if m.texture_id != egui::TextureId::default()
            )
        });
        out.textures_delta.clear();
        assert!(
            has_textured_mesh,
            "expected textured meshes for Kitty & Sixel graphics"
        );
    }
}
