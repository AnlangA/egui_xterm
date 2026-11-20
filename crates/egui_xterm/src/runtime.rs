//! Frame-driven [`TerminalRuntime`] and [`TerminalWidget`] integration for `egui`.

use std::sync::atomic::{AtomicU64, Ordering};

use egui::{Ui, Widget};
use egui_xterm_core::{CoreConfig, MouseProtocol, Terminal, TerminalInput};

use crate::config::{TerminalConfig, TerminalResponse};
use crate::input_map::{InputMapperState, copy_text_everywhere, try_read_native_clipboard};
use crate::layout::TerminalLayoutState;
use crate::renderer::TerminalRenderer;
use crate::widgets::WidgetAction;

static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);
const POLL_CHUNK_BYTES: usize = 65_536;

/// Stateful `egui` terminal runtime wrapping a [`Terminal`] and a user-provided [`TerminalInput`].
///
/// Calling [`Self::show`] or [`Self::show_with_config`] inside any `egui` container automatically
/// detects the allocated area, snaps cell geometry to physical pixels, updates `(cols, rows)` or
/// font scaling, drains input up to `read_budget_bytes_per_frame`, routes keyboard/IME/mouse/
/// clipboard events, and renders via the 4-stage cached pipeline.
pub struct TerminalRuntime<I: TerminalInput> {
    instance_id: u64,
    input: I,
    terminal: Terminal,
    config: TerminalConfig,
    layout_state: TerminalLayoutState,
    renderer: TerminalRenderer,
    input_mapper: InputMapperState,
    poll_scratch: Vec<u8>,
    secondary_press_in_search_bar: bool,
    scrollbar_secondary_press_in_track: bool,
    #[cfg(feature = "search")]
    search_bar: crate::widgets::search_bar::SearchBarState,
}

impl<I: TerminalInput> TerminalRuntime<I> {
    /// Creates a new runtime with [`TerminalConfig::default()`].
    #[must_use]
    pub fn new(input: I) -> Self {
        Self::with_config(input, TerminalConfig::default())
    }

    /// Creates a new runtime initialized from `config`.
    #[must_use]
    pub fn with_config(input: I, config: TerminalConfig) -> Self {
        let core_cfg = CoreConfig {
            scrollback_limit: config.scrollback_limit,
            theme: config.theme.clone(),
            default_cursor_shape: config.cursor_shape,
            default_cursor_blink: config.cursor_blink,
            ..CoreConfig::default()
        };
        let terminal = Terminal::new(80, 24, core_cfg);
        Self {
            instance_id: NEXT_RUNTIME_ID.fetch_add(1, Ordering::Relaxed),
            input,
            terminal,
            config,
            layout_state: TerminalLayoutState::default(),
            renderer: TerminalRenderer::default(),
            input_mapper: InputMapperState::default(),
            poll_scratch: vec![0u8; POLL_CHUNK_BYTES],
            secondary_press_in_search_bar: false,
            scrollbar_secondary_press_in_track: false,
            #[cfg(feature = "search")]
            search_bar: crate::widgets::search_bar::SearchBarState::default(),
        }
    }

    /// Renders and updates the terminal using its internal [`TerminalConfig`].
    pub fn show(&mut self, ui: &mut Ui) -> TerminalResponse {
        let mut cfg = self.config.clone();
        let resp = self.show_with_config(ui, &mut cfg);
        self.config = cfg;
        resp
    }

