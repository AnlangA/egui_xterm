//! Translation of `egui` keyboard, text, IME, clipboard, focus, and pointer events into
//! terminal actions and VT byte sequences.

use egui::{
    Event, EventFilter, ImeEvent, Key, Modifiers, MouseWheelUnit, OutputCommand, PointerButton,
    Pos2, Rect, Ui, Vec2,
};
use egui_xterm_core::{
    MouseButton, MouseEventKind, MouseInputEvent, MouseModifiers, MouseProtocol, SelectionKind,
    Terminal,
};

use crate::config::{CopyBehavior, TerminalConfig};
use crate::renderer::ImePreeditState;

pub(crate) const TERMINAL_FOCUS_FILTER: EventFilter = EventFilter {
    tab: true,
    horizontal_arrows: true,
    vertical_arrows: true,
    escape: true,
};

/// Persistent per-runtime input, focus, IME, and pointer state.
#[derive(Debug, Default)]
pub(crate) struct InputMapperState {
    was_focused: bool,
    last_reported_focus: Option<bool>,
    pub pending_focus_lock_settle: bool,
    pub ime_preedit: ImePreeditState,
    scrollbar_press_active: bool,
    left_down_in_grid: bool,
    press_pos: Option<Pos2>,
    drag_started: bool,
    click_count: u8,
    last_click_time: f64,
    last_click_pos: Option<Pos2>,
    active_mouse_button: Option<MouseButton>,
    pressed_mouse_buttons: u8,
    last_reported_cell: Option<(u16, u16)>,
    last_pointer_pos: Option<Pos2>,
}

/// Output produced by [`InputMapperState::process_frame_events`].
pub(crate) struct InputFrameOutcome {
    pub bytes_to_write: Vec<u8>,
    #[cfg_attr(not(feature = "hyperlinks"), allow(dead_code))]
    pub clean_primary_click: bool,
    pub any_user_activity: bool,
    #[cfg(feature = "search")]
    pub open_search_requested: bool,
}

impl InputMapperState {
    /// Marks that focus was requested on this frame so the next frame guards immediate `Escape`/`Tab`.
    #[cfg(feature = "search")]
    pub(crate) fn note_focus_requested(&mut self) {
        self.pending_focus_lock_settle = true;
    }

    /// Emits `DEC ?1004` focus reporting strictly on focus transitions and clears IME preedit on blur.
    pub(crate) fn sync_focus_transition(
        &mut self,
        terminal: &mut Terminal,
        has_focus: bool,
        out_bytes: &mut Vec<u8>,
    ) {
        if has_focus != self.was_focused {
            self.was_focused = has_focus;
            if self.last_reported_focus != Some(has_focus) {
                if let Some(bytes) = terminal.focus_report(has_focus) {
                    out_bytes.extend_from_slice(bytes);
                }
                self.last_reported_focus = Some(has_focus);
            }
            if !has_focus {
                self.ime_preedit.text.clear();
                self.ime_preedit.active_range_chars = None;
            }
        }
    }

