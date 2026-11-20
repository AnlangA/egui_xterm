//! High-performance 4-stage terminal renderer (`bg_mesh`, dirty-row `RowRenderCache`,
//! `vector-glyphs`/decorations/cursor `overlay_mesh`, batched `Painter::extend`).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::Vec2;
use egui::emath::GuiRounding as _;
use egui::epaint::text::Glyph;
use egui::epaint::{Mesh, PathShape, TextOptions, TextShape, Vertex};
use egui::text::{ByteIndex, LayoutJob, LayoutSection, TextFormat, TextWrapping};
use egui::{Color32, FontFamily, FontId, Galley, Pos2, Rect, Stroke, Ui, pos2};
use egui_xterm_core::{
    CellFlags, ColorScheme, CursorShape, PackedColor, RenderSnapshot, Terminal, Theme,
    UnderlineStyle,
};

use crate::config::TerminalConfig;
use crate::layout::{FrameLayout, snap_px, snap_rect, snap_size_at_least_1px};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GlobalRenderKey {
    cols: u16,
    rows: u16,
    ppp_bits: u32,
    font_size_bits: u32,
    raw_cell_w_bits: u32,
    cell_w_bits: u32,
    cell_h_bits: u32,
    is_alt_screen: bool,
    reverse_video: bool,
    theme_hash: u64,
    palette_hash: u64,
}

#[derive(Debug, Clone, Copy)]
struct RowDecoration {
    start_col: u16,
    end_col: u16,
    underline: UnderlineStyle,
    underline_color: Color32,
    strikethrough: bool,
    overline: bool,
    fg_color: Color32,
}

#[cfg(feature = "vector-glyphs")]
#[derive(Debug, Clone, Copy)]
struct VectorGlyphCell {
    col: u16,
    ch: char,
    color: Color32,
}

#[derive(Debug, Clone, Default)]
struct CachedRowSlot {
    valid: bool,
    row_gen: u64,
    selection_range: Option<(usize, usize)>,
    cursor_col: Option<u16>,
    start_col: u16,
    galley: Option<Arc<Galley>>,
    decorations: Vec<RowDecoration>,
    #[cfg(feature = "vector-glyphs")]
    vector_cells: Vec<VectorGlyphCell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClusterWidthKey {
    text: String,
    bold: bool,
    italic: bool,
}

#[derive(Debug, Clone, Copy)]
struct ClusterMetrics {
    advance: f32,
    glyph_count: usize,
}

/// Active IME composition preedit state rendered inline at the cursor.
#[derive(Debug, Clone, Default)]
pub(crate) struct ImePreeditState {
    pub text: String,
    pub active_range_chars: Option<std::ops::Range<usize>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BgCacheKey {
    global: GlobalRenderKey,
    origin_x_bits: u32,
    origin_y_bits: u32,
    damage_epoch: u64,
    top_abs_line: u64,
    block_cursor: Option<(u16, u16)>,
    selection_hash: u64,
    search_hash: u64,
}

#[derive(Clone, Copy)]
struct AsciiGlyphEntry {
    glyph: Glyph,
    has_quad: bool,
    rect_min: Pos2,
    rect_size: egui::Vec2,
    uv_min: Pos2,
    uv_max: Pos2,
}

struct AsciiGlyphCache {
    entries: [Option<AsciiGlyphEntry>; 95],
    unsupported: [bool; 95],
    base_galley: Option<Arc<Galley>>,
    row_height: f32,
}

impl Default for AsciiGlyphCache {
    fn default() -> Self {
        Self {
            entries: [None; 95],
            unsupported: [false; 95],
            base_galley: None,
            row_height: 0.0,
        }
    }
}

impl AsciiGlyphCache {
    fn clear(&mut self) {
        self.entries = [None; 95];
        self.unsupported = [false; 95];
        self.base_galley = None;
        self.row_height = 0.0;
    }
}

/// Persistent per-runtime 4-stage cached renderer.
pub(crate) struct TerminalRenderer {
    bg_mesh: Arc<Mesh>,
    last_bg_key: Option<BgCacheKey>,
    overlay_mesh: Arc<Mesh>,
    scratch_cluster: String,
    scratch_paths: Vec<PathShape>,
    scratch_shapes: Vec<egui::Shape>,
    col_x_cache: Vec<f32>,
    row_y_cache: Vec<f32>,
    slots: Vec<CachedRowSlot>,
    last_global_key: Option<GlobalRenderKey>,
    last_top_abs_line: u64,
    last_atlas_fill_ratio: f32,
    last_atlas_height: usize,
    atlas_exhausted_repaint_sent: bool,
    last_text_options: Option<TextOptions>,
    atlas_sentinel_galley: Option<Arc<Galley>>,
    cluster_width_cache: HashMap<ClusterWidthKey, ClusterMetrics>,
    ascii_glyph_cache: AsciiGlyphCache,
    // Track DECSCUSR vs user config overrides for cursor shape & blink.
    last_term_cursor_shape: Option<CursorShape>,
    last_cfg_cursor_shape: CursorShape,
    effective_cursor_shape: CursorShape,
    last_term_cursor_blink: Option<bool>,
    last_cfg_cursor_blink: bool,
    effective_cursor_blink: bool,
    last_input_time_secs: f64,
}

impl Default for TerminalRenderer {
    fn default() -> Self {
        Self {
            bg_mesh: Arc::new(Mesh::default()),
            last_bg_key: None,
            overlay_mesh: Arc::new(Mesh::default()),
            scratch_cluster: String::with_capacity(16),
            scratch_paths: Vec::new(),
            scratch_shapes: Vec::new(),
            col_x_cache: Vec::new(),
            row_y_cache: Vec::new(),
            slots: Vec::new(),
            last_global_key: None,
            last_top_abs_line: 0,
            last_atlas_fill_ratio: 0.0,
            last_atlas_height: 0,
            atlas_exhausted_repaint_sent: false,
            last_text_options: None,
            atlas_sentinel_galley: None,
            cluster_width_cache: HashMap::new(),
            ascii_glyph_cache: AsciiGlyphCache::default(),
            last_term_cursor_shape: None,
            last_cfg_cursor_shape: CursorShape::Block,
            effective_cursor_shape: CursorShape::Block,
            last_term_cursor_blink: None,
            last_cfg_cursor_blink: true,
            effective_cursor_blink: true,
            last_input_time_secs: f64::NEG_INFINITY,
        }
    }
}

impl TerminalRenderer {
    /// Records user interaction time so cursor blink stays momentarily solid while typing.
    pub(crate) fn note_input_activity(&mut self, now_secs: f64) {
        self.last_input_time_secs = now_secs;
    }

    pub(crate) fn effective_cursor_shape(
        &mut self,
        term_shape: CursorShape,
        cfg_shape: CursorShape,
    ) -> CursorShape {
        match self.last_term_cursor_shape {
            None => {
                self.last_term_cursor_shape = Some(term_shape);
                self.last_cfg_cursor_shape = cfg_shape;
                self.effective_cursor_shape = cfg_shape;
            }
            Some(prev_term) => {
                if term_shape != prev_term {
                    self.last_term_cursor_shape = Some(term_shape);
                    self.effective_cursor_shape = term_shape;
                } else if cfg_shape != self.last_cfg_cursor_shape {
                    self.last_cfg_cursor_shape = cfg_shape;
                    self.effective_cursor_shape = cfg_shape;
                }
            }
        }
        self.effective_cursor_shape
    }

    pub(crate) fn effective_cursor_blink(&mut self, term_blink: bool, cfg_blink: bool) -> bool {
        match self.last_term_cursor_blink {
            None => {
                self.last_term_cursor_blink = Some(term_blink);
                self.last_cfg_cursor_blink = cfg_blink;
                self.effective_cursor_blink = cfg_blink;
            }
            Some(prev_term) => {
                if term_blink != prev_term {
                    self.last_term_cursor_blink = Some(term_blink);
                    self.effective_cursor_blink = term_blink;
                } else if cfg_blink != self.last_cfg_cursor_blink {
                    self.last_cfg_cursor_blink = cfg_blink;
                    self.effective_cursor_blink = cfg_blink;
                }
            }
        }
        self.effective_cursor_blink
    }