    /// Renders and updates the terminal using an externally owned mutable [`TerminalConfig`].
    pub fn show_with_config(
        &mut self,
        ui: &mut Ui,
        config: &mut TerminalConfig,
    ) -> TerminalResponse {
        let now_secs = ui.input(|i| i.time);
        if self.terminal.theme() != &config.theme {
            self.terminal.set_theme(config.theme.clone());
        }

        // 1. Auto-layout, pixel snapping, core reflow, and debounced backend resize.
        #[cfg(feature = "search")]
        let (search_blocks_focus, prev_search_rect) = (
            self.search_bar.blocks_terminal_focus(),
            self.search_bar.last_rect,
        );
        #[cfg(not(feature = "search"))]
        let (search_blocks_focus, prev_search_rect) = (false, None);

        let layout = self.layout_state.layout_and_resize(
            ui,
            self.instance_id,
            config,
            &mut self.terminal,
            &mut self.input,
            now_secs,
            &mut self.input_mapper.pending_focus_lock_settle,
            search_blocks_focus,
            prev_search_rect,
        );

        // 2. Bounded non-blocking poll_read -> VT advance -> flush outgoing VT replies.
        let budget = config.read_budget_bytes_per_frame;
        let mut total_read = 0usize;
        if budget > 0 && !self.input.is_closed() {
            while total_read < budget {
                let want = (budget - total_read).min(self.poll_scratch.len());
                match self.input.poll_read(&mut self.poll_scratch[..want]) {
                    Ok(0) => break,
                    Ok(n) => {
                        let n = n.min(want);
                        if n == 0 {
                            break;
                        }
                        total_read += n;
                        self.terminal
                            .advance_with_time(&self.poll_scratch[..n], now_secs);
                    }
                    Err(_) => break,
                }
            }
            if total_read >= budget {
                ui.ctx().request_repaint();
            }
        }
        if total_read == 0 {
            let _ = self.terminal.check_sync_timeout(now_secs);
        }

        let outgoing = self.terminal.drain_outgoing();
        if !outgoing.is_empty() {
            let _ = self.input.write_bytes(&outgoing);
        }

        // 3. Interact with scrollbar overlay before grid event mapping, and isolate search bar
        //    and any open context menu popup.
        #[cfg(feature = "search")]
        let (search_bar_focused, search_bar_actually_focused, search_bar_rect) = {
            let rect = self
                .search_bar
                .active_rect(layout.term_rect, layout.clip_rect);
            if rect.is_none() {
                self.search_bar.clear_hidden_focus();
            }
            (
                self.search_bar.blocks_terminal_focus(),
                self.search_bar.open && self.search_bar.has_focus,
                rect,
            )
        };
        #[cfg(not(feature = "search"))]
        let (search_bar_focused, search_bar_actually_focused, search_bar_rect) =
            (false, false, None);

        let (mouse_protocol, shift_held, secondary_shift_click) = {
            let mouse_protocol = self.terminal.active_snapshot().modes.mouse_protocol;
            let (shift_held, secondary_shift_click) = ui.input(|i| {
                let any_btn_shift = i.events.iter().any(|ev| {
                    matches!(
                        ev,
                        egui::Event::PointerButton { modifiers, .. } if modifiers.shift
                    )
                });
                let sec_shift = i.events.iter().any(|ev| {
                    matches!(
                        ev,
                        egui::Event::PointerButton {
                            button: egui::PointerButton::Secondary,
                            modifiers,
                            ..
                        } if modifiers.shift
                    )
                });
                (i.modifiers.shift || any_btn_shift, sec_shift)
            });
            (mouse_protocol, shift_held, secondary_shift_click)
        };
        let mouse_captured = mouse_protocol != MouseProtocol::None && !shift_held;
        let menu_mouse_captured = mouse_captured && !secondary_shift_click;
        let popup_id = egui::Popup::default_response_id(&layout.response);
        let context_menu_open = egui::Popup::is_id_open(ui.ctx(), popup_id);

        let scrollbar_frame = crate::widgets::scrollbar::interact_scrollbar(
            ui,
            self.instance_id,
            &mut self.terminal,
            &layout,
            search_bar_rect,
            context_menu_open,
        );
        let scrollbar_rect = scrollbar_frame.map(|s| s.track_rect);
        let scrollbar_dragging = scrollbar_frame.is_some_and(|s| s.dragged);
        let scrollbar_secondary_started_in_track = track_secondary_press_in_rect(
            ui,
            scrollbar_rect,
            &mut self.scrollbar_secondary_press_in_track,
            context_menu_open,
        );
        let scrollbar_pointer_active = scrollbar_frame.is_some_and(|s| s.pointer_active)
            || scrollbar_secondary_started_in_track;

        let input_out = self.input_mapper.process_frame_events(
            ui,
            layout.response.id,
            &layout.response,
            layout.term_rect,
            layout.cell_size,
            layout.cols,
            layout.rows,
            now_secs,
            config,
            &mut self.terminal,
            search_bar_focused,
            search_bar_actually_focused,
            search_bar_rect,
            scrollbar_rect,
            scrollbar_dragging,
            scrollbar_pointer_active,
            context_menu_open,
        );
        if input_out.any_user_activity {
            self.renderer.note_input_activity(now_secs);
        }
        if !input_out.bytes_to_write.is_empty() {
            let _ = self.input.write_bytes(&input_out.bytes_to_write);
        }
        #[cfg(feature = "search")]
        if input_out.open_search_requested {
            self.search_bar.open_from_action();
        }

        let secondary_press_started_in_search = track_secondary_press_in_rect(
            ui,
            search_bar_rect,
            &mut self.secondary_press_in_search_bar,
            false,
        );

        let pointer_over_search = secondary_press_started_in_search
            || search_bar_rect.is_some_and(|bar_rect| {
                let latest_in_bar = ui
                    .input(|i| i.pointer.latest_pos())
                    .is_some_and(|p| bar_rect.contains(p));
                let interact_in_bar = layout
                    .response
                    .interact_pointer_pos()
                    .is_some_and(|p| bar_rect.contains(p));
                let press_origin_in_bar = ui
                    .input(|i| i.pointer.press_origin())
                    .is_some_and(|p| bar_rect.contains(p));
                let secondary_event_in_bar = ui.input(|i| {
                    i.events.iter().any(|ev| {
                        matches!(
                            ev,
                            egui::Event::PointerButton {
                                button: egui::PointerButton::Secondary,
                                pos,
                                ..
                            } if bar_rect.contains(*pos)
                        )
                    })
                });
                latest_in_bar || interact_in_bar || press_origin_in_bar || secondary_event_in_bar
            });

        // 4. Optional hyperlink hover & activation.
        #[cfg(feature = "hyperlinks")]
        let hovered_spans = {
            let pointer_pos = ui.input(|i| i.pointer.hover_pos()).filter(|p| {
                !context_menu_open
                    && !scrollbar_dragging
                    && !scrollbar_pointer_active
                    && !search_bar_rect.is_some_and(|r| r.contains(*p))
                    && !scrollbar_rect.is_some_and(|r| r.contains(*p))
            });
            crate::widgets::hyperlink::update_hyperlink_interaction(
                ui,
                &mut self.terminal,
                pointer_pos,
                &layout,
                !mouse_captured,
                input_out.clean_primary_click && !mouse_captured,
            )
        };

        // 5. Optional context menu (keep open once opened via Shift+RightClick even after Shift release).
        let allow_menu = context_menu_open
            || (!menu_mouse_captured && !pointer_over_search && !scrollbar_pointer_active);
        if let Some(action) = crate::widgets::context_menu::attach_context_menu(
            &layout.response,
            &self.terminal,
            allow_menu,
        ) {
            match action {
                WidgetAction::CopySelection => {
                    if let Some(sel) = self.terminal.selection_text().filter(|s| !s.is_empty()) {
                        copy_text_everywhere(ui, sel);
                    }
                }
                WidgetAction::PasteClipboard => {
                    if let Some(text) = try_read_native_clipboard() {
                        let formatted = self.terminal.format_paste(&text);
                        if !formatted.is_empty() {
                            let _ = self.input.write_bytes(&formatted);
                        }
                    } else {
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                    }
                }
                WidgetAction::ClearScrollback => {
                    self.terminal.advance(b"\x1b[3J\x1b[2J\x1b[H");
                }
                #[cfg(feature = "search")]
                WidgetAction::OpenSearchBar => {
                    self.search_bar.open_from_action();
                }
                #[cfg(feature = "shell-integration")]
                WidgetAction::JumpPrompt(delta) => {
                    let _ = self.terminal.jump_to_prompt(delta);
                }
            }
        }

        // 6. 4-stage cached rendering.
        let has_focus = layout.response.has_focus();
        let ime_cursor_rect = self.renderer.render(
            ui,
            &layout,
            config,
            &mut self.terminal,
            now_secs,
            has_focus,
            &self.input_mapper.ime_preedit,
            #[cfg(feature = "hyperlinks")]
            &hovered_spans,
        );

        // 7. Overlaid scrollbar & optional floating search bar.
        let default_fg = {
            let rgb = config.theme.scheme.foreground;
            egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2])
        };
        let overlay_painter = ui.painter_at(layout.clip_rect);
        if let Some(sb) = scrollbar_frame {
            crate::widgets::scrollbar::paint_scrollbar(&overlay_painter, sb, default_fg);
        }

        #[cfg(feature = "search")]
        if self.search_bar.open
            && crate::widgets::search_bar::show_search_bar(
                ui,
                self.instance_id,
                layout.term_rect,
                layout.clip_rect,
                &mut self.search_bar,
                &mut self.terminal,
            )
        {
            layout.response.request_focus();
            self.input_mapper.note_focus_requested();
        }

        // 8. Sync final DEC ?1004 focus state (after search bar close/open) and report IME area.
        #[cfg(feature = "search")]
        let final_search_bar_focused = self.search_bar.blocks_terminal_focus();
        #[cfg(not(feature = "search"))]
        let final_search_bar_focused = false;
        let final_has_focus = layout.response.has_focus() && !final_search_bar_focused;
        let mut focus_bytes = Vec::new();
        self.input_mapper.sync_focus_transition(
            &mut self.terminal,
            final_has_focus,
            &mut focus_bytes,
        );
        if !focus_bytes.is_empty() {
            let _ = self.input.write_bytes(&focus_bytes);
        }
        if !final_search_bar_focused && ui.memory(|m| m.owns_ime_events(layout.response.id)) {
            let to_global = ui
                .ctx()
                .layer_transform_to_global(ui.layer_id())
                .unwrap_or_default();
            ui.ctx().output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Normal,
                    rect: to_global * layout.term_rect,
                    cursor_rect: to_global * ime_cursor_rect,
                    should_interrupt_composition: false,
                });
            });
        }

        let events = self.terminal.drain_events();

        TerminalResponse {
            response: layout.response,
            cols: layout.cols,
            rows: layout.rows,
            cell_size: layout.cell_size,
            effective_font_size: layout.effective_font_size,
            resized_this_frame: layout.resized_this_frame,
            events,
        }
    }

    /// Returns a [`TerminalWidget`] adapter implementing [`egui::Widget`].
    pub fn widget(&mut self) -> TerminalWidget<'_, I> {
        TerminalWidget::new(self)
    }

    /// Returns a shared reference to the underlying [`Terminal`] state machine.
    #[must_use]
    pub const fn terminal(&self) -> &Terminal {
        &self.terminal
    }

    /// Returns a mutable reference to the underlying [`Terminal`] state machine.
    pub fn terminal_mut(&mut self) -> &mut Terminal {
        &mut self.terminal
    }

    /// Returns a shared reference to the backing [`TerminalInput`].
    #[must_use]
    pub const fn input(&self) -> &I {
        &self.input
    }

    /// Returns a mutable reference to the backing [`TerminalInput`].
    pub fn input_mut(&mut self) -> &mut I {
        &mut self.input
    }

    /// Returns a shared reference to the internal [`TerminalConfig`].
    #[must_use]
    pub const fn config(&self) -> &TerminalConfig {
        &self.config
    }

    /// Returns a mutable reference to the internal [`TerminalConfig`].
    pub fn config_mut(&mut self) -> &mut TerminalConfig {
        &mut self.config
    }

    /// Opens the floating search bar overlay (`search` feature).
    #[cfg(feature = "search")]
    pub fn open_search(&mut self) {
        self.search_bar.open();
    }

    /// Closes the floating search bar overlay (`search` feature).
    #[cfg(feature = "search")]
    pub fn close_search(&mut self) {
        self.search_bar.close();
    }

    /// Returns mutable access to [`crate::widgets::search_bar::SearchBarState`] (`search` feature).
    #[cfg(feature = "search")]
    pub fn search_bar_mut(&mut self) -> &mut crate::widgets::search_bar::SearchBarState {
        &mut self.search_bar
    }
}