    /// Applies focus lock, processes pointer/mouse/keyboard/IME/clipboard events, and updates
    /// selection and IME preedit state.
    #[expect(clippy::too_many_arguments, reason = "per-frame input coordinator")]
    pub(crate) fn process_frame_events(
        &mut self,
        ui: &Ui,
        id: egui::Id,
        response: &egui::Response,
        term_rect: Rect,
        cell_size: Vec2,
        cols: u16,
        rows: u16,
        now_secs: f64,
        config: &TerminalConfig,
        terminal: &mut Terminal,
        search_bar_focused: bool,
        search_bar_actually_focused: bool,
        search_bar_rect: Option<Rect>,
        scrollbar_rect: Option<Rect>,
        scrollbar_dragging: bool,
        scrollbar_pointer_active: bool,
        context_menu_open: bool,
    ) -> InputFrameOutcome {
        let mut out = InputFrameOutcome {
            bytes_to_write: Vec::new(),
            clean_primary_click: false,
            any_user_activity: false,
            #[cfg(feature = "search")]
            open_search_requested: false,
        };

        let raw_events = ui.input(|i| i.events.clone());
        let interact_rect = response.rect;
        let in_interact_grid =
            |pos: Pos2| interact_rect.is_positive() && interact_rect.contains(pos);
        let pointer_in_overlay = |pos: Pos2| {
            search_bar_rect.is_some_and(|r| r.contains(pos))
                || scrollbar_rect.is_some_and(|r| r.contains(pos))
        };

        let mouse_protocol = terminal.active_snapshot().modes.mouse_protocol;

        // Acquire focus on click or initial pointer press/drag inside the terminal grid
        // (excluding overlay widgets like the search bar, scrollbar, and open context menu).
        let primary_pressed_in_grid = !context_menu_open
            && raw_events.iter().any(|ev| {
                matches!(
                    ev,
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        ..
                    } if in_interact_grid(*pos) && !pointer_in_overlay(*pos)
                )
            });
        let mouse_protocol_press_in_grid = !context_menu_open
            && mouse_protocol != MouseProtocol::None
            && raw_events.iter().any(|ev| {
                matches!(
                    ev,
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Secondary | PointerButton::Middle,
                        pressed: true,
                        modifiers,
                    } if !modifiers.shift && in_interact_grid(*pos) && !pointer_in_overlay(*pos)
                )
            });
        let any_pressed_in_grid = !context_menu_open
            && raw_events.iter().any(|ev| {
                matches!(
                    ev,
                    Event::PointerButton {
                        pos,
                        pressed: true,
                        ..
                    } if in_interact_grid(*pos) && !pointer_in_overlay(*pos)
                )
            });
        let interact_in_grid = !context_menu_open
            && response
                .interact_pointer_pos()
                .is_some_and(|p| in_interact_grid(p) && !pointer_in_overlay(p));
        let clicked_in_grid = response.clicked() && interact_in_grid;
        let drag_started_in_grid =
            response.drag_started() && !scrollbar_dragging && interact_in_grid;

        let non_primary_in_search_bar = !context_menu_open
            && !search_bar_focused
            && ui.memory(|m| m.focused().is_none() && m.had_focus_last_frame(id))
            && search_bar_rect.is_some_and(|bar_rect| {
                let mut non_primary_in_bar = false;
                let mut primary_or_outside = false;
                for ev in &raw_events {
                    if let Event::PointerButton { pos, button, .. } = ev {
                        if *button != PointerButton::Primary && bar_rect.contains(*pos) {
                            non_primary_in_bar = true;
                        } else {
                            primary_or_outside = true;
                        }
                    }
                }
                let held_non_primary_in_bar = ui.input(|i| {
                    !i.pointer.primary_down()
                        && i.pointer.any_down()
                        && i.pointer.latest_pos().is_some_and(|p| bar_rect.contains(p))
                });
                (non_primary_in_bar || held_non_primary_in_bar) && !primary_or_outside
            });

        let in_own_scrollbar = if !context_menu_open
            && !search_bar_focused
            && ui.memory(|m| m.focused().is_none() && m.had_focus_last_frame(id))
        {
            if let Some(sb_rect) = scrollbar_rect {
                let in_sb = |p: Pos2| {
                    sb_rect.contains(p) && !search_bar_rect.is_some_and(|r| r.contains(p))
                };
                let mut origin_in_sb = self.scrollbar_press_active
                    || ui.input(|i| i.pointer.press_origin()).is_some_and(in_sb);
                let mut btn_in_sb = false;
                let mut pressed_outside_sb = false;
                for ev in &raw_events {
                    if let Event::PointerButton { pos, pressed, .. } = ev {
                        if *pressed {
                            origin_in_sb = in_sb(*pos);
                            if origin_in_sb {
                                btn_in_sb = true;
                            } else {
                                pressed_outside_sb = true;
                            }
                        } else {
                            if in_sb(*pos) {
                                btn_in_sb = true;
                            } else if !origin_in_sb {
                                pressed_outside_sb = true;
                            }
                            origin_in_sb = false;
                        }
                    }
                }
                let held_in_sb = scrollbar_dragging
                    || self.scrollbar_press_active
                    || (scrollbar_pointer_active
                        && ui.input(|i| i.pointer.press_origin()).is_some_and(in_sb));
                (btn_in_sb || held_in_sb) && !pressed_outside_sb
            } else if scrollbar_pointer_active {
                !raw_events.iter().any(|ev| {
                    matches!(
                        ev,
                        Event::PointerButton {
                            pos,
                            pressed: true,
                            ..
                        } if !in_interact_grid(*pos)
                    )
                })
            } else {
                false
            }
        } else {
            false
        };

        if let Some(sb_rect) = scrollbar_rect {
            let in_sb =
                |p: Pos2| sb_rect.contains(p) && !search_bar_rect.is_some_and(|r| r.contains(p));
            let mut saw_button_ev = false;
            for ev in &raw_events {
                if let Event::PointerButton { pos, pressed, .. } = ev {
                    saw_button_ev = true;
                    self.scrollbar_press_active = *pressed && in_sb(*pos);
                }
            }
            if !saw_button_ev && !ui.input(|i| i.pointer.any_down()) {
                self.scrollbar_press_active = false;
            }
        } else if !scrollbar_pointer_active || !ui.input(|i| i.pointer.any_down()) {
            self.scrollbar_press_active = false;
        }

        if primary_pressed_in_grid
            || mouse_protocol_press_in_grid
            || non_primary_in_search_bar
            || in_own_scrollbar
            || (!search_bar_focused && (clicked_in_grid || drag_started_in_grid))
        {
            response.request_focus();
        }

        let effective_search_bar_focused = search_bar_focused
            && !primary_pressed_in_grid
            && !mouse_protocol_press_in_grid
            && (search_bar_actually_focused || !any_pressed_in_grid);
        let has_focus = ui.memory(|m| m.has_focus(id)) && !effective_search_bar_focused;
        let os_has_focus = response.has_focus() && !effective_search_bar_focused;

        // Lock Tab, arrow keys, and Escape while the terminal has focus.
        let filter = TERMINAL_FOCUS_FILTER;
        if has_focus {
            ui.memory_mut(|m| m.set_focus_lock_filter(id, filter));
            self.pending_focus_lock_settle = !ui.memory(|m| m.had_focus_last_frame(id));
        } else {
            self.pending_focus_lock_settle = false;
        }

        // Emit DEC ?1004 focus reporting strictly on focus transitions.
        // When a visible search bar overlay is open, defer focus-out transitions until after
        // Stage 7 so closing the bar via its "x" button keeps terminal focus without a spurious
        // `\x1b[O` -> `\x1b[I` bounce.
        if os_has_focus || search_bar_rect.is_none() {
            self.sync_focus_transition(terminal, os_has_focus, &mut out.bytes_to_write);
        }

        // Process pointer / mouse / wheel events first.
        if mouse_protocol == MouseProtocol::None {
            self.active_mouse_button = None;
            self.pressed_mouse_buttons = 0;
            self.last_reported_cell = None;
        }

        // Reconcile latched pointer-button state if a release event was missed (e.g. released
        // outside the window or while this widget was hidden).
        let mut saw_primary_release = false;
        let mut saw_middle_release = false;
        let mut saw_secondary_release = false;
        for ev in &raw_events {
            if let Event::PointerButton {
                button,
                pressed: false,
                ..
            } = ev
            {
                match button {
                    PointerButton::Primary | PointerButton::Extra1 | PointerButton::Extra2 => {
                        saw_primary_release = true;
                    }
                    PointerButton::Middle => saw_middle_release = true,
                    PointerButton::Secondary => saw_secondary_release = true,
                }
            }
        }
        let (primary_down, middle_down, secondary_down) = ui.input(|i| {
            (
                i.pointer.button_down(PointerButton::Primary),
                i.pointer.button_down(PointerButton::Middle),
                i.pointer.button_down(PointerButton::Secondary),
            )
        });
        if self.left_down_in_grid && !saw_primary_release && !primary_down {
            self.left_down_in_grid = false;
            self.pressed_mouse_buttons &= !mouse_button_bit(MouseButton::Left);
            if self.drag_started {
                terminal.selection_finish();
            } else {
                terminal.selection_clear();
            }
            self.drag_started = false;
            self.press_pos = None;
        }
        if !saw_primary_release && !primary_down {
            self.pressed_mouse_buttons &= !mouse_button_bit(MouseButton::Left);
        }
        if !saw_middle_release && !middle_down {
            self.pressed_mouse_buttons &= !mouse_button_bit(MouseButton::Middle);
        }
        if !saw_secondary_release && !secondary_down {
            self.pressed_mouse_buttons &= !mouse_button_bit(MouseButton::Right);
        }
        if let Some(active_btn) = self.active_mouse_button
            && (self.pressed_mouse_buttons & mouse_button_bit(active_btn)) == 0
        {
            self.active_mouse_button = first_pressed_button(self.pressed_mouse_buttons);
        }
        if !raw_events.is_empty() {
            let frame_shift_held = ui.input(|i| i.modifiers.shift)
                || raw_events.iter().any(|ev| {
                    matches!(
                        ev,
                        Event::PointerButton { modifiers, .. } | Event::MouseWheel { modifiers, .. }
                            if modifiers.shift
                    )
                });
            for ev in &raw_events {
                match ev {
                    Event::PointerMoved(pos) => {
                        self.last_pointer_pos = Some(*pos);
                        if self.left_down_in_grid {
                            if self.press_pos.is_some_and(|p| p.distance(*pos) > 2.0) {
                                self.drag_started = true;
                            }
                            if let Some((col, row)) =
                                clamp_pos_to_cell(*pos, term_rect, cell_size, cols, rows)
                            {
                                terminal.selection_update(usize::from(row), usize::from(col));
                            }
                        } else if !context_menu_open
                            && !scrollbar_dragging
                            && !pointer_in_overlay(*pos)
                            && mouse_protocol != MouseProtocol::None
                            && !frame_shift_held
                            && (in_interact_grid(*pos) || self.active_mouse_button.is_some())
                        {
                            let mods = ui.input(|i| i.modifiers);
                            let cell = if in_interact_grid(*pos) {
                                pos_to_cell(*pos, term_rect, cell_size, cols, rows)
                            } else {
                                clamp_pos_to_cell(*pos, term_rect, cell_size, cols, rows)
                            };
                            if let Some((col, row)) = cell
                                && self.last_reported_cell != Some((col, row))
                            {
                                self.last_reported_cell = Some((col, row));
                                let mev = MouseInputEvent {
                                    kind: MouseEventKind::Motion,
                                    button: self.active_mouse_button,
                                    col,
                                    row,
                                    modifiers: to_mouse_mods(mods),
                                };
                                if let Some(bytes) = terminal.encode_mouse_event(mev) {
                                    out.bytes_to_write.extend_from_slice(&bytes);
                                }
                            }
                        }
                    }
                    Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers,
                    } => {
                        self.last_pointer_pos = Some(*pos);
                        let in_bounds = in_interact_grid(*pos);
                        let over_overlay = pointer_in_overlay(*pos);
                        let bypass_mouse_tracking = modifiers.shift;

                        if !*pressed {
                            let m_btn = to_mouse_button(*button);
                            let m_bit = mouse_button_bit(m_btn);
                            if *button == PointerButton::Primary && self.left_down_in_grid {
                                self.left_down_in_grid = false;
                                self.pressed_mouse_buttons &= !m_bit;
                                if self.drag_started {
                                    if let Some((col, row)) =
                                        clamp_pos_to_cell(*pos, term_rect, cell_size, cols, rows)
                                    {
                                        terminal
                                            .selection_update(usize::from(row), usize::from(col));
                                    }
                                    terminal.selection_finish();
                                    if let Some(sel) =
                                        terminal.selection_text().filter(|s| !s.is_empty())
                                    {
                                        ui.ctx().send_cmd(OutputCommand::TextSelectionSettled(sel));
                                    }
                                } else if self.click_count == 1 && !context_menu_open {
                                    terminal.selection_clear();
                                    if in_bounds && !over_overlay && !scrollbar_pointer_active {
                                        out.clean_primary_click = true;
                                    }
                                }
                                continue;
                            }
                            if self.active_mouse_button == Some(m_btn)
                                || (self.pressed_mouse_buttons & m_bit) != 0
                            {
                                self.pressed_mouse_buttons &= !m_bit;
                                if self.active_mouse_button == Some(m_btn) {
                                    self.active_mouse_button =
                                        first_pressed_button(self.pressed_mouse_buttons);
                                }
                                if mouse_protocol != MouseProtocol::None {
                                    let cell = if in_bounds {
                                        pos_to_cell(*pos, term_rect, cell_size, cols, rows)
                                    } else {
                                        clamp_pos_to_cell(*pos, term_rect, cell_size, cols, rows)
                                    };
                                    if let Some((col, row)) = cell {
                                        let mev = MouseInputEvent {
                                            kind: MouseEventKind::Release,
                                            button: Some(m_btn),
                                            col,
                                            row,
                                            modifiers: to_mouse_mods(*modifiers),
                                        };
                                        if let Some(bytes) = terminal.encode_mouse_event(mev) {
                                            out.bytes_to_write.extend_from_slice(&bytes);
                                        }
                                    }
                                }
                            }
                            continue;
                        }

                        if !in_bounds
                            || over_overlay
                            || scrollbar_dragging
                            || scrollbar_pointer_active
                            || context_menu_open
                        {
                            continue;
                        }

                        if mouse_protocol != MouseProtocol::None && !bypass_mouse_tracking {
                            if let Some((col, row)) =
                                pos_to_cell(*pos, term_rect, cell_size, cols, rows)
                            {
                                let m_btn = to_mouse_button(*button);
                                self.active_mouse_button = Some(m_btn);
                                self.pressed_mouse_buttons |= mouse_button_bit(m_btn);
                                self.last_reported_cell = Some((col, row));
                                let mev = MouseInputEvent {
                                    kind: MouseEventKind::Press,
                                    button: Some(m_btn),
                                    col,
                                    row,
                                    modifiers: to_mouse_mods(*modifiers),
                                };
                                if let Some(bytes) = terminal.encode_mouse_event(mev) {
                                    out.bytes_to_write.extend_from_slice(&bytes);
                                }
                            }
                        } else if *button == PointerButton::Primary
                            && let Some((col, row)) =
                                pos_to_cell(*pos, term_rect, cell_size, cols, rows)
                        {
                            self.left_down_in_grid = true;
                            self.press_pos = Some(*pos);
                            self.drag_started = false;

                            let rapid = (now_secs - self.last_click_time) <= 0.4
                                && self.last_click_pos.is_some_and(|p| p.distance(*pos) <= 4.0);
                            self.click_count = if rapid { (self.click_count % 3) + 1 } else { 1 };
                            self.last_click_time = now_secs;
                            self.last_click_pos = Some(*pos);

                            match self.click_count {
                                2 => terminal.selection_word(usize::from(row), usize::from(col)),
                                3 => terminal.selection_line(usize::from(row), usize::from(col)),
                                _ if modifiers.alt => {
                                    terminal.selection_block(usize::from(row), usize::from(col));
                                }
                                _ => terminal.selection_start(
                                    usize::from(row),
                                    usize::from(col),
                                    SelectionKind::Stream,
                                ),
                            }
                        }
                    }
                    Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } => {
                        if context_menu_open {
                            continue;
                        }
                        let hover_pos = ui
                            .input(|i| i.pointer.hover_pos())
                            .or(self.last_pointer_pos);
                        if hover_pos.is_some_and(|p| search_bar_rect.is_some_and(|r| r.contains(p)))
                        {
                            continue;
                        }
                        let pointer_inside = hover_pos.is_some_and(in_interact_grid);
                        if !pointer_inside && !(has_focus && hover_pos.is_none()) {
                            continue;
                        }
                        let (col, row) = hover_pos
                            .and_then(|p| pos_to_cell(p, term_rect, cell_size, cols, rows))
                            .unwrap_or((0, 0));

                        let steps_y = match unit {
                            MouseWheelUnit::Line => delta.y.abs().round().max(1.0) as usize,
                            MouseWheelUnit::Point => {
                                (delta.y.abs() / cell_size.y.max(1.0)).round().max(1.0) as usize
                            }
                            MouseWheelUnit::Page => usize::from(rows.max(1)),
                        };

                        if (mouse_protocol != MouseProtocol::None && !modifiers.shift)
                            || (terminal.is_alt_screen()
                                && terminal.active_snapshot().modes.alt_scroll)
                        {
                            if delta.y.abs() > f32::EPSILON {
                                let btn = if delta.y > 0.0 {
                                    MouseButton::WheelUp
                                } else {
                                    MouseButton::WheelDown
                                };
                                for _ in 0..steps_y.min(16) {
                                    let mev = MouseInputEvent {
                                        kind: MouseEventKind::Press,
                                        button: Some(btn),
                                        col,
                                        row,
                                        modifiers: to_mouse_mods(*modifiers),
                                    };
                                    if let Some(bytes) = terminal.encode_mouse_event(mev) {
                                        out.bytes_to_write.extend_from_slice(&bytes);
                                    }
                                }
                            } else if delta.x.abs() > f32::EPSILON
                                && mouse_protocol != MouseProtocol::None
                            {
                                let btn = if delta.x > 0.0 {
                                    MouseButton::WheelLeft
                                } else {
                                    MouseButton::WheelRight
                                };
                                let mev = MouseInputEvent {
                                    kind: MouseEventKind::Press,
                                    button: Some(btn),
                                    col,
                                    row,
                                    modifiers: to_mouse_mods(*modifiers),
                                };
                                if let Some(bytes) = terminal.encode_mouse_event(mev) {
                                    out.bytes_to_write.extend_from_slice(&bytes);
                                }
                            }
                        } else if delta.y.abs() > f32::EPSILON {
                            let signed_lines = if delta.y > 0.0 {
                                steps_y as i32
                            } else {
                                -(steps_y as i32)
                            };
                            terminal.scroll_display(signed_lines);
                        }
                    }
                    Event::WindowFocused(focused)
                        if has_focus && self.last_reported_focus != Some(*focused) =>
                    {
                        if let Some(bytes) = terminal.focus_report(*focused) {
                            out.bytes_to_write.extend_from_slice(bytes);
                        }
                        self.last_reported_focus = Some(*focused);
                    }
                    _ => {}
                }
            }
        }

        if !has_focus || raw_events.is_empty() || context_menu_open {
            return out;
        }

        let is_mac = ui.ctx().os().is_mac();
        let filtered_events = ui.input(|i| i.filtered_events(&filter));
        let mouse_protocol = terminal.active_snapshot().modes.mouse_protocol;
        let shift_held = ui.input(|i| i.modifiers.shift);
        let mut skip_next_text = false;

        for (idx, ev) in filtered_events.iter().enumerate() {
            match ev {
                Event::Copy => {
                    out.any_user_activity = true;
                    #[cfg(feature = "kitty-keyboard")]
                    let kitty_handled = if !is_mac && !terminal.kitty_keyboard_flags().is_empty() {
                        let has_sel = terminal.selection_text().is_some_and(|s| !s.is_empty());
                        let should_send_key = match config.effective_copy_behavior() {
                            CopyBehavior::AlwaysSendEtx => true,
                            CopyBehavior::CopyIfSelectionElseSendEtx => !has_sel,
                            CopyBehavior::AlwaysCopy => false,
                        };
                        if should_send_key {
                            let flags = terminal.kitty_keyboard_flags();
                            let modes = *terminal.active_snapshot().modes;
                            if let Some(encoded) = encode_egui_key_kitty(
                                flags,
                                &modes,
                                Key::C,
                                true,
                                false,
                                Modifiers::CTRL,
                                None,
                            ) {
                                out.bytes_to_write.extend_from_slice(&encoded);
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    #[cfg(not(feature = "kitty-keyboard"))]
                    let kitty_handled = false;

                    if !kitty_handled {
                        handle_copy_shortcut(
                            ui,
                            config,
                            terminal,
                            is_mac,
                            true,
                            &mut out.bytes_to_write,
                        );
                    }
                }
                Event::Cut => {
                    out.any_user_activity = true;
                    if !is_mac {
                        out.bytes_to_write.push(0x18);
                    }
                }
                Event::Paste(text) => {
                    out.any_user_activity = true;
                    let formatted = terminal.format_paste(text);
                    out.bytes_to_write.extend_from_slice(&formatted);
                }
                Event::MiddleClickPaste { text, pos } => {
                    let allow_middle_paste = mouse_protocol == MouseProtocol::None || shift_held;
                    if allow_middle_paste && in_interact_grid(*pos) && !pointer_in_overlay(*pos) {
                        out.any_user_activity = true;
                        let formatted = terminal.format_paste(text);
                        out.bytes_to_write.extend_from_slice(&formatted);
                    }
                }
                Event::Ime(ime_ev) => {
                    out.any_user_activity = true;
                    match ime_ev {
                        ImeEvent::Preedit {
                            text,
                            active_range_chars,
                        } => {
                            if text.is_empty() {
                                self.ime_preedit.text.clear();
                                self.ime_preedit.active_range_chars = None;
                            } else {
                                self.ime_preedit.text.clone_from(text);
                                self.ime_preedit.active_range_chars = active_range_chars.clone();
                            }
                        }
                        ImeEvent::Commit(text) => {
                            self.ime_preedit.text.clear();
                            self.ime_preedit.active_range_chars = None;
                            if !text.is_empty() {
                                out.bytes_to_write.extend_from_slice(text.as_bytes());
                            }
                        }
                        ImeEvent::DeleteSurrounding { .. } => {}
                        _ => {}
                    }
                }
                Event::Key {
                    key,
                    pressed,
                    repeat,
                    modifiers,
                    ..
                } => {
                    #[cfg(not(feature = "kitty-keyboard"))]
                    let _ = repeat;
                    // Check for an immediately following Event::Text so Kitty Flag 16 and legacy
                    // shortcut suppression stay synchronized without double-sending.
                    let paired_text = if *pressed {
                        match filtered_events.get(idx + 1) {
                            Some(Event::Text(txt)) => Some(txt.as_str()),
                            _ => None,
                        }
                    } else {
                        None
                    };

                    if *pressed && *key == Key::F && modifiers.ctrl && modifiers.shift {
                        #[cfg(feature = "search")]
                        {
                            out.open_search_requested = true;
                            skip_next_text = paired_text.is_some();
                            continue;
                        }
                    }

                    #[cfg(feature = "kitty-keyboard")]
                    let kitty_active = !terminal.kitty_keyboard_flags().is_empty();
                    #[cfg(not(feature = "kitty-keyboard"))]
                    let kitty_active = false;

                    let has_non_empty_selection =
                        terminal.selection_text().is_some_and(|s| !s.is_empty());

                    // Handle direct Key::C + CTRL when not already translated into Event::Copy.
                    if *pressed
                        && *key == Key::C
                        && modifiers.ctrl
                        && !modifiers.shift
                        && !modifiers.alt
                        && !modifiers.mac_cmd
                        && (!kitty_active
                            || (has_non_empty_selection
                                && config.effective_copy_behavior() != CopyBehavior::AlwaysSendEtx))
                    {
                        out.any_user_activity = true;
                        handle_copy_shortcut(
                            ui,
                            config,
                            terminal,
                            false,
                            false,
                            &mut out.bytes_to_write,
                        );
                        skip_next_text = paired_text.is_some();
                        continue;
                    }

                    // Handle Ctrl+Shift+C / Ctrl+Shift+V when Kitty keyboard isn't intercepting them.
                    if *pressed
                        && !kitty_active
                        && modifiers.ctrl
                        && modifiers.shift
                        && !modifiers.alt
                    {
                        if *key == Key::C {
                            if let Some(sel) = terminal.selection_text().filter(|s| !s.is_empty()) {
                                copy_text_everywhere(ui, sel);
                            }
                            skip_next_text = paired_text.is_some();
                            continue;
                        }
                        if *key == Key::V {
                            if let Some(text) = try_read_native_clipboard() {
                                let formatted = terminal.format_paste(&text);
                                out.bytes_to_write.extend_from_slice(&formatted);
                            } else {
                                ui.ctx()
                                    .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                            }
                            skip_next_text = paired_text.is_some();
                            continue;
                        }
                    }

                    #[cfg(feature = "kitty-keyboard")]
                    if kitty_active {
                        let flags = terminal.kitty_keyboard_flags();
                        let modes = *terminal.active_snapshot().modes;
                        if let Some(encoded) = encode_egui_key_kitty(
                            flags,
                            &modes,
                            *key,
                            *pressed,
                            *repeat,
                            *modifiers,
                            paired_text,
                        ) {
                            out.any_user_activity = true;
                            out.bytes_to_write.extend_from_slice(&encoded);
                            skip_next_text = paired_text.is_some();
                            continue;
                        } else if !*pressed
                            || (paired_text.is_some()
                                && !modifiers.ctrl
                                && !modifiers.alt
                                && !modifiers.mac_cmd)
                        {
                            // Suppressed key release, or let Event::Text handle printable char.
                            continue;
                        }
                        // Fall through to legacy encoding when Kitty returns `None` on press
                        // (e.g., Flag 2 only with Ctrl+- / Ctrl+/ / Ctrl+2..7).
                    }

                    if !*pressed {
                        continue;
                    }

                    let modes = *terminal.active_snapshot().modes;
                    if let Some(bytes) = encode_egui_key_legacy(&modes, *key, *modifiers) {
                        out.any_user_activity = true;
                        out.bytes_to_write.extend_from_slice(&bytes);
                        skip_next_text = paired_text.is_some();
                    }
                }
                Event::Text(text) => {
                    if skip_next_text {
                        skip_next_text = false;
                        continue;
                    }
                    if !text.is_empty() {
                        out.any_user_activity = true;
                        out.bytes_to_write.extend_from_slice(text.as_bytes());
                    }
                }
                _ => {}
            }
        }

        out
    }
}

fn handle_copy_shortcut(
    ui: &Ui,
    config: &TerminalConfig,
    terminal: &Terminal,
    is_mac: bool,
    from_copy_event: bool,
    out_bytes: &mut Vec<u8>,
) {
    match config.effective_copy_behavior() {
        CopyBehavior::AlwaysCopy => {
            if let Some(sel) = terminal.selection_text().filter(|s| !s.is_empty()) {
                copy_text_everywhere(ui, sel);
            }
        }
        CopyBehavior::AlwaysSendEtx => {
            if !(from_copy_event && is_mac) {
                out_bytes.push(0x03);
            }
        }
        CopyBehavior::CopyIfSelectionElseSendEtx => {
            if let Some(sel) = terminal.selection_text().filter(|s| !s.is_empty()) {
                copy_text_everywhere(ui, sel);
            } else if !(from_copy_event && is_mac) {
                out_bytes.push(0x03);
            }
        }
    }
}

pub(crate) fn copy_text_everywhere(ui: &Ui, text: String) {
    #[cfg(all(feature = "native-clipboard", not(target_arch = "wasm32")))]
    {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(text.clone());
        }
    }
    ui.ctx().copy_text(text);
}