    /// Executes the 4-stage render pipeline and returns the cursor anchor rect (in local layer
    /// coordinates) for IME positioning.
    #[expect(clippy::too_many_arguments, reason = "per-frame render context")]
    pub(crate) fn render(
        &mut self,
        ui: &mut Ui,
        layout: &FrameLayout,
        config: &TerminalConfig,
        terminal: &mut Terminal,
        now_secs: f64,
        has_focus: bool,
        ime_preedit: &ImePreeditState,
        #[cfg(feature = "hyperlinks")] hovered_link_spans: &[egui_xterm_core::HyperlinkSpan],
    ) -> Rect {
        let origin = layout.term_rect.min;
        let cell_w = layout.cell_size.x;
        let cell_h = layout.cell_size.y;
        let ppp = layout.pixels_per_point;

        // Read snapshot, search matches, and shell zones once (all shared `&Terminal` borrows).
        #[cfg(feature = "search")]
        let search_matches = terminal.search_matches();
        #[cfg(feature = "search")]
        let active_search = terminal.active_search_match().copied();
        #[cfg(feature = "search")]
        let search_hash: u64 = if search_matches.is_empty() {
            0
        } else {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            search_matches.hash(&mut h);
            active_search.hash(&mut h);
            h.finish()
        };
        #[cfg(not(feature = "search"))]
        let search_hash: u64 = 0;

        #[cfg(feature = "shell-integration")]
        let shell_zones = terminal.shell_zones();

        // Borrow active_snapshot once across cursor setup and Stages 1-3 (DEC Mode 2026 safe).
        let (dirty_rows, default_fg, default_bg, ime_cursor_rect_base) = {
            let snapshot = terminal.active_snapshot();
            let cursor_pos = (snapshot.cursor.col, snapshot.cursor.row);
            let cursor_visible = snapshot.cursor.visible;
            let term_cursor_shape = snapshot.cursor.style.shape;
            let term_cursor_blink = snapshot.cursor.style.blink;

            let cursor_shape = self.effective_cursor_shape(term_cursor_shape, config.cursor_shape);
            let cursor_blink = self.effective_cursor_blink(term_cursor_blink, config.cursor_blink);

            // Compute deterministic blink phase from egui frame time.
            let cursor_blink_on = if !has_focus {
                false
            } else if !cursor_blink {
                true
            } else {
                let raw_period = config.cursor_config.blink_period_secs;
                let period = if raw_period.is_finite() {
                    raw_period.clamp(0.2, 60.0)
                } else {
                    1.0
                };
                let half = period * 0.5;
                let safe_now = if now_secs.is_finite() { now_secs } else { 0.0 };
                let recent_input = self.last_input_time_secs.is_finite()
                    && (safe_now - self.last_input_time_secs) < half;
                if recent_input {
                    true
                } else {
                    let phase = (safe_now / half).floor();
                    let is_on = (phase as i64).rem_euclid(2) == 0;
                    let next_toggle = ((phase + 1.0) * half - safe_now).clamp(0.01, half);
                    if next_toggle.is_finite() {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_secs_f64(next_toggle));
                    }
                    is_on
                }
            };

            let block_cursor_active = cursor_visible
                && has_focus
                && cursor_blink_on
                && cursor_shape == CursorShape::Block;

            let (default_fg, default_bg) = resolve_default_pair(&config.theme.scheme, &snapshot);

            let global_key = GlobalRenderKey {
                cols: snapshot.cols,
                rows: snapshot.rows,
                ppp_bits: ppp.to_bits(),
                font_size_bits: layout.effective_font_size.to_bits(),
                raw_cell_w_bits: layout.raw_cell_w.to_bits(),
                cell_w_bits: cell_w.to_bits(),
                cell_h_bits: cell_h.to_bits(),
                is_alt_screen: snapshot.is_alt_screen,
                reverse_video: snapshot.modes.reverse_video,
                theme_hash: hash_theme(&config.theme),
                palette_hash: hash_palette(&snapshot),
            };

            let selection_hash = if let Some(sel) = snapshot.selection {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                sel.mode.hash(&mut h);
                sel.anchor.line.0.hash(&mut h);
                sel.anchor.col.hash(&mut h);
                sel.head.line.0.hash(&mut h);
                sel.head.col.hash(&mut h);
                h.finish()
            } else {
                0
            };

            let bg_key = BgCacheKey {
                global: global_key,
                origin_x_bits: origin.x.to_bits(),
                origin_y_bits: origin.y.to_bits(),
                damage_epoch: snapshot.damage_epoch,
                top_abs_line: snapshot.top_abs_line.0,
                block_cursor: if block_cursor_active {
                    Some(cursor_pos)
                } else {
                    None
                },
                selection_hash,
                search_hash,
            };

            // Stage 1: Background, selection, search highlights, and filled Block cursor.
            if self.last_bg_key != Some(bg_key) {
                self.last_bg_key = Some(bg_key);
                let cols_usize = usize::from(snapshot.cols);
                let rows_usize = snapshot.visible_rows.len();
                self.col_x_cache.clear();
                self.col_x_cache
                    .extend((0..=cols_usize).map(|c| snap_px(origin.x + c as f32 * cell_w, ppp)));
                self.row_y_cache.clear();
                self.row_y_cache
                    .extend((0..=rows_usize).map(|r| snap_px(origin.y + r as f32 * cell_h, ppp)));

                let bg_mesh = Arc::make_mut(&mut self.bg_mesh);
                bg_mesh.vertices.clear();
                bg_mesh.indices.clear();
                let snapped_term_rect = snap_rect(layout.term_rect, ppp);
                if snapped_term_rect.is_positive() {
                    bg_mesh.add_colored_rect(snapped_term_rect, default_bg);
                }

                #[cfg(feature = "search")]
                let has_search = !search_matches.is_empty();
                #[cfg(not(feature = "search"))]
                let has_search = false;
                let has_selection = snapshot.selection.is_some();

                #[cfg(feature = "search")]
                let retained_top = snapshot
                    .scrollback_len
                    .saturating_sub(snapshot.display_offset);

                for (vp_row, row) in snapshot.visible_rows.iter().enumerate() {
                    let row_y0 = self.row_y_cache[vp_row];
                    let row_y1 = self.row_y_cache[vp_row + 1];
                    if row_y1 <= row_y0 {
                        continue;
                    }
                    let row_cols = cols_usize.min(row.len());
                    let cells = row.cells();
                    let mut run_start_col: usize = 0;
                    let mut run_color: Option<Color32> = None;

                    if !has_selection && !has_search {
                        for (col, &cell) in cells.iter().enumerate().take(row_cols) {
                            let bg_c32 = if cell.flags.contains(CellFlags::INVERSE) {
                                resolve_fg_color(
                                    cell.fg,
                                    default_fg,
                                    &config.theme,
                                    &snapshot,
                                    false,
                                )
                            } else {
                                resolve_bg_color(cell.bg, default_bg, &config.theme, &snapshot)
                            };
                            let cell_color = if bg_c32 == default_bg {
                                None
                            } else {
                                Some(bg_c32)
                            };
                            if cell_color != run_color {
                                if let Some(color) = run_color {
                                    let x0 = self.col_x_cache[run_start_col];
                                    let x1 = self.col_x_cache[col];
                                    if x1 > x0 {
                                        bg_mesh.add_colored_rect(
                                            Rect::from_min_max(pos2(x0, row_y0), pos2(x1, row_y1)),
                                            color,
                                        );
                                    }
                                }
                                run_start_col = col;
                                run_color = cell_color;
                            }
                        }
                    } else {
                        let sel_range = snapshot.selection.and_then(|sel| {
                            let line =
                                egui_xterm_core::AbsLine(snapshot.top_abs_line.0 + vp_row as u64);
                            sel.row_col_range(line, row)
                        });
                        #[cfg(feature = "search")]
                        let retained_line = retained_top + vp_row;
                        #[cfg(feature = "search")]
                        let row_has_search = has_search
                            && search_matches
                                .iter()
                                .any(|m| retained_line >= m.line && retained_line <= m.end_line);

                        for (col, &cell) in cells.iter().enumerate().take(row_cols) {
                            let inverse = cell.flags.contains(CellFlags::INVERSE);
                            let mut bg_c32 = if inverse {
                                resolve_fg_color(
                                    cell.fg,
                                    default_fg,
                                    &config.theme,
                                    &snapshot,
                                    false,
                                )
                            } else {
                                resolve_bg_color(cell.bg, default_bg, &config.theme, &snapshot)
                            };

                            let selected = sel_range.is_some_and(|(c0, c1)| col >= c0 && col <= c1);
                            if selected {
                                bg_c32 = rgb_to_color32(config.theme.scheme.selection_bg);
                            }

                            #[cfg(feature = "search")]
                            if row_has_search
                                && let Some(hl_color) = search_highlight_at(
                                    search_matches,
                                    active_search,
                                    retained_line,
                                    col as u16,
                                )
                            {
                                bg_c32 = hl_color;
                            }

                            let cell_color = if bg_c32 == default_bg {
                                None
                            } else {
                                Some(bg_c32)
                            };

                            if cell_color != run_color {
                                if let Some(color) = run_color {
                                    let x0 = self.col_x_cache[run_start_col];
                                    let x1 = self.col_x_cache[col];
                                    if x1 > x0 {
                                        bg_mesh.add_colored_rect(
                                            Rect::from_min_max(pos2(x0, row_y0), pos2(x1, row_y1)),
                                            color,
                                        );
                                    }
                                }
                                run_start_col = col;
                                run_color = cell_color;
                            }
                        }
                    }

                    if let Some(color) = run_color {
                        let x0 = self.col_x_cache[run_start_col];
                        let x1 = self.col_x_cache[row_cols];
                        if x1 > x0 {
                            bg_mesh.add_colored_rect(
                                Rect::from_min_max(pos2(x0, row_y0), pos2(x1, row_y1)),
                                color,
                            );
                        }
                    }
                }

                // Filled Block cursor placed in bg_mesh so cursor cell text stays legible above it.
                if block_cursor_active
                    && cursor_pos.0 < snapshot.cols
                    && cursor_pos.1 < snapshot.rows
                {
                    let c_rect =
                        cursor_cell_rect(origin, cursor_pos.0, cursor_pos.1, cell_w, cell_h, ppp);
                    if c_rect.is_positive() {
                        let cursor_c32 = resolve_cursor_color(&config.theme.scheme, &snapshot);
                        bg_mesh.add_colored_rect(c_rect, cursor_c32);
                    }
                }
            }

            // Stage 2: Dirty-row RowRenderCache with atlas-reset detection, slot rotation,
            // and per-glyph cached ASCII row galley fast-path.

            let row_count = snapshot.visible_rows.len();
            if self.slots.len() != row_count {
                self.slots.resize_with(row_count, CachedRowSlot::default);
                for slot in &mut self.slots {
                    slot.valid = false;
                }
            }

            let mut dirty_count: u64 = 0;
            let cursor_text_c32 = rgb_to_color32(config.theme.scheme.cursor_text);
            let sel_fg_override = config.theme.scheme.selection_fg.map(rgb_to_color32);
            let font_size = layout.effective_font_size;
            let extra_letter_spacing = cell_w - layout.raw_cell_w;
            let base_font_id = FontId::new(font_size, FontFamily::Monospace);

            let needs_repaint = ui.ctx().fonts_mut(|fonts| {
                let curr_fill = fonts.font_atlas_fill_ratio();
                let curr_height = fonts.font_image_size()[1];
                let curr_opts = *fonts.options();
                let sentinel = fonts.layout_job(LayoutJob::default());
                let sentinel_reset = !self
                    .atlas_sentinel_galley
                    .as_ref()
                    .is_some_and(|prev| Arc::ptr_eq(prev, &sentinel));
                let atlas_reset = sentinel_reset
                    || curr_fill < self.last_atlas_fill_ratio
                    || curr_height < self.last_atlas_height
                    || self.last_text_options.as_ref() != Some(&curr_opts);

                if atlas_reset {
                    self.cluster_width_cache.clear();
                    self.ascii_glyph_cache.clear();
                    self.last_text_options = Some(curr_opts);
                    self.atlas_sentinel_galley = Some(sentinel);
                }

                let global_changed = self.last_global_key.as_ref() != Some(&global_key);
                if global_changed || atlas_reset {
                    if self.last_global_key.is_some_and(|prev| {
                        prev.ppp_bits != global_key.ppp_bits
                            || prev.font_size_bits != global_key.font_size_bits
                            || prev.raw_cell_w_bits != global_key.raw_cell_w_bits
                            || prev.cell_w_bits != global_key.cell_w_bits
                            || prev.cell_h_bits != global_key.cell_h_bits
                    }) {
                        self.cluster_width_cache.clear();
                        self.ascii_glyph_cache.clear();
                    }
                    for slot in &mut self.slots {
                        slot.valid = false;
                    }
                    self.last_global_key = Some(global_key);
                    self.last_top_abs_line = snapshot.top_abs_line.0;
                } else if snapshot.top_abs_line.0 != self.last_top_abs_line {
                    let delta = snapshot.top_abs_line.0 as i64 - self.last_top_abs_line as i64;
                    let len = self.slots.len();
                    if len > 0 && delta > 0 && (delta as usize) < len {
                        let d = delta as usize;
                        self.slots.rotate_left(d);
                        for slot in &mut self.slots[len - d..] {
                            slot.valid = false;
                        }
                    } else if len > 0 && delta < 0 && ((-delta) as usize) < len {
                        let d = (-delta) as usize;
                        self.slots.rotate_right(d);
                        for slot in &mut self.slots[..d] {
                            slot.valid = false;
                        }
                    } else {
                        for slot in &mut self.slots {
                            slot.valid = false;
                        }
                    }
                    self.last_top_abs_line = snapshot.top_abs_line.0;
                }

                for (vp_row, row) in snapshot.visible_rows.iter().enumerate() {
                    let row_gen = row.generation();
                    let row_sel = if sel_fg_override.is_some() {
                        snapshot.selection.and_then(|sel| {
                            let line =
                                egui_xterm_core::AbsLine(snapshot.top_abs_line.0 + vp_row as u64);
                            sel.row_col_range(line, row)
                        })
                    } else {
                        None
                    };
                    let row_cursor_col =
                        if block_cursor_active && usize::from(cursor_pos.1) == vp_row {
                            Some(cursor_pos.0)
                        } else {
                            None
                        };

                    let slot = &mut self.slots[vp_row];
                    if slot.valid
                        && slot.row_gen == row_gen
                        && slot.selection_range == row_sel
                        && slot.cursor_col == row_cursor_col
                    {
                        continue;
                    }

                    dirty_count += 1;
                    slot.valid = true;
                    slot.row_gen = row_gen;
                    slot.selection_range = row_sel;
                    slot.cursor_col = row_cursor_col;
                    slot.decorations.clear();
                    #[cfg(feature = "vector-glyphs")]
                    slot.vector_cells.clear();

                    let cells = row.cells();
                    let row_cols = usize::from(snapshot.cols).min(cells.len());
                    let scan_end = row.trimmed_len().min(row_cols);

                    // Compute the exclusive end column of visible (non-vector, non-trailing-space)
                    // text so interior spaces stay in `LayoutJob.text` while empty rows emit no
                    // `Shape::Text`.
                    let mut row_text_end = 0usize;
                    for col in (0..scan_end).rev() {
                        let c = cells[col];
                        if c.is_wide_spacer() || c.is_wrap_spacer() {
                            continue;
                        }
                        let has_comb = c.extra_id != 0
                            && snapshot
                                .cell_extra(c.extra_id)
                                .is_some_and(|e| !e.combining.is_empty());
                        #[cfg(feature = "vector-glyphs")]
                        if !c.flags.contains(CellFlags::HIDDEN)
                            && !has_comb
                            && is_vector_glyph(c.codepoint)
                        {
                            continue;
                        }
                        if c.codepoint != ' ' || has_comb {
                            row_text_end = col + 1;
                            break;
                        }
                    }

                    // Pass 1: Collect decorations, vector cells, `first_text_col`, and check
                    // whether `first_text_col..row_text_end` is eligible for the ASCII fast path.
                    let mut first_text_col: Option<u16> = None;
                    let mut ascii_fast_eligible = row_text_end > 0;
                    let mut active_deco: Option<RowDecoration> = None;

                    for (col, &cell) in cells.iter().enumerate().take(row_cols) {
                        if cell.is_wide_spacer() || cell.is_wrap_spacer() {
                            if col < row_text_end && first_text_col.is_some() {
                                ascii_fast_eligible = false;
                            }
                            continue;
                        }
                        let col_u16 = col as u16;
                        let is_hidden = cell.flags.contains(CellFlags::HIDDEN);
                        let has_combining = cell.extra_id != 0
                            && snapshot
                                .cell_extra(cell.extra_id)
                                .is_some_and(|e| !e.combining.is_empty());
                        let underline_style = cell.underline_style();
                        let strikethrough = cell.flags.contains(CellFlags::STRIKETHROUGH);
                        let overline = cell.flags.contains(CellFlags::OVERLINE);
                        let has_deco = !is_hidden
                            && (underline_style != UnderlineStyle::None
                                || strikethrough
                                || overline);

                        #[cfg(feature = "vector-glyphs")]
                        let is_vec =
                            !is_hidden && !has_combining && is_vector_glyph(cell.codepoint);
                        #[cfg(not(feature = "vector-glyphs"))]
                        let is_vec = false;

                        if has_deco || is_vec {
                            let fg_c32 = resolve_cell_fg(
                                cell,
                                col,
                                row_cursor_col,
                                cursor_text_c32,
                                sel_fg_override,
                                row_sel,
                                default_fg,
                                default_bg,
                                &config.theme,
                                &snapshot,
                            );
                            if has_deco {
                                let cell_span_cols: u16 = if cell.is_wide_lead() { 2 } else { 1 };
                                let ul_color = if cell.extra_id != 0
                                    && let Some(ext) = snapshot.cell_extra(cell.extra_id)
                                    && let Some(ul_c) = ext.underline_color
                                {
                                    resolve_fg_color(
                                        ul_c,
                                        default_fg,
                                        &config.theme,
                                        &snapshot,
                                        false,
                                    )
                                } else {
                                    fg_c32
                                };
                                let end_col =
                                    col_u16.saturating_add(cell_span_cols).min(snapshot.cols);
                                match &mut active_deco {
                                    Some(prev)
                                        if prev.end_col == col_u16
                                            && prev.underline == underline_style
                                            && prev.underline_color == ul_color
                                            && prev.strikethrough == strikethrough
                                            && prev.overline == overline
                                            && prev.fg_color == fg_c32 =>
                                    {
                                        prev.end_col = end_col;
                                    }
                                    _ => {
                                        if let Some(prev) = active_deco.take() {
                                            slot.decorations.push(prev);
                                        }
                                        active_deco = Some(RowDecoration {
                                            start_col: col_u16,
                                            end_col,
                                            underline: underline_style,
                                            underline_color: ul_color,
                                            strikethrough,
                                            overline,
                                            fg_color: fg_c32,
                                        });
                                    }
                                }
                            } else if let Some(prev) = active_deco.take() {
                                slot.decorations.push(prev);
                            }

                            #[cfg(feature = "vector-glyphs")]
                            if is_vec {
                                slot.vector_cells.push(VectorGlyphCell {
                                    col: col_u16,
                                    ch: cell.codepoint,
                                    color: fg_c32,
                                });
                                if col < row_text_end && first_text_col.is_some() {
                                    ascii_fast_eligible = false;
                                }
                                continue;
                            }
                        } else if let Some(prev) = active_deco.take() {
                            slot.decorations.push(prev);
                        }

                        if col >= row_text_end {
                            continue;
                        }
                        if first_text_col.is_none() {
                            if cell.codepoint == ' ' && !has_combining {
                                continue;
                            }
                            first_text_col = Some(col_u16);
                        }
                        if ascii_fast_eligible
                            && (cell.extra_id != 0
                                || cell.is_wide_lead()
                                || !(' '..='~').contains(&cell.codepoint))
                        {
                            ascii_fast_eligible = false;
                        }
                    }

                    if let Some(prev) = active_deco.take() {
                        slot.decorations.push(prev);
                    }

                    let Some(start_col) = first_text_col else {
                        slot.start_col = 0;
                        slot.galley = None;
                        continue;
                    };

                    slot.start_col = start_col;
                    let anchor_idx = usize::from(start_col);

                    if ascii_fast_eligible
                        && build_ascii_row_galley(
                            &mut self.ascii_glyph_cache,
                            fonts,
                            &mut slot.galley,
                            &cells[anchor_idx..row_text_end],
                            start_col,
                            row_cursor_col,
                            cursor_text_c32,
                            sel_fg_override,
                            row_sel,
                            default_fg,
                            default_bg,
                            &config.theme,
                            &snapshot,
                            &base_font_id,
                            extra_letter_spacing,
                            cell_w,
                            ppp,
                        )
                    {
                        continue;
                    }

                    // Slow path for Unicode, combining clusters, wide chars, or fallback fonts.
                    let mut job = LayoutJob {
                        text: String::with_capacity(row_text_end.saturating_sub(anchor_idx)),
                        sections: Vec::with_capacity(
                            row_text_end.saturating_sub(anchor_idx).min(64),
                        ),
                        wrap: TextWrapping {
                            max_width: f32::INFINITY,
                            ..Default::default()
                        },
                        break_on_newline: false,
                        keep_trailing_whitespace: true,
                        ..Default::default()
                    };
                    let mut job_cursor_x: f32 = 0.0;
                    let mut is_first_glyph_in_job = true;
                    let mut slow_clusters: Vec<(f32, usize)> =
                        Vec::with_capacity(row_text_end.saturating_sub(anchor_idx));

                    for (col, &cell) in cells.iter().enumerate().take(row_text_end).skip(anchor_idx)
                    {
                        if cell.is_wide_spacer() || cell.is_wrap_spacer() {
                            continue;
                        }
                        #[cfg(feature = "vector-glyphs")]
                        {
                            let has_comb = cell.extra_id != 0
                                && snapshot
                                    .cell_extra(cell.extra_id)
                                    .is_some_and(|e| !e.combining.is_empty());
                            if !cell.flags.contains(CellFlags::HIDDEN)
                                && !has_comb
                                && is_vector_glyph(cell.codepoint)
                            {
                                continue;
                            }
                        }

                        let col_u16 = col as u16;
                        let is_bold = cell.flags.contains(CellFlags::BOLD);
                        let is_italic = cell.flags.contains(CellFlags::ITALIC);
                        let fg_c32 = resolve_cell_fg(
                            cell,
                            col,
                            row_cursor_col,
                            cursor_text_c32,
                            sel_fg_override,
                            row_sel,
                            default_fg,
                            default_bg,
                            &config.theme,
                            &snapshot,
                        );

                        let target_x = snap_px(f32::from(col_u16 - start_col) * cell_w, ppp);
                        let spacing_prefix = if is_first_glyph_in_job {
                            0.0
                        } else {
                            extra_letter_spacing
                        };
                        let leading_space = target_x - (job_cursor_x + spacing_prefix);

                        let fmt = TextFormat {
                            font_id: base_font_id.clone(),
                            color: fg_c32,
                            italics: is_italic,
                            extra_letter_spacing,
                            ..Default::default()
                        };

                        if cell.extra_id == 0 && (' '..='~').contains(&cell.codepoint) {
                            let mut ascii_buf = [0u8; 4];
                            let s = cell.codepoint.encode_utf8(&mut ascii_buf);
                            job.append(s, leading_space, fmt);
                            job_cursor_x = target_x + layout.raw_cell_w;
                            slow_clusters.push((target_x, 1));
                            is_first_glyph_in_job = false;
                        } else {
                            self.scratch_cluster.clear();
                            if cell.extra_id != 0
                                && let Some(ext) = snapshot.cell_extra(cell.extra_id)
                            {
                                ext.write_grapheme(cell.codepoint, &mut self.scratch_cluster);
                            } else {
                                self.scratch_cluster.push(cell.codepoint);
                            }

                            let is_ascii_single = self.scratch_cluster.len() == 1
                                && (' '..='~').contains(&cell.codepoint);
                            let metrics = if is_ascii_single {
                                ClusterMetrics {
                                    advance: layout.raw_cell_w,
                                    glyph_count: 1,
                                }
                            } else {
                                let key = ClusterWidthKey {
                                    text: self.scratch_cluster.clone(),
                                    bold: is_bold,
                                    italic: is_italic,
                                };
                                if let Some(&cached) = self.cluster_width_cache.get(&key) {
                                    cached
                                } else {
                                    let mut probe_job = LayoutJob {
                                        wrap: TextWrapping {
                                            max_width: f32::INFINITY,
                                            ..Default::default()
                                        },
                                        break_on_newline: false,
                                        keep_trailing_whitespace: true,
                                        ..Default::default()
                                    };
                                    probe_job.append(&self.scratch_cluster, 0.0, fmt.clone());
                                    let probe_galley = fonts.layout_job(probe_job);
                                    let m = ClusterMetrics {
                                        advance: probe_galley.intrinsic_size().x,
                                        glyph_count: probe_galley
                                            .rows
                                            .first()
                                            .map_or(0, |r| r.row.glyphs.len()),
                                    };
                                    self.cluster_width_cache.insert(key, m);
                                    m
                                }
                            };

                            job.append(&self.scratch_cluster, leading_space, fmt);
                            job_cursor_x = target_x + metrics.advance;
                            slow_clusters.push((target_x, metrics.glyph_count));
                            is_first_glyph_in_job = false;
                        }
                    }

                    let mut galley = fonts.layout_job(job);
                    snap_slow_galley_clusters(&mut galley, &slow_clusters);
                    slot.galley = Some(galley);
                }

                let post_fill = fonts.font_atlas_fill_ratio();
                let post_height = fonts.font_image_size()[1];
                let mid_loop_reset = post_fill < curr_fill || post_height < curr_height;
                let newly_exhausted = post_fill >= 1.0 && !self.atlas_exhausted_repaint_sent;
                let mut needs_repaint = false;
                if mid_loop_reset || newly_exhausted {
                    self.cluster_width_cache.clear();
                    self.ascii_glyph_cache.clear();
                    self.atlas_sentinel_galley = None;
                    for slot in &mut self.slots {
                        slot.valid = false;
                    }
                    needs_repaint = true;
                    if post_fill >= 1.0 {
                        self.atlas_exhausted_repaint_sent = true;
                    }
                }
                if post_fill < 1.0 {
                    self.atlas_exhausted_repaint_sent = false;
                }
                self.last_atlas_fill_ratio = post_fill;
                self.last_atlas_height = post_height;
                needs_repaint
            });
            if needs_repaint {
                ui.ctx().request_repaint();
            }

            // Stage 3: Vector glyphs, underlines, curly paths, prompt gutter, hyperlink underline,
            // underline/bar/hollow cursor.
            let overlay_mesh = Arc::make_mut(&mut self.overlay_mesh);
            overlay_mesh.vertices.clear();
            overlay_mesh.indices.clear();
            self.scratch_paths.clear();

            for (vp_row, slot) in self.slots.iter().enumerate() {
                let row_y = origin.y + vp_row as f32 * cell_h;

                #[cfg(feature = "vector-glyphs")]
                for vcell in &slot.vector_cells {
                    let cell_rect = Rect::from_min_size(
                        pos2(origin.x + f32::from(vcell.col) * cell_w, row_y),
                        Vec2::new(cell_w, cell_h),
                    );
                    tessellate_vector_glyph(overlay_mesh, vcell.ch, cell_rect, vcell.color, ppp);
                }

                for deco in &slot.decorations {
                    let x0 = snap_px(origin.x + f32::from(deco.start_col) * cell_w, ppp);
                    let x1 = snap_px(origin.x + f32::from(deco.end_col) * cell_w, ppp);
                    if x1 <= x0 {
                        continue;
                    }
                    let line_thick = snap_size_at_least_1px((cell_h * 0.07).max(1.0), ppp);

                    if deco.strikethrough {
                        let cy = snap_px(row_y + cell_h * 0.5, ppp);
                        let r = snap_rect(
                            Rect::from_min_max(pos2(x0, cy), pos2(x1, cy + line_thick)),
                            ppp,
                        );
                        if r.is_positive() {
                            overlay_mesh.add_colored_rect(r, deco.fg_color);
                        }
                    }

                    if deco.overline {
                        let top = snap_px(row_y, ppp);
                        let r = snap_rect(
                            Rect::from_min_max(pos2(x0, top), pos2(x1, top + line_thick)),
                            ppp,
                        );
                        if r.is_positive() {
                            overlay_mesh.add_colored_rect(r, deco.fg_color);
                        }
                    }

                    paint_underline(
                        overlay_mesh,
                        &mut self.scratch_paths,
                        deco.underline,
                        x0,
                        x1,
                        row_y,
                        cell_h,
                        line_thick,
                        ppp,
                        deco.underline_color,
                    );
                }
            }

            #[cfg(feature = "shell-integration")]
            crate::widgets::prompt_gutter::paint_prompt_gutter(
                overlay_mesh,
                &snapshot,
                shell_zones,
                origin,
                layout.cell_size,
                ppp,
            );

            #[cfg(feature = "hyperlinks")]
            crate::widgets::hyperlink::paint_hyperlink_underlines(
                overlay_mesh,
                hovered_link_spans,
                origin,
                layout.cell_size,
                ppp,
                default_fg,
            );

            // Cursor overlay (Underline, Bar, or hollow outline when unfocused).
            if cursor_visible && cursor_pos.0 < snapshot.cols && cursor_pos.1 < snapshot.rows {
                let c_rect =
                    cursor_cell_rect(origin, cursor_pos.0, cursor_pos.1, cell_w, cell_h, ppp);
                let cursor_c32 = resolve_cursor_color(&config.theme.scheme, &snapshot);
                if !has_focus {
                    if config.cursor_config.hollow_when_unfocused && c_rect.is_positive() {
                        add_hollow_rect_to_mesh(overlay_mesh, c_rect, ppp, cursor_c32);
                    }
                } else if cursor_blink_on {
                    match cursor_shape {
                        CursorShape::Block => {} // Already drawn behind text in bg_mesh.
                        CursorShape::Underline => {
                            let h = snap_size_at_least_1px(
                                config.cursor_config.underline_height_px,
                                ppp,
                            );
                            let ul_rect = snap_rect(
                                Rect::from_min_max(
                                    pos2(c_rect.min.x, (c_rect.max.y - h).max(c_rect.min.y)),
                                    c_rect.max,
                                ),
                                ppp,
                            );
                            if ul_rect.is_positive() {
                                overlay_mesh.add_colored_rect(ul_rect, cursor_c32);
                            }
                        }
                        CursorShape::Bar => {
                            let w = snap_size_at_least_1px(config.cursor_config.bar_width_px, ppp);
                            let bar_rect = snap_rect(
                                Rect::from_min_max(
                                    c_rect.min,
                                    pos2((c_rect.min.x + w).min(c_rect.max.x), c_rect.max.y),
                                ),
                                ppp,
                            );
                            if bar_rect.is_positive() {
                                overlay_mesh.add_colored_rect(bar_rect, cursor_c32);
                            }
                        }
                    }
                }
            }

            let base_cursor_rect = cursor_cell_rect(
                origin,
                cursor_pos.0.min(layout.cols.saturating_sub(1)),
                cursor_pos.1.min(layout.rows.saturating_sub(1)),
                cell_w,
                cell_h,
                ppp,
            );
            (dirty_count, default_fg, default_bg, base_cursor_rect)
        };