/// [`egui::Widget`] adapter wrapping a mutable [`TerminalRuntime`] (and optional external
/// [`TerminalConfig`]) for ergonomic `ui.add(TerminalWidget::new(&mut runtime))` usage.
pub struct TerminalWidget<'a, I: TerminalInput> {
    runtime: &'a mut TerminalRuntime<I>,
    external_config: Option<&'a mut TerminalConfig>,
}

impl<'a, I: TerminalInput> TerminalWidget<'a, I> {
    /// Creates a widget borrowing `runtime` and using `runtime`'s internal configuration.
    pub fn new(runtime: &'a mut TerminalRuntime<I>) -> Self {
        Self {
            runtime,
            external_config: None,
        }
    }

    /// Overrides the configuration with an external mutable [`TerminalConfig`].
    #[must_use]
    pub fn with_config(mut self, config: &'a mut TerminalConfig) -> Self {
        self.external_config = Some(config);
        self
    }

    /// Shows the widget and returns the rich [`TerminalResponse`].
    pub fn show(self, ui: &mut Ui) -> TerminalResponse {
        match self.external_config {
            Some(cfg) => self.runtime.show_with_config(ui, cfg),
            None => self.runtime.show(ui),
        }
    }
}

impl<I: TerminalInput> Widget for TerminalWidget<'_, I> {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        self.show(ui).response
    }
}

fn track_secondary_press_in_rect(
    ui: &Ui,
    rect: Option<egui::Rect>,
    latched: &mut bool,
    suppress_latch: bool,
) -> bool {
    let mut started_in_rect = false;
    let mut saw_secondary_event = false;
    ui.input(|i| {
        for ev in &i.events {
            if let egui::Event::PointerButton {
                button: egui::PointerButton::Secondary,
                pressed,
                pos,
                ..
            } = ev
            {
                saw_secondary_event = true;
                if *pressed {
                    *latched = !suppress_latch && rect.is_some_and(|r| r.contains(*pos));
                    if *latched {
                        started_in_rect = true;
                    }
                } else {
                    if *latched {
                        started_in_rect = true;
                    }
                    *latched = false;
                }
            }
        }
    });
    if !saw_secondary_event && *latched && ui.input(|i| !i.pointer.secondary_down()) {
        *latched = false;
    }
    started_in_rect
}