pub(crate) fn try_read_native_clipboard() -> Option<String> {
    #[cfg(all(feature = "native-clipboard", not(target_arch = "wasm32")))]
    {
        arboard::Clipboard::new().ok()?.get_text().ok()
    }
    #[cfg(not(all(feature = "native-clipboard", not(target_arch = "wasm32"))))]
    {
        None
    }
}

fn pos_to_cell(
    pos: Pos2,
    term_rect: Rect,
    cell_size: Vec2,
    cols: u16,
    rows: u16,
) -> Option<(u16, u16)> {
    if cols == 0
        || rows == 0
        || cell_size.x <= 0.0
        || cell_size.y <= 0.0
        || !term_rect.contains(pos)
    {
        return None;
    }
    let col = (((pos.x - term_rect.min.x) / cell_size.x).floor() as u16).min(cols - 1);
    let row = (((pos.y - term_rect.min.y) / cell_size.y).floor() as u16).min(rows - 1);
    Some((col, row))
}

fn clamp_pos_to_cell(
    pos: Pos2,
    term_rect: Rect,
    cell_size: Vec2,
    cols: u16,
    rows: u16,
) -> Option<(u16, u16)> {
    if cols == 0 || rows == 0 || cell_size.x <= 0.0 || cell_size.y <= 0.0 {
        return None;
    }
    let rx = ((pos.x - term_rect.min.x) / cell_size.x).floor();
    let ry = ((pos.y - term_rect.min.y) / cell_size.y).floor();
    let col = rx.clamp(0.0, f32::from(cols - 1)) as u16;
    let row = ry.clamp(0.0, f32::from(rows - 1)) as u16;
    Some((col, row))
}