        #[cfg(feature = "metrics")]
        terminal.record_frame_rendered(dirty_rows);
        #[cfg(not(feature = "metrics"))]
        let _ = dirty_rows;

        // Stage 4: Submit batched shapes to clipped painter.
        let painter = ui.painter_at(layout.clip_rect);

        self.scratch_shapes.clear();
        if !self.bg_mesh.vertices.is_empty() {
            self.scratch_shapes
                .push(egui::Shape::Mesh(Arc::clone(&self.bg_mesh)));
        }

        for (vp_row, slot) in self.slots.iter().enumerate() {
            if let Some(galley) = &slot.galley {
                let text_pos = pos2(
                    snap_px(origin.x + f32::from(slot.start_col) * cell_w, ppp),
                    snap_px(origin.y + vp_row as f32 * cell_h, ppp),
                );
                self.scratch_shapes.push(egui::Shape::Text(TextShape::new(
                    text_pos,
                    Arc::clone(galley),
                    default_fg,
                )));
            }
        }

        if !self.overlay_mesh.vertices.is_empty() {
            self.scratch_shapes
                .push(egui::Shape::Mesh(Arc::clone(&self.overlay_mesh)));
        }
        for path in self.scratch_paths.drain(..) {
            self.scratch_shapes.push(egui::Shape::Path(path));
        }