const fn to_mouse_mods(mods: Modifiers) -> MouseModifiers {
    MouseModifiers {
        shift: mods.shift,
        alt: mods.alt,
        ctrl: mods.ctrl,
    }
}

const fn to_mouse_button(btn: PointerButton) -> MouseButton {
    match btn {
        PointerButton::Primary | PointerButton::Extra1 | PointerButton::Extra2 => MouseButton::Left,
        PointerButton::Middle => MouseButton::Middle,
        PointerButton::Secondary => MouseButton::Right,
    }
}

const fn mouse_button_bit(btn: MouseButton) -> u8 {
    match btn {
        MouseButton::Left => 1 << 0,
        MouseButton::Middle => 1 << 1,
        MouseButton::Right => 1 << 2,
        MouseButton::WheelUp => 1 << 3,
        MouseButton::WheelDown => 1 << 4,
        MouseButton::WheelLeft => 1 << 5,
        MouseButton::WheelRight => 1 << 6,
    }
}

const fn first_pressed_button(mask: u8) -> Option<MouseButton> {
    if (mask & mouse_button_bit(MouseButton::Left)) != 0 {
        Some(MouseButton::Left)
    } else if (mask & mouse_button_bit(MouseButton::Middle)) != 0 {
        Some(MouseButton::Middle)
    } else if (mask & mouse_button_bit(MouseButton::Right)) != 0 {
        Some(MouseButton::Right)
    } else {
        None
    }
}

#[cfg(feature = "kitty-keyboard")]
fn encode_egui_key_kitty(
    flags: egui_xterm_core::KittyKeyboardFlags,
    modes: &egui_xterm_core::Modes,
    key: Key,
    pressed: bool,
    repeat: bool,
    modifiers: Modifiers,
    associated_text: Option<&str>,
) -> Option<Vec<u8>> {
    use egui_xterm_core::{
        KittyEventType, KittyKeyEvent, KittyKeyboardFlags, KittyModifiers, encode_kitty_key,
    };

    let event_type = if !pressed {
        KittyEventType::Release
    } else if repeat {
        KittyEventType::Repeat
    } else {
        KittyEventType::Press
    };

    let k_mods = KittyModifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        ctrl: modifiers.ctrl,
        super_key: modifiers.mac_cmd || modifiers.command && !modifiers.ctrl,
        hyper: false,
        meta: false,
        caps_lock: false,
        num_lock: false,
    };

    // Preserve DECCKM application cursor keys when no modifiers/event-types/report-all apply.
    if modes.app_cursor_keys
        && event_type == KittyEventType::Press
        && !k_mods.has_active_modifiers()
        && !flags.contains(KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES)
    {
        match key {
            Key::ArrowUp => return Some(b"\x1bOA".to_vec()),
            Key::ArrowDown => return Some(b"\x1bOB".to_vec()),
            Key::ArrowRight => return Some(b"\x1bOC".to_vec()),
            Key::ArrowLeft => return Some(b"\x1bOD".to_vec()),
            Key::Home => return Some(b"\x1bOH".to_vec()),
            Key::End => return Some(b"\x1bOF".to_vec()),
            _ => {}
        }
    }

    let (kitty_key, shifted_key, base_layout_key) = egui_key_to_kitty(key)?;
    let ev = KittyKeyEvent {
        key: kitty_key,
        event_type,
        modifiers: k_mods,
        shifted_key,
        base_layout_key,
        associated_text,
    };
    encode_kitty_key(flags, &ev)
}