        // Render active IME preedit inline at cursor and compute IME cursor_rect.
        let ime_cursor_rect = if !ime_preedit.text.is_empty() {
            let (preedit_galley, preedit_w) = ui.ctx().fonts_mut(|f| {
                let font_id = FontId::new(layout.effective_font_size, FontFamily::Monospace);
                let g = f.layout_no_wrap(ime_preedit.text.clone(), font_id, default_fg);
                let w = g.size().x.max(cell_w);
                (g, w)
            });
            let max_x = layout.term_rect.max.x;
            let px = ime_cursor_rect_base
                .min
                .x
                .min((max_x - preedit_w).max(origin.x));
            let py = ime_cursor_rect_base.min.y;
            let preedit_rect = snap_rect(
                Rect::from_min_max(
                    pos2(px, py),
                    pos2(
                        (px + preedit_w).min(max_x),
                        (py + cell_h).min(layout.term_rect.max.y),
                    ),
                ),
                ppp,
            );
            if preedit_rect.is_positive() {
                self.scratch_shapes
                    .push(egui::Shape::rect_filled(preedit_rect, 0.0, default_bg));
                let ul_h = snap_size_at_least_1px(1.0, ppp);
                let ul_rect = snap_rect(
                    Rect::from_min_max(
                        pos2(
                            preedit_rect.min.x,
                            (preedit_rect.max.y - ul_h).max(preedit_rect.min.y),
                        ),
                        preedit_rect.max,
                    ),
                    ppp,
                );
                self.scratch_shapes
                    .push(egui::Shape::rect_filled(ul_rect, 0.0, default_fg));
            }
            self.scratch_shapes.push(egui::Shape::Text(TextShape::new(
                preedit_rect.min,
                preedit_galley,
                default_fg,
            )));
            snap_rect(
                Rect::from_min_max(
                    pos2(
                        (preedit_rect.max.x - 1.0 / ppp).max(preedit_rect.min.x),
                        preedit_rect.min.y,
                    ),
                    preedit_rect.max,
                ),
                ppp,
            )
        } else {
            ime_cursor_rect_base
        };

        painter.extend(self.scratch_shapes.drain(..));
        ime_cursor_rect
    }
}

fn snap_slow_galley_clusters(galley: &mut Arc<Galley>, clusters: &[(f32, usize)]) {
    if galley.rows.len() != 1 {
        return;
    }
    let total_glyphs: usize = clusters.iter().map(|&(_, g)| g).sum();
    if galley.rows[0].row.glyphs.len() != total_glyphs {
        return;
    }
    let mut g_idx = 0usize;
    let mut needs_adjust = false;
    {
        let glyphs = &galley.rows[0].row.glyphs;
        for &(target_x, glyph_count) in clusters {
            if glyph_count > 0 && (glyphs[g_idx].pos.x - target_x).abs() > 1e-4 {
                needs_adjust = true;
                break;
            }
            g_idx += glyph_count;
        }
    }
    if !needs_adjust {
        return;
    }

    let galley_mut = Arc::make_mut(galley);
    let placed_row = &mut galley_mut.rows[0];
    let row_mut = Arc::make_mut(&mut placed_row.row);
    let glyph_vertex_end = row_mut.visuals.glyph_vertex_range.end;
    let num_glyphs = row_mut.glyphs.len();
    let mut g_idx = 0usize;
    let mut max_glyph_x = 0.0_f32;
    for &(target_x, glyph_count) in clusters {
        if glyph_count == 0 {
            continue;
        }
        let dx = target_x - row_mut.glyphs[g_idx].pos.x;
        if dx.abs() > 1e-4 {
            for i in g_idx..(g_idx + glyph_count) {
                let v_start = row_mut.glyphs[i].first_vertex as usize;
                let v_end = if i + 1 < num_glyphs {
                    row_mut.glyphs[i + 1].first_vertex as usize
                } else {
                    glyph_vertex_end
                };
                row_mut.glyphs[i].pos.x += dx;
                if v_start < v_end && v_end <= row_mut.visuals.mesh.vertices.len() {
                    for v in &mut row_mut.visuals.mesh.vertices[v_start..v_end] {
                        v.pos.x += dx;
                    }
                }
            }
        }
        for i in g_idx..(g_idx + glyph_count) {
            max_glyph_x = max_glyph_x.max(row_mut.glyphs[i].max_x());
        }
        g_idx += glyph_count;
    }

    let mesh_bounds = row_mut.visuals.mesh.calc_bounds();
    row_mut.visuals.mesh_bounds = mesh_bounds;
    row_mut.size.x = row_mut.size.x.max(max_glyph_x).round_ui();
    galley_mut.mesh_bounds = mesh_bounds.translate(placed_row.pos.to_vec2());
    galley_mut.rect = placed_row.rect().round_ui();
}

#[inline]
#[expect(clippy::too_many_arguments, reason = "cell fg resolver")]
fn resolve_cell_fg(
    cell: egui_xterm_core::Cell,
    col: usize,
    row_cursor_col: Option<u16>,
    cursor_text_c32: Color32,
    sel_fg_override: Option<Color32>,
    row_sel: Option<(usize, usize)>,
    default_fg: Color32,
    default_bg: Color32,
    theme: &Theme,
    snapshot: &RenderSnapshot<'_>,
) -> Color32 {
    let is_hidden = cell.flags.contains(CellFlags::HIDDEN);
    if is_hidden {
        return Color32::TRANSPARENT;
    }
    let is_bold = cell.flags.contains(CellFlags::BOLD);
    let is_dim = cell.flags.contains(CellFlags::DIM);
    let inverse = cell.flags.contains(CellFlags::INVERSE);

    let mut fg_c32 = if row_cursor_col == Some(col as u16) {
        cursor_text_c32
    } else if let (Some(s_fg), Some((s0, s1))) = (sel_fg_override, row_sel)
        && col >= s0
        && col <= s1
    {
        s_fg
    } else if inverse {
        resolve_bg_color(cell.bg, default_bg, theme, snapshot)
    } else {
        resolve_fg_color(cell.fg, default_fg, theme, snapshot, is_bold)
    };

    if is_dim {
        fg_c32 = Color32::from_rgba_unmultiplied(
            ((u16::from(fg_c32.r()) * 2) / 3) as u8,
            ((u16::from(fg_c32.g()) * 2) / 3) as u8,
            ((u16::from(fg_c32.b()) * 2) / 3) as u8,
            fg_c32.a(),
        );
    }
    fg_c32
}

fn ensure_ascii_cache_base(
    cache: &mut AsciiGlyphCache,
    fonts: &mut egui::epaint::FontsView<'_>,
    font_id: &FontId,
    extra_letter_spacing: f32,
) -> bool {
    if cache.base_galley.is_some() {
        return true;
    }
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: f32::INFINITY,
            ..Default::default()
        },
        break_on_newline: false,
        keep_trailing_whitespace: true,
        ..Default::default()
    };
    job.append(
        " ",
        0.0,
        TextFormat {
            font_id: font_id.clone(),
            color: Color32::WHITE,
            extra_letter_spacing,
            ..Default::default()
        },
    );
    let g_arc = fonts.layout_job(job);
    if g_arc.rows.len() != 1 || g_arc.rows[0].row.glyphs.len() != 1 {
        return false;
    }
    cache.row_height = g_arc.rows[0].row.size.y;
    let mut empty_galley = (*g_arc).clone();
    Arc::make_mut(&mut empty_galley.job).clear();
    let row_mut = Arc::make_mut(&mut empty_galley.rows[0].row);
    row_mut.glyphs.clear();
    row_mut.visuals.mesh.vertices.clear();
    row_mut.visuals.mesh.indices.clear();
    cache.base_galley = Some(Arc::new(empty_galley));
    true
}

#[inline]
fn get_or_probe_ascii_entry(
    cache: &mut AsciiGlyphCache,
    fonts: &mut egui::epaint::FontsView<'_>,
    ch: char,
    font_id: &FontId,
    extra_letter_spacing: f32,
) -> Option<AsciiGlyphEntry> {
    let idx = (ch as u32).checked_sub(0x20)? as usize;
    if idx >= 95 || cache.unsupported[idx] {
        return None;
    }
    if let Some(entry) = cache.entries[idx] {
        return Some(entry);
    }

    let mut buf = [0u8; 4];
    let s = ch.encode_utf8(&mut buf);
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: f32::INFINITY,
            ..Default::default()
        },
        break_on_newline: false,
        keep_trailing_whitespace: true,
        ..Default::default()
    };
    job.append(
        s,
        0.0,
        TextFormat {
            font_id: font_id.clone(),
            color: Color32::WHITE,
            italics: false,
            extra_letter_spacing,
            ..Default::default()
        },
    );
    let g = fonts.layout_job(job);
    if g.rows.len() != 1 || g.rows[0].row.glyphs.len() != 1 {
        cache.unsupported[idx] = true;
        return None;
    }
    let row = &g.rows[0].row;
    let glyph = row.glyphs[0];
    if glyph.is_color {
        cache.unsupported[idx] = true;
        return None;
    }
    let mesh = &row.visuals.mesh;
    let entry = if mesh.vertices.is_empty() && mesh.indices.is_empty() {
        AsciiGlyphEntry {
            glyph,
            has_quad: false,
            rect_min: Pos2::ZERO,
            rect_size: Vec2::ZERO,
            uv_min: Pos2::ZERO,
            uv_max: Pos2::ZERO,
        }
    } else if mesh.vertices.len() == 4
        && mesh.indices.len() == 6
        && mesh.texture_id == egui::TextureId::default()
    {
        let rect_min = mesh.vertices[0].pos;
        let rect_max = mesh.vertices[3].pos;
        AsciiGlyphEntry {
            glyph,
            has_quad: true,
            rect_min,
            rect_size: rect_max - rect_min,
            uv_min: mesh.vertices[0].uv,
            uv_max: mesh.vertices[3].uv,
        }
    } else {
        cache.unsupported[idx] = true;
        return None;
    };

    cache.entries[idx] = Some(entry);
    Some(entry)
}

#[expect(clippy::too_many_arguments, reason = "ASCII row galley fast path")]
fn build_ascii_row_galley(
    cache: &mut AsciiGlyphCache,
    fonts: &mut egui::epaint::FontsView<'_>,
    slot_galley: &mut Option<Arc<Galley>>,
    slice: &[egui_xterm_core::Cell],
    start_col: u16,
    row_cursor_col: Option<u16>,
    cursor_text_c32: Color32,
    sel_fg_override: Option<Color32>,
    row_sel: Option<(usize, usize)>,
    default_fg: Color32,
    default_bg: Color32,
    theme: &Theme,
    snapshot: &RenderSnapshot<'_>,
    base_font_id: &FontId,
    extra_letter_spacing: f32,
    cell_w: f32,
    ppp: f32,
) -> bool {
    if slice.is_empty() {
        return false;
    }
    if !ensure_ascii_cache_base(cache, fonts, base_font_id, extra_letter_spacing) {
        return false;
    }
    // Ensure every ASCII character in this row is cached before mutating `slot_galley`.
    for &cell in slice {
        if get_or_probe_ascii_entry(
            cache,
            fonts,
            cell.codepoint,
            base_font_id,
            extra_letter_spacing,
        )
        .is_none()
        {
            return false;
        }
    }

    let Some(base_arc) = cache.base_galley.as_ref() else {
        return false;
    };

    // Reuse the existing slot Galley in-place when exclusively owned (`strong_count == 1`),
    // avoiding all heap allocations across frames.
    let mut reusable_arc = slot_galley.take().filter(|g| {
        Arc::strong_count(g) == 1
            && Arc::weak_count(g) == 0
            && g.rows.len() == 1
            && Arc::strong_count(&g.job) == 1
            && Arc::weak_count(&g.job) == 0
            && Arc::strong_count(&g.rows[0].row) == 1
            && Arc::weak_count(&g.rows[0].row) == 0
    });
    let mut galley_arc = match reusable_arc.take() {
        Some(g) => g,
        None => {
            let mut g = (**base_arc).clone();
            Arc::make_mut(&mut g.job);
            Arc::make_mut(&mut g.rows[0].row);
            Arc::new(g)
        }
    };

    let galley = Arc::get_mut(&mut galley_arc).expect("galley exclusively owned");
    let job = Arc::get_mut(&mut galley.job).expect("job exclusively owned");
    let placed_row = &mut galley.rows[0];
    let row = Arc::get_mut(&mut placed_row.row).expect("row exclusively owned");

    let n = slice.len();
    job.text.clear();
    job.text.reserve(n);
    job.sections.clear();
    row.glyphs.clear();
    row.glyphs.reserve(n);
    let mesh = &mut row.visuals.mesh;
    mesh.vertices.clear();
    mesh.indices.clear();

    let base_format = TextFormat {
        font_id: base_font_id.clone(),
        color: Color32::WHITE,
        italics: false,
        extra_letter_spacing,
        ..Default::default()
    };

    let mut last_style: Option<(Color32, bool)> = None;
    let mut bounds_min_x = f32::INFINITY;
    let mut bounds_min_y = f32::INFINITY;
    let mut bounds_max_x = f32::NEG_INFINITY;
    let mut bounds_max_y = f32::NEG_INFINITY;
    let mut last_advance = cell_w;
    let mut last_x_off = 0.0_f32;

    for (offset, &cell) in slice.iter().enumerate() {
        let col = usize::from(start_col) + offset;
        let is_italic = cell.flags.contains(CellFlags::ITALIC);
        let fg_c32 = resolve_cell_fg(
            cell,
            col,
            row_cursor_col,
            cursor_text_c32,
            sel_fg_override,
            row_sel,
            default_fg,
            default_bg,
            theme,
            snapshot,
        );

        let byte_start = job.text.len();
        job.text.push(cell.codepoint);
        let byte_end = job.text.len();

        let style = (fg_c32, is_italic);
        if last_style == Some(style)
            && let Some(last_sec) = job.sections.last_mut()
        {
            last_sec.byte_range.end = ByteIndex(byte_end);
        } else {
            let mut fmt = base_format.clone();
            fmt.color = fg_c32;
            fmt.italics = is_italic;
            job.sections.push(LayoutSection {
                leading_space: 0.0,
                byte_range: ByteIndex(byte_start)..ByteIndex(byte_end),
                format: fmt,
            });
            last_style = Some(style);
        }

        let idx = (cell.codepoint as usize) - 0x20;
        let entry = cache.entries[idx].expect("verified cached above");
        let x_off = snap_px(offset as f32 * cell_w, ppp);
        last_x_off = x_off;
        last_advance = entry.glyph.advance_width;

        let mut glyph = entry.glyph;
        glyph.pos.x += x_off;
        glyph.first_vertex = mesh.vertices.len() as u32;
        row.glyphs.push(glyph);

        if entry.has_quad {
            let left_top = pos2(x_off + entry.rect_min.x, entry.rect_min.y);
            let right_bottom = left_top + entry.rect_size;
            let v_idx = mesh.vertices.len() as u32;
            mesh.indices.extend_from_slice(&[
                v_idx,
                v_idx + 1,
                v_idx + 2,
                v_idx + 2,
                v_idx + 1,
                v_idx + 3,
            ]);

            let top_dx = if is_italic {
                entry.rect_size.y * 0.25
            } else {
                0.0
            };
            mesh.vertices.extend_from_slice(&[
                Vertex {
                    pos: pos2(left_top.x + top_dx, left_top.y),
                    uv: entry.uv_min,
                    color: fg_c32,
                },
                Vertex {
                    pos: pos2(right_bottom.x + top_dx, left_top.y),
                    uv: pos2(entry.uv_max.x, entry.uv_min.y),
                    color: fg_c32,
                },
                Vertex {
                    pos: pos2(left_top.x, right_bottom.y),
                    uv: pos2(entry.uv_min.x, entry.uv_max.y),
                    color: fg_c32,
                },
                Vertex {
                    pos: right_bottom,
                    uv: entry.uv_max,
                    color: fg_c32,
                },
            ]);

            bounds_min_x = bounds_min_x.min(left_top.x);
            bounds_min_y = bounds_min_y.min(left_top.y);
            bounds_max_x = bounds_max_x.max(right_bottom.x + top_dx);
            bounds_max_y = bounds_max_y.max(right_bottom.y);
        }
    }

    let mesh_bounds = if mesh.vertices.is_empty() {
        Rect::NOTHING
    } else {
        Rect::from_min_max(
            pos2(bounds_min_x, bounds_min_y),
            pos2(bounds_max_x, bounds_max_y),
        )
    };
    row.visuals.mesh_bounds = mesh_bounds;
    row.visuals.glyph_index_start = 0;
    row.visuals.glyph_vertex_range = 0..mesh.vertices.len();
    row.size = Vec2::new(last_x_off + last_advance, cache.row_height).round_ui();

    placed_row.pos = Pos2::ZERO;
    placed_row.ends_with_newline = false;

    galley.elided = false;
    galley.rect = Rect::from_min_size(Pos2::ZERO, row.size).round_ui();
    galley.mesh_bounds = mesh_bounds;
    galley.num_vertices = mesh.vertices.len();
    galley.num_indices = mesh.indices.len();
    galley.pixels_per_point = ppp;

    *slot_galley = Some(galley_arc);
    true
}