#[cfg(feature = "kitty-keyboard")]
fn egui_key_to_kitty(key: Key) -> Option<(egui_xterm_core::KittyKey, Option<char>, Option<char>)> {
    use egui_xterm_core::KittyKey;

    if let Some((base, shifted)) = key_to_ascii_pair(key) {
        return Some((KittyKey::Char(base), Some(shifted), Some(base)));
    }
    let kk = match key {
        Key::Enter => KittyKey::Enter,
        Key::Tab => KittyKey::Tab,
        Key::Backspace => KittyKey::Backspace,
        Key::Escape => KittyKey::Escape,
        Key::Space => KittyKey::Space,
        Key::ArrowUp => KittyKey::Up,
        Key::ArrowDown => KittyKey::Down,
        Key::ArrowRight => KittyKey::Right,
        Key::ArrowLeft => KittyKey::Left,
        Key::Home => KittyKey::Home,
        Key::End => KittyKey::End,
        Key::Insert => KittyKey::Insert,
        Key::Delete => KittyKey::Delete,
        Key::PageUp => KittyKey::PageUp,
        Key::PageDown => KittyKey::PageDown,
        Key::CapsLock => KittyKey::CapsLock,
        Key::ScrollLock => KittyKey::ScrollLock,
        Key::NumLock => KittyKey::NumLock,
        Key::PrintScreen => KittyKey::PrintScreen,
        Key::Pause => KittyKey::Pause,
        Key::Menu => KittyKey::Menu,
        Key::ShiftLeft => KittyKey::LeftShift,
        Key::ShiftRight => KittyKey::RightShift,
        Key::ControlLeft => KittyKey::LeftControl,
        Key::ControlRight => KittyKey::RightControl,
        Key::AltLeft => KittyKey::LeftAlt,
        Key::AltRight => KittyKey::RightAlt,
        Key::SuperLeft => KittyKey::LeftSuper,
        Key::SuperRight => KittyKey::RightSuper,
        Key::F1 => KittyKey::F(1),
        Key::F2 => KittyKey::F(2),
        Key::F3 => KittyKey::F(3),
        Key::F4 => KittyKey::F(4),
        Key::F5 => KittyKey::F(5),
        Key::F6 => KittyKey::F(6),
        Key::F7 => KittyKey::F(7),
        Key::F8 => KittyKey::F(8),
        Key::F9 => KittyKey::F(9),
        Key::F10 => KittyKey::F(10),
        Key::F11 => KittyKey::F(11),
        Key::F12 => KittyKey::F(12),
        Key::F13 => KittyKey::F(13),
        Key::F14 => KittyKey::F(14),
        Key::F15 => KittyKey::F(15),
        Key::F16 => KittyKey::F(16),
        Key::F17 => KittyKey::F(17),
        Key::F18 => KittyKey::F(18),
        Key::F19 => KittyKey::F(19),
        Key::F20 => KittyKey::F(20),
        Key::F21 => KittyKey::F(21),
        Key::F22 => KittyKey::F(22),
        Key::F23 => KittyKey::F(23),
        Key::F24 => KittyKey::F(24),
        _ => return None,
    };
    Some((kk, None, None))
}