#[inline]
fn rgb_to_color32(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

#[inline]
fn resolve_default_pair(
    theme_scheme: &ColorScheme,
    snapshot: &RenderSnapshot<'_>,
) -> (Color32, Color32) {
    let fg_rgb = if snapshot.palette_overrides.foreground {
        snapshot.palette.foreground
    } else {
        theme_scheme.foreground
    };
    let bg_rgb = if snapshot.palette_overrides.background {
        snapshot.palette.background
    } else {
        theme_scheme.background
    };
    if snapshot.modes.reverse_video {
        (rgb_to_color32(bg_rgb), rgb_to_color32(fg_rgb))
    } else {
        (rgb_to_color32(fg_rgb), rgb_to_color32(bg_rgb))
    }
}

#[inline]
fn resolve_cursor_color(theme_scheme: &ColorScheme, snapshot: &RenderSnapshot<'_>) -> Color32 {
    let rgb = if snapshot.palette_overrides.cursor {
        snapshot.palette.cursor
    } else {
        theme_scheme.cursor
    };
    rgb_to_color32(rgb)
}

#[inline]
fn resolve_indexed_rgb(idx: u8, theme: &Theme, snapshot: &RenderSnapshot<'_>) -> [u8; 3] {
    if usize::from(idx) < 16 {
        if snapshot.palette_overrides.ansi[usize::from(idx)] {
            snapshot.palette.ansi[usize::from(idx)]
        } else {
            theme.scheme.ansi[usize::from(idx)]
        }
    } else if let Some(rgb) = snapshot.extended_palette[usize::from(idx - 16)] {
        rgb
    } else {
        ColorScheme::xterm_256_color(idx, &theme.scheme.ansi)
    }
}

#[inline]
fn resolve_fg_color(
    packed: PackedColor,
    default_fg: Color32,
    theme: &Theme,
    snapshot: &RenderSnapshot<'_>,
    is_bold: bool,
) -> Color32 {
    match packed {
        PackedColor::Default => default_fg,
        PackedColor::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        PackedColor::Indexed(idx) => {
            let eff_idx = if is_bold && theme.bold_is_bright && idx < 8 {
                idx + 8
            } else {
                idx
            };
            rgb_to_color32(resolve_indexed_rgb(eff_idx, theme, snapshot))
        }
    }
}

#[inline]
fn resolve_bg_color(
    packed: PackedColor,
    default_bg: Color32,
    theme: &Theme,
    snapshot: &RenderSnapshot<'_>,
) -> Color32 {
    match packed {
        PackedColor::Default => default_bg,
        PackedColor::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        PackedColor::Indexed(idx) => rgb_to_color32(resolve_indexed_rgb(idx, theme, snapshot)),
    }
}

#[cfg(feature = "search")]
fn search_highlight_at(
    matches: &[egui_xterm_core::SearchMatch],
    active: Option<egui_xterm_core::SearchMatch>,
    retained_line: usize,
    col: u16,
) -> Option<Color32> {
    let mut matched = false;
    let mut is_active = false;
    for m in matches {
        if retained_line < m.line || retained_line > m.end_line {
            continue;
        }
        let in_span = if m.line == m.end_line {
            col >= m.start_col && col < m.end_col
        } else if retained_line == m.line {
            col >= m.start_col
        } else if retained_line == m.end_line {
            col < m.end_col
        } else {
            true
        };
        if in_span {
            matched = true;
            if active == Some(*m) {
                is_active = true;
                break;
            }
        }
    }
    if is_active {
        Some(Color32::from_rgb(255, 165, 0))
    } else if matched {
        Some(Color32::from_rgb(130, 100, 30))
    } else {
        None
    }
}

fn cursor_cell_rect(origin: Pos2, col: u16, row: u16, cell_w: f32, cell_h: f32, ppp: f32) -> Rect {
    let min_x = snap_px(origin.x + f32::from(col) * cell_w, ppp);
    let min_y = snap_px(origin.y + f32::from(row) * cell_h, ppp);
    let max_x = snap_px(origin.x + f32::from(col + 1) * cell_w, ppp);
    let max_y = snap_px(origin.y + f32::from(row + 1) * cell_h, ppp);
    snap_rect(
        Rect::from_min_max(pos2(min_x, min_y), pos2(max_x, max_y)),
        ppp,
    )
}

fn add_hollow_rect_to_mesh(mesh: &mut Mesh, rect: Rect, ppp: f32, color: Color32) {
    let t = snap_size_at_least_1px(1.0, ppp);
    let top = snap_rect(
        Rect::from_min_max(rect.min, pos2(rect.max.x, (rect.min.y + t).min(rect.max.y))),
        ppp,
    );
    let bottom = snap_rect(
        Rect::from_min_max(pos2(rect.min.x, (rect.max.y - t).max(rect.min.y)), rect.max),
        ppp,
    );
    let left = snap_rect(
        Rect::from_min_max(rect.min, pos2((rect.min.x + t).min(rect.max.x), rect.max.y)),
        ppp,
    );
    let right = snap_rect(
        Rect::from_min_max(pos2((rect.max.x - t).max(rect.min.x), rect.min.y), rect.max),
        ppp,
    );
    for edge in [top, bottom, left, right] {
        if edge.is_positive() {
            mesh.add_colored_rect(edge, color);
        }
    }
}

#[expect(clippy::too_many_arguments, reason = "underline geometry helper")]
fn paint_underline(
    overlay_mesh: &mut Mesh,
    paths: &mut Vec<PathShape>,
    style: UnderlineStyle,
    x0: f32,
    x1: f32,
    row_y: f32,
    cell_h: f32,
    thick: f32,
    ppp: f32,
    color: Color32,
) {
    let y_bot = snap_px(row_y + cell_h, ppp);
    let y_base = snap_px((y_bot - thick * 1.5).max(row_y), ppp);

    match style {
        UnderlineStyle::None => {}
        UnderlineStyle::Single => {
            let r = snap_rect(
                Rect::from_min_max(pos2(x0, y_base), pos2(x1, (y_base + thick).min(y_bot))),
                ppp,
            );
            if r.is_positive() {
                overlay_mesh.add_colored_rect(r, color);
            }
        }
        UnderlineStyle::Double => {
            let y1 = snap_px((y_bot - thick).max(row_y), ppp);
            let y0 = snap_px((y1 - thick * 2.0).max(row_y), ppp);
            let r0 = snap_rect(
                Rect::from_min_max(pos2(x0, y0), pos2(x1, (y0 + thick).min(y1))),
                ppp,
            );
            let r1 = snap_rect(
                Rect::from_min_max(pos2(x0, y1), pos2(x1, (y1 + thick).min(y_bot))),
                ppp,
            );
            if r0.is_positive() {
                overlay_mesh.add_colored_rect(r0, color);
            }
            if r1.is_positive() {
                overlay_mesh.add_colored_rect(r1, color);
            }
        }
        UnderlineStyle::Dotted => {
            let step = snap_size_at_least_1px(thick * 2.0, ppp);
            let mut x = x0;
            while x < x1 {
                let dx1 = snap_px((x + thick).min(x1), ppp);
                let r = snap_rect(
                    Rect::from_min_max(pos2(x, y_base), pos2(dx1, (y_base + thick).min(y_bot))),
                    ppp,
                );
                if r.is_positive() {
                    overlay_mesh.add_colored_rect(r, color);
                }
                x += step;
            }
        }
        UnderlineStyle::Dashed => {
            let dash = snap_size_at_least_1px(thick * 4.0, ppp);
            let gap = snap_size_at_least_1px(thick * 2.0, ppp);
            let mut x = x0;
            while x < x1 {
                let dx1 = snap_px((x + dash).min(x1), ppp);
                let r = snap_rect(
                    Rect::from_min_max(pos2(x, y_base), pos2(dx1, (y_base + thick).min(y_bot))),
                    ppp,
                );
                if r.is_positive() {
                    overlay_mesh.add_colored_rect(r, color);
                }
                x += dash + gap;
            }
        }
        UnderlineStyle::Curly => {
            let amp = (cell_h * 0.08).max(1.0);
            let wave_len = (cell_h * 0.35).max(4.0);
            let steps = (((x1 - x0) / (wave_len * 0.25)).ceil() as usize).clamp(4, 512);
            let mut points = Vec::with_capacity(steps + 1);
            let mid_y = (y_bot - amp - thick * 0.5).max(row_y);
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let x = x0 + (x1 - x0) * t;
                let phase = ((x - x0) / wave_len) * std::f32::consts::TAU;
                let y = mid_y + phase.sin() * amp;
                points.push(pos2(x, y));
            }
            paths.push(PathShape::line(points, Stroke::new(thick, color)));
        }
    }
}

fn hash_theme(theme: &Theme) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    theme.bold_is_bright.hash(&mut h);
    hash_scheme_into(&theme.scheme, &mut h);
    h.finish()
}

fn hash_palette(snapshot: &RenderSnapshot<'_>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash_scheme_into(snapshot.palette, &mut h);
    snapshot.palette_overrides.foreground.hash(&mut h);
    snapshot.palette_overrides.background.hash(&mut h);
    snapshot.palette_overrides.cursor.hash(&mut h);
    snapshot.palette_overrides.ansi.hash(&mut h);
    snapshot.extended_palette.hash(&mut h);
    h.finish()
}

fn hash_scheme_into(scheme: &ColorScheme, h: &mut impl Hasher) {
    scheme.foreground.hash(h);
    scheme.background.hash(h);
    scheme.cursor.hash(h);
    scheme.cursor_text.hash(h);
    scheme.selection_bg.hash(h);
    scheme.selection_fg.hash(h);
    scheme.ansi.hash(h);
}

#[cfg(feature = "vector-glyphs")]
#[inline]
const fn is_vector_glyph(ch: char) -> bool {
    matches!(
        ch,
        '\u{2500}'..='\u{257F}'
            | '\u{2580}'..='\u{259F}'
            | '\u{2800}'..='\u{28FF}'
            | '\u{E0B0}'..='\u{E0D4}'
    )
}

#[cfg(feature = "vector-glyphs")]
fn tessellate_vector_glyph(mesh: &mut Mesh, ch: char, rect: Rect, color: Color32, ppp: f32) {
    let r = snap_rect(rect, ppp);
    if !r.is_positive() {
        return;
    }
    match ch {
        '\u{2580}'..='\u{259F}' => tessellate_block_element(mesh, ch, r, color, ppp),
        '\u{2800}'..='\u{28FF}' => tessellate_braille(mesh, ch, r, color, ppp),
        '\u{2500}'..='\u{257F}' => tessellate_box_drawing(mesh, ch, r, color, ppp),
        '\u{E0B0}'..='\u{E0D4}' => tessellate_powerline(mesh, ch, r, color, ppp),
        _ => {}
    }
}