fn encode_egui_key_legacy(
    modes: &egui_xterm_core::Modes,
    key: Key,
    modifiers: Modifiers,
) -> Option<Vec<u8>> {
    let mod_param = legacy_modifier_param(modifiers);

    match key {
        Key::Enter => {
            let base: &[u8] = if modes.linefeed_newline {
                b"\r\n"
            } else {
                b"\r"
            };
            if modifiers.alt {
                let mut out = Vec::with_capacity(base.len() + 1);
                out.push(0x1B);
                out.extend_from_slice(base);
                return Some(out);
            }
            return Some(base.to_vec());
        }
        Key::Tab => {
            if modifiers.shift {
                return Some(b"\x1b[Z".to_vec());
            }
            return Some(b"\t".to_vec());
        }
        Key::Backspace => {
            if modifiers.alt {
                return Some(b"\x1b\x7f".to_vec());
            }
            if modifiers.ctrl {
                return Some(b"\x08".to_vec());
            }
            return Some(b"\x7f".to_vec());
        }
        Key::Escape => return Some(b"\x1b".to_vec()),
        Key::ArrowUp => return Some(encode_csi_cursor(b'A', mod_param, modes.app_cursor_keys)),
        Key::ArrowDown => return Some(encode_csi_cursor(b'B', mod_param, modes.app_cursor_keys)),
        Key::ArrowRight => return Some(encode_csi_cursor(b'C', mod_param, modes.app_cursor_keys)),
        Key::ArrowLeft => return Some(encode_csi_cursor(b'D', mod_param, modes.app_cursor_keys)),
        Key::Home => return Some(encode_csi_cursor(b'H', mod_param, modes.app_cursor_keys)),
        Key::End => return Some(encode_csi_cursor(b'F', mod_param, modes.app_cursor_keys)),
        Key::Insert => return Some(encode_tilde_key(2, mod_param)),
        Key::Delete => return Some(encode_tilde_key(3, mod_param)),
        Key::PageUp => return Some(encode_tilde_key(5, mod_param)),
        Key::PageDown => return Some(encode_tilde_key(6, mod_param)),
        Key::F1 => return Some(encode_ss3_or_csi(b'P', mod_param)),
        Key::F2 => return Some(encode_ss3_or_csi(b'Q', mod_param)),
        Key::F3 => return Some(encode_ss3_or_csi(b'R', mod_param)),
        Key::F4 => return Some(encode_ss3_or_csi(b'S', mod_param)),
        Key::F5 => return Some(encode_tilde_key(15, mod_param)),
        Key::F6 => return Some(encode_tilde_key(17, mod_param)),
        Key::F7 => return Some(encode_tilde_key(18, mod_param)),
        Key::F8 => return Some(encode_tilde_key(19, mod_param)),
        Key::F9 => return Some(encode_tilde_key(20, mod_param)),
        Key::F10 => return Some(encode_tilde_key(21, mod_param)),
        Key::F11 => return Some(encode_tilde_key(23, mod_param)),
        Key::F12 => return Some(encode_tilde_key(24, mod_param)),
        _ => {}
    }

    // Ctrl + key -> C0 control codes.
    if modifiers.ctrl && !modifiers.mac_cmd {
        let ctrl_byte = match key {
            Key::Space | Key::Num2 => Some(0x00),
            Key::A => Some(0x01),
            Key::B => Some(0x02),
            Key::C => Some(0x03),
            Key::D => Some(0x04),
            Key::E => Some(0x05),
            Key::F => Some(0x06),
            Key::G => Some(0x07),
            Key::H => Some(0x08),
            Key::I => Some(0x09),
            Key::J => Some(0x0A),
            Key::K => Some(0x0B),
            Key::L => Some(0x0C),
            Key::M => Some(0x0D),
            Key::N => Some(0x0E),
            Key::O => Some(0x0F),
            Key::P => Some(0x10),
            Key::Q => Some(0x11),
            Key::R => Some(0x12),
            Key::S => Some(0x13),
            Key::T => Some(0x14),
            Key::U => Some(0x15),
            Key::V => Some(0x16),
            Key::W => Some(0x17),
            Key::X => Some(0x18),
            Key::Y => Some(0x19),
            Key::Z => Some(0x1A),
            Key::OpenBracket | Key::Num3 => Some(0x1B),
            Key::Backslash | Key::Num4 => Some(0x1C),
            Key::CloseBracket | Key::Num5 => Some(0x1D),
            Key::Num6 => Some(0x1E),
            Key::Minus | Key::Num7 | Key::Slash => Some(0x1F),
            _ => None,
        };
        if let Some(b) = ctrl_byte {
            return if modifiers.alt {
                Some(vec![0x1B, b])
            } else {
                Some(vec![b])
            };
        }
    }

    // Alt + printable key -> ESC prefix.
    if modifiers.alt && !modifiers.ctrl && !modifiers.mac_cmd {
        if let Some((base, shifted)) = key_to_ascii_pair(key) {
            let ch = if modifiers.shift { shifted } else { base };
            return Some(vec![0x1B, ch as u8]);
        }
        if key == Key::Space {
            return Some(vec![0x1B, b' ']);
        }
    }

    None
}