#[cfg(feature = "vector-glyphs")]
fn tessellate_block_element(mesh: &mut Mesh, ch: char, r: Rect, color: Color32, ppp: f32) {
    let add = |mesh: &mut Mesh, fx0: f32, fy0: f32, fx1: f32, fy1: f32, c: Color32| {
        let sub = snap_rect(
            Rect::from_min_max(
                pos2(r.min.x + r.width() * fx0, r.min.y + r.height() * fy0),
                pos2(r.min.x + r.width() * fx1, r.min.y + r.height() * fy1),
            ),
            ppp,
        );
        if sub.is_positive() {
            mesh.add_colored_rect(sub, c);
        }
    };

    match ch {
        '\u{2580}' => add(mesh, 0.0, 0.0, 1.0, 0.5, color),
        '\u{2581}' => add(mesh, 0.0, 0.875, 1.0, 1.0, color),
        '\u{2582}' => add(mesh, 0.0, 0.75, 1.0, 1.0, color),
        '\u{2583}' => add(mesh, 0.0, 0.625, 1.0, 1.0, color),
        '\u{2584}' => add(mesh, 0.0, 0.5, 1.0, 1.0, color),
        '\u{2585}' => add(mesh, 0.0, 0.375, 1.0, 1.0, color),
        '\u{2586}' => add(mesh, 0.0, 0.25, 1.0, 1.0, color),
        '\u{2587}' => add(mesh, 0.0, 0.125, 1.0, 1.0, color),
        '\u{2588}' => add(mesh, 0.0, 0.0, 1.0, 1.0, color),
        '\u{2589}' => add(mesh, 0.0, 0.0, 0.875, 1.0, color),
        '\u{258A}' => add(mesh, 0.0, 0.0, 0.75, 1.0, color),
        '\u{258B}' => add(mesh, 0.0, 0.0, 0.625, 1.0, color),
        '\u{258C}' => add(mesh, 0.0, 0.0, 0.5, 1.0, color),
        '\u{258D}' => add(mesh, 0.0, 0.0, 0.375, 1.0, color),
        '\u{258E}' => add(mesh, 0.0, 0.0, 0.25, 1.0, color),
        '\u{258F}' => add(mesh, 0.0, 0.0, 0.125, 1.0, color),
        '\u{2590}' => add(mesh, 0.5, 0.0, 1.0, 1.0, color),
        '\u{2591}' => {
            let c = Color32::from_rgba_unmultiplied(
                color.r(),
                color.g(),
                color.b(),
                (u16::from(color.a()) / 4) as u8,
            );
            add(mesh, 0.0, 0.0, 1.0, 1.0, c);
        }
        '\u{2592}' => {
            let c = Color32::from_rgba_unmultiplied(
                color.r(),
                color.g(),
                color.b(),
                (u16::from(color.a()) / 2) as u8,
            );
            add(mesh, 0.0, 0.0, 1.0, 1.0, c);
        }
        '\u{2593}' => {
            let c = Color32::from_rgba_unmultiplied(
                color.r(),
                color.g(),
                color.b(),
                ((u16::from(color.a()) * 3) / 4) as u8,
            );
            add(mesh, 0.0, 0.0, 1.0, 1.0, c);
        }
        '\u{2594}' => add(mesh, 0.0, 0.0, 1.0, 0.125, color),
        '\u{2595}' => add(mesh, 0.875, 0.0, 1.0, 1.0, color),
        // Quadrant blocks U+2596..=U+259F: bitflags (BL, BR, TL, TR)
        '\u{2596}' => add(mesh, 0.0, 0.5, 0.5, 1.0, color),
        '\u{2597}' => add(mesh, 0.5, 0.5, 1.0, 1.0, color),
        '\u{2598}' => add(mesh, 0.0, 0.0, 0.5, 0.5, color),
        '\u{2599}' => {
            add(mesh, 0.0, 0.0, 0.5, 1.0, color);
            add(mesh, 0.5, 0.5, 1.0, 1.0, color);
        }
        '\u{259A}' => {
            add(mesh, 0.0, 0.0, 0.5, 0.5, color);
            add(mesh, 0.5, 0.5, 1.0, 1.0, color);
        }
        '\u{259B}' => {
            add(mesh, 0.0, 0.0, 1.0, 0.5, color);
            add(mesh, 0.0, 0.5, 0.5, 1.0, color);
        }
        '\u{259C}' => {
            add(mesh, 0.0, 0.0, 1.0, 0.5, color);
            add(mesh, 0.5, 0.5, 1.0, 1.0, color);
        }
        '\u{259D}' => add(mesh, 0.5, 0.0, 1.0, 0.5, color),
        '\u{259E}' => {
            add(mesh, 0.5, 0.0, 1.0, 0.5, color);
            add(mesh, 0.0, 0.5, 0.5, 1.0, color);
        }
        '\u{259F}' => {
            add(mesh, 0.5, 0.0, 1.0, 0.5, color);
            add(mesh, 0.0, 0.5, 1.0, 1.0, color);
        }
        _ => {}
    }
}

#[cfg(feature = "vector-glyphs")]
fn tessellate_braille(mesh: &mut Mesh, ch: char, r: Rect, color: Color32, ppp: f32) {
    let bits = (ch as u32) - 0x2800;
    if bits == 0 {
        return;
    }
    // Unicode Braille dot positions: (col 0..=1, row 0..=3)
    const DOTS: [(u32, f32, f32); 8] = [
        (0, 0.25, 0.125), // dot 1
        (1, 0.25, 0.375), // dot 2
        (2, 0.25, 0.625), // dot 3
        (3, 0.75, 0.125), // dot 4
        (4, 0.75, 0.375), // dot 5
        (5, 0.75, 0.625), // dot 6
        (6, 0.25, 0.875), // dot 7
        (7, 0.75, 0.875), // dot 8
    ];
    let dot_w = snap_size_at_least_1px((r.width() * 0.26).max(1.0), ppp);
    let dot_h = snap_size_at_least_1px((r.height() * 0.13).max(1.0), ppp);
    for (bit, fx, fy) in DOTS {
        if (bits & (1 << bit)) != 0 {
            let cx = r.min.x + r.width() * fx;
            let cy = r.min.y + r.height() * fy;
            let dot = snap_rect(
                Rect::from_center_size(pos2(cx, cy), Vec2::new(dot_w, dot_h)),
                ppp,
            );
            if dot.is_positive() {
                mesh.add_colored_rect(dot, color);
            }
        }
    }
}

#[cfg(feature = "vector-glyphs")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum LineWeight {
    None,
    Light,
    Heavy,
    Double,
}

#[cfg(feature = "vector-glyphs")]
fn tessellate_box_drawing(mesh: &mut Mesh, ch: char, r: Rect, color: Color32, ppp: f32) {
    let light = snap_size_at_least_1px((r.width() * 0.14).max(1.0), ppp);
    let heavy = snap_size_at_least_1px(light * 2.0, ppp);
    let cx = snap_px(r.center().x - light * 0.5, ppp);
    let cy = snap_px(r.center().y - light * 0.5, ppp);

    // Handle diagonals and rounded corners first.
    match ch {
        '\u{2571}' => {
            // Diagonal upper-right to lower-left
            add_quad(
                mesh,
                pos2(r.max.x - light * 0.5, r.min.y),
                pos2(r.max.x, r.min.y + light * 0.5),
                pos2(r.min.x + light * 0.5, r.max.y),
                pos2(r.min.x, r.max.y - light * 0.5),
                color,
            );
            return;
        }
        '\u{2572}' => {
            // Diagonal upper-left to lower-right
            add_quad(
                mesh,
                pos2(r.min.x, r.min.y + light * 0.5),
                pos2(r.min.x + light * 0.5, r.min.y),
                pos2(r.max.x, r.max.y - light * 0.5),
                pos2(r.max.x - light * 0.5, r.max.y),
                color,
            );
            return;
        }
        '\u{2573}' => {
            tessellate_box_drawing(mesh, '\u{2571}', r, color, ppp);
            tessellate_box_drawing(mesh, '\u{2572}', r, color, ppp);
            return;
        }
        '\u{256D}' => {
            // Arc down and right
            draw_box_arms(
                mesh,
                r,
                cx,
                cy,
                light,
                heavy,
                ppp,
                color,
                LineWeight::None,
                LineWeight::Light,
                LineWeight::None,
                LineWeight::Light,
            );
            return;
        }
        '\u{256E}' => {
            // Arc down and left
            draw_box_arms(
                mesh,
                r,
                cx,
                cy,
                light,
                heavy,
                ppp,
                color,
                LineWeight::Light,
                LineWeight::None,
                LineWeight::None,
                LineWeight::Light,
            );
            return;
        }
        '\u{256F}' => {
            // Arc up and left
            draw_box_arms(
                mesh,
                r,
                cx,
                cy,
                light,
                heavy,
                ppp,
                color,
                LineWeight::Light,
                LineWeight::None,
                LineWeight::Light,
                LineWeight::None,
            );
            return;
        }
        '\u{2570}' => {
            // Arc up and right
            draw_box_arms(
                mesh,
                r,
                cx,
                cy,
                light,
                heavy,
                ppp,
                color,
                LineWeight::None,
                LineWeight::Light,
                LineWeight::Light,
                LineWeight::None,
            );
            return;
        }
        _ => {}
    }

    // Dashed horizontal / vertical lines.
    if matches!(
        ch,
        '\u{2504}' | '\u{2505}' | '\u{2508}' | '\u{2509}' | '\u{254C}' | '\u{254D}'
    ) {
        let thick = if matches!(ch, '\u{2505}' | '\u{2509}' | '\u{254D}') {
            heavy
        } else {
            light
        };
        let count = match ch {
            '\u{254C}' | '\u{254D}' => 2,
            '\u{2504}' | '\u{2505}' => 3,
            _ => 4,
        };
        let seg_w = r.width() / (count as f32 * 2.0);
        for i in 0..count {
            let x0 = snap_px(r.min.x + (i as f32 * 2.0 + 0.25) * seg_w, ppp);
            let x1 = snap_px((x0 + seg_w).min(r.max.x), ppp);
            let dash = snap_rect(Rect::from_min_max(pos2(x0, cy), pos2(x1, cy + thick)), ppp);
            if dash.is_positive() {
                mesh.add_colored_rect(dash, color);
            }
        }
        return;
    }
    if matches!(
        ch,
        '\u{2506}' | '\u{2507}' | '\u{250A}' | '\u{250B}' | '\u{254E}' | '\u{254F}'
    ) {
        let thick = if matches!(ch, '\u{2507}' | '\u{250B}' | '\u{254F}') {
            heavy
        } else {
            light
        };
        let count = match ch {
            '\u{254E}' | '\u{254F}' => 2,
            '\u{2506}' | '\u{2507}' => 3,
            _ => 4,
        };
        let seg_h = r.height() / (count as f32 * 2.0);
        for i in 0..count {
            let y0 = snap_px(r.min.y + (i as f32 * 2.0 + 0.25) * seg_h, ppp);
            let y1 = snap_px((y0 + seg_h).min(r.max.y), ppp);
            let dash = snap_rect(Rect::from_min_max(pos2(cx, y0), pos2(cx + thick, y1)), ppp);
            if dash.is_positive() {
                mesh.add_colored_rect(dash, color);
            }
        }
        return;
    }

    let (left, right, up, down) = box_arms_for_char(ch);
    draw_box_arms(
        mesh, r, cx, cy, light, heavy, ppp, color, left, right, up, down,
    );
}