const fn legacy_modifier_param(mods: Modifiers) -> u8 {
    let mut mask = 0u8;
    if mods.shift {
        mask |= 1;
    }
    if mods.alt {
        mask |= 2;
    }
    if mods.ctrl {
        mask |= 4;
    }
    if mask == 0 { 1 } else { 1 + mask }
}

fn encode_csi_cursor(final_ch: u8, mod_param: u8, app_cursor: bool) -> Vec<u8> {
    if mod_param > 1 {
        format!("\x1b[1;{mod_param}{}", final_ch as char).into_bytes()
    } else if app_cursor {
        vec![0x1B, b'O', final_ch]
    } else {
        vec![0x1B, b'[', final_ch]
    }
}

fn encode_tilde_key(num: u16, mod_param: u8) -> Vec<u8> {
    if mod_param > 1 {
        format!("\x1b[{num};{mod_param}~").into_bytes()
    } else {
        format!("\x1b[{num}~").into_bytes()
    }
}

fn encode_ss3_or_csi(final_ch: u8, mod_param: u8) -> Vec<u8> {
    if mod_param > 1 {
        format!("\x1b[1;{mod_param}{}", final_ch as char).into_bytes()
    } else {
        vec![0x1B, b'O', final_ch]
    }
}

const fn key_to_ascii_pair(key: Key) -> Option<(char, char)> {
    Some(match key {
        Key::A => ('a', 'A'),
        Key::B => ('b', 'B'),
        Key::C => ('c', 'C'),
        Key::D => ('d', 'D'),
        Key::E => ('e', 'E'),
        Key::F => ('f', 'F'),
        Key::G => ('g', 'G'),
        Key::H => ('h', 'H'),
        Key::I => ('i', 'I'),
        Key::J => ('j', 'J'),
        Key::K => ('k', 'K'),
        Key::L => ('l', 'L'),
        Key::M => ('m', 'M'),
        Key::N => ('n', 'N'),
        Key::O => ('o', 'O'),
        Key::P => ('p', 'P'),
        Key::Q => ('q', 'Q'),
        Key::R => ('r', 'R'),
        Key::S => ('s', 'S'),
        Key::T => ('t', 'T'),
        Key::U => ('u', 'U'),
        Key::V => ('v', 'V'),
        Key::W => ('w', 'W'),
        Key::X => ('x', 'X'),
        Key::Y => ('y', 'Y'),
        Key::Z => ('z', 'Z'),
        Key::Num0 => ('0', ')'),
        Key::Num1 => ('1', '!'),
        Key::Num2 => ('2', '@'),
        Key::Num3 => ('3', '#'),
        Key::Num4 => ('4', '$'),
        Key::Num5 => ('5', '%'),
        Key::Num6 => ('6', '^'),
        Key::Num7 => ('7', '&'),
        Key::Num8 => ('8', '*'),
        Key::Num9 => ('9', '('),
        Key::Minus => ('-', '_'),
        Key::Equals => ('=', '+'),
        Key::OpenBracket => ('[', '{'),
        Key::CloseBracket => (']', '}'),
        Key::Backslash => ('\\', '|'),
        Key::Semicolon => (';', ':'),
        Key::Quote => ('\'', '"'),
        Key::Comma => (',', '<'),
        Key::Period => ('.', '>'),
        Key::Slash => ('/', '?'),
        Key::Backtick => ('`', '~'),
        Key::Plus => ('+', '+'),
        Key::Colon => (':', ':'),
        Key::Pipe => ('|', '|'),
        Key::Questionmark => ('?', '?'),
        Key::Exclamationmark => ('!', '!'),
        Key::OpenCurlyBracket => ('{', '{'),
        Key::CloseCurlyBracket => ('}', '}'),
        _ => return None,
    })
}