#[cfg(feature = "vector-glyphs")]
#[expect(clippy::too_many_arguments, reason = "4-arm box drawing primitive")]
fn draw_box_arms(
    mesh: &mut Mesh,
    r: Rect,
    cx: f32,
    cy: f32,
    light: f32,
    heavy: f32,
    ppp: f32,
    color: Color32,
    left: LineWeight,
    right: LineWeight,
    up: LineWeight,
    down: LineWeight,
) {
    let gap = light;
    let draw_h = |mesh: &mut Mesh, x0: f32, x1: f32, w: LineWeight| match w {
        LineWeight::None => {}
        LineWeight::Light => {
            let b = snap_rect(Rect::from_min_max(pos2(x0, cy), pos2(x1, cy + light)), ppp);
            if b.is_positive() {
                mesh.add_colored_rect(b, color);
            }
        }
        LineWeight::Heavy => {
            let top = snap_px(cy - (heavy - light) * 0.5, ppp);
            let b = snap_rect(
                Rect::from_min_max(pos2(x0, top), pos2(x1, top + heavy)),
                ppp,
            );
            if b.is_positive() {
                mesh.add_colored_rect(b, color);
            }
        }
        LineWeight::Double => {
            let top1 = snap_px(cy - gap, ppp);
            let top2 = snap_px(cy + gap, ppp);
            let b1 = snap_rect(
                Rect::from_min_max(pos2(x0, top1), pos2(x1, top1 + light)),
                ppp,
            );
            let b2 = snap_rect(
                Rect::from_min_max(pos2(x0, top2), pos2(x1, top2 + light)),
                ppp,
            );
            if b1.is_positive() {
                mesh.add_colored_rect(b1, color);
            }
            if b2.is_positive() {
                mesh.add_colored_rect(b2, color);
            }
        }
    };

    let draw_v = |mesh: &mut Mesh, y0: f32, y1: f32, w: LineWeight| match w {
        LineWeight::None => {}
        LineWeight::Light => {
            let b = snap_rect(Rect::from_min_max(pos2(cx, y0), pos2(cx + light, y1)), ppp);
            if b.is_positive() {
                mesh.add_colored_rect(b, color);
            }
        }
        LineWeight::Heavy => {
            let left_x = snap_px(cx - (heavy - light) * 0.5, ppp);
            let b = snap_rect(
                Rect::from_min_max(pos2(left_x, y0), pos2(left_x + heavy, y1)),
                ppp,
            );
            if b.is_positive() {
                mesh.add_colored_rect(b, color);
            }
        }
        LineWeight::Double => {
            let lx1 = snap_px(cx - gap, ppp);
            let lx2 = snap_px(cx + gap, ppp);
            let b1 = snap_rect(
                Rect::from_min_max(pos2(lx1, y0), pos2(lx1 + light, y1)),
                ppp,
            );
            let b2 = snap_rect(
                Rect::from_min_max(pos2(lx2, y0), pos2(lx2 + light, y1)),
                ppp,
            );
            if b1.is_positive() {
                mesh.add_colored_rect(b1, color);
            }
            if b2.is_positive() {
                mesh.add_colored_rect(b2, color);
            }
        }
    };

    let mid_x_end = snap_px(cx + light, ppp);
    let mid_y_end = snap_px(cy + light, ppp);
    draw_h(mesh, r.min.x, mid_x_end, left);
    draw_h(mesh, cx, r.max.x, right);
    draw_v(mesh, r.min.y, mid_y_end, up);
    draw_v(mesh, cy, r.max.y, down);
}

#[cfg(feature = "vector-glyphs")]
const fn box_arms_for_char(ch: char) -> (LineWeight, LineWeight, LineWeight, LineWeight) {
    use LineWeight::{Double as D, Heavy as H, Light as L, None as N};
    match ch {
        '\u{2500}' => (L, L, N, N),
        '\u{2501}' => (H, H, N, N),
        '\u{2502}' => (N, N, L, L),
        '\u{2503}' => (N, N, H, H),
        '\u{250C}' => (N, L, N, L),
        '\u{250D}' => (N, H, N, L),
        '\u{250E}' => (N, L, N, H),
        '\u{250F}' => (N, H, N, H),
        '\u{2510}' => (L, N, N, L),
        '\u{2511}' => (H, N, N, L),
        '\u{2512}' => (L, N, N, H),
        '\u{2513}' => (H, N, N, H),
        '\u{2514}' => (N, L, L, N),
        '\u{2515}' => (N, H, L, N),
        '\u{2516}' => (N, L, H, N),
        '\u{2517}' => (N, H, H, N),
        '\u{2518}' => (L, N, L, N),
        '\u{2519}' => (H, N, L, N),
        '\u{251A}' => (L, N, H, N),
        '\u{251B}' => (H, N, H, N),
        '\u{251C}' => (N, L, L, L),
        '\u{251D}' => (N, H, L, L),
        '\u{251E}' => (N, L, H, L),
        '\u{251F}' => (N, L, L, H),
        '\u{2520}' => (N, L, H, H),
        '\u{2521}' => (N, H, H, L),
        '\u{2522}' => (N, H, L, H),
        '\u{2523}' => (N, H, H, H),
        '\u{2524}' => (L, N, L, L),
        '\u{2525}' => (H, N, L, L),
        '\u{2526}' => (L, N, H, L),
        '\u{2527}' => (L, N, L, H),
        '\u{2528}' => (L, N, H, H),
        '\u{2529}' => (H, N, H, L),
        '\u{252A}' => (H, N, L, H),
        '\u{252B}' => (H, N, H, H),
        '\u{252C}' => (L, L, N, L),
        '\u{252D}' => (H, L, N, L),
        '\u{252E}' => (L, H, N, L),
        '\u{252F}' => (H, H, N, L),
        '\u{2530}' => (L, L, N, H),
        '\u{2531}' => (H, L, N, H),
        '\u{2532}' => (L, H, N, H),
        '\u{2533}' => (H, H, N, H),
        '\u{2534}' => (L, L, L, N),
        '\u{2535}' => (H, L, L, N),
        '\u{2536}' => (L, H, L, N),
        '\u{2537}' => (H, H, L, N),
        '\u{2538}' => (L, L, H, N),
        '\u{2539}' => (H, L, H, N),
        '\u{253A}' => (L, H, H, N),
        '\u{253B}' => (H, H, H, N),
        '\u{253C}' => (L, L, L, L),
        '\u{253D}' => (H, L, L, L),
        '\u{253E}' => (L, H, L, L),
        '\u{253F}' => (H, H, L, L),
        '\u{2540}' => (L, L, H, L),
        '\u{2541}' => (L, L, L, H),
        '\u{2542}' => (L, L, H, H),
        '\u{2543}' => (H, L, H, L),
        '\u{2544}' => (L, H, H, L),
        '\u{2545}' => (H, L, L, H),
        '\u{2546}' => (L, H, L, H),
        '\u{2547}' => (H, H, H, L),
        '\u{2548}' => (H, H, L, H),
        '\u{2549}' => (H, L, H, H),
        '\u{254A}' => (L, H, H, H),
        '\u{254B}' => (H, H, H, H),
        '\u{2550}' => (D, D, N, N),
        '\u{2551}' => (N, N, D, D),
        '\u{2552}' => (N, D, N, L),
        '\u{2553}' => (N, L, N, D),
        '\u{2554}' => (N, D, N, D),
        '\u{2555}' => (D, N, N, L),
        '\u{2556}' => (L, N, N, D),
        '\u{2557}' => (D, N, N, D),
        '\u{2558}' => (N, D, L, N),
        '\u{2559}' => (N, L, D, N),
        '\u{255A}' => (N, D, D, N),
        '\u{255B}' => (D, N, L, N),
        '\u{255C}' => (L, N, D, N),
        '\u{255D}' => (D, N, D, N),
        '\u{255E}' => (N, D, L, L),
        '\u{255F}' => (N, L, D, D),
        '\u{2560}' => (N, D, D, D),
        '\u{2561}' => (D, N, L, L),
        '\u{2562}' => (L, N, D, D),
        '\u{2563}' => (D, N, D, D),
        '\u{2564}' => (D, D, N, L),
        '\u{2565}' => (L, L, N, D),
        '\u{2566}' => (D, D, N, D),
        '\u{2567}' => (D, D, L, N),
        '\u{2568}' => (L, L, D, N),
        '\u{2569}' => (D, D, D, N),
        '\u{256A}' => (D, D, L, L),
        '\u{256B}' => (L, L, D, D),
        '\u{256C}' => (D, D, D, D),
        '\u{2574}' => (L, N, N, N),
        '\u{2575}' => (N, N, L, N),
        '\u{2576}' => (N, L, N, N),
        '\u{2577}' => (N, N, N, L),
        '\u{2578}' => (H, N, N, N),
        '\u{2579}' => (N, N, H, N),
        '\u{257A}' => (N, H, N, N),
        '\u{257B}' => (N, N, N, H),
        '\u{257C}' => (L, H, N, N),
        '\u{257D}' => (N, N, L, H),
        '\u{257E}' => (H, L, N, N),
        '\u{257F}' => (N, N, H, L),
        _ => (N, N, N, N),
    }
}

#[cfg(feature = "vector-glyphs")]
fn tessellate_powerline(mesh: &mut Mesh, ch: char, r: Rect, color: Color32, ppp: f32) {
    let mid_y = snap_px(r.center().y, ppp);
    match ch {
        '\u{E0B0}' | '\u{E0B4}' => {
            // Solid right-pointing triangle (or rounded divider)
            add_tri(
                mesh,
                r.left_top(),
                pos2(r.max.x, mid_y),
                r.left_bottom(),
                color,
            );
        }
        '\u{E0B2}' | '\u{E0B6}' => {
            // Solid left-pointing triangle
            add_tri(
                mesh,
                r.right_top(),
                r.right_bottom(),
                pos2(r.min.x, mid_y),
                color,
            );
        }
        '\u{E0B1}' | '\u{E0B5}' => {
            // Thin right-pointing chevron
            let t = snap_size_at_least_1px((r.width() * 0.14).max(1.0), ppp);
            add_quad(
                mesh,
                r.left_top(),
                pos2(r.min.x + t, r.min.y),
                pos2(r.max.x, mid_y),
                pos2(r.max.x - t, mid_y),
                color,
            );
            add_quad(
                mesh,
                pos2(r.max.x - t, mid_y),
                pos2(r.max.x, mid_y),
                pos2(r.min.x + t, r.max.y),
                r.left_bottom(),
                color,
            );
        }
        '\u{E0B3}' | '\u{E0B7}' => {
            // Thin left-pointing chevron
            let t = snap_size_at_least_1px((r.width() * 0.14).max(1.0), ppp);
            add_quad(
                mesh,
                pos2(r.max.x - t, r.min.y),
                r.right_top(),
                pos2(r.min.x + t, mid_y),
                pos2(r.min.x, mid_y),
                color,
            );
            add_quad(
                mesh,
                pos2(r.min.x, mid_y),
                pos2(r.min.x + t, mid_y),
                r.right_bottom(),
                pos2(r.max.x - t, r.max.y),
                color,
            );
        }
        '\u{E0B8}' | '\u{E0BC}' => {
            add_tri(mesh, r.left_top(), r.right_bottom(), r.left_bottom(), color);
        }
        '\u{E0BA}' | '\u{E0BE}' => {
            add_tri(
                mesh,
                r.right_top(),
                r.right_bottom(),
                r.left_bottom(),
                color,
            );
        }
        _ => {
            // Fallback subtle bar for other U+E0B0..E0D4 glyphs
            let t = snap_size_at_least_1px((r.width() * 0.2).max(1.0), ppp);
            let b = snap_rect(
                Rect::from_min_max(r.min, pos2((r.min.x + t).min(r.max.x), r.max.y)),
                ppp,
            );
            if b.is_positive() {
                mesh.add_colored_rect(b, color);
            }
        }
    }
}

#[cfg(feature = "vector-glyphs")]
fn add_tri(mesh: &mut Mesh, a: Pos2, b: Pos2, c: Pos2, color: Color32) {
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(a, color);
    mesh.colored_vertex(b, color);
    mesh.colored_vertex(c, color);
    mesh.add_triangle(base, base + 1, base + 2);
}

#[cfg(feature = "vector-glyphs")]
fn add_quad(mesh: &mut Mesh, a: Pos2, b: Pos2, c: Pos2, d: Pos2, color: Color32) {
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(a, color);
    mesh.colored_vertex(b, color);
    mesh.colored_vertex(c, color);
    mesh.colored_vertex(d, color);
    mesh.add_triangle(base, base + 1, base + 2);
    mesh.add_triangle(base, base + 2, base + 3);
}
