//! Terminal grid storage, scrollback ring buffer, row pooling, dirty tracking, and reflow.

pub mod extras;
pub mod reflow;
pub mod row;

use std::collections::VecDeque;

use crate::cell::{Cell, CellExtra, CellFlags};
use crate::color::PackedColor;

pub use self::extras::CellExtraTable;
pub use self::reflow::{ReflowAnchors, ReflowResult};
pub use self::row::Row;

/// Maximum number of evicted rows retained in the primary screen's free-list pool.
pub const ROW_POOL_CAP: usize = 64;

/// Which screen buffer a [`Grid`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ScreenKind {
    /// Primary screen with scrollback ring buffer and resize reflow.
    Primary,
    /// Alternate screen with fixed viewport height and no resize reflow.
    Alternate,
}

/// Monotonic 0-based absolute line identifier across retained history and evicted lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AbsLine(pub u64);

/// Absolute grid coordinate pairing an [`AbsLine`] with a 0-based column index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GridPoint {
    /// Absolute line number.
    pub line: AbsLine,
    /// Zero-based column index.
    pub col: usize,
}

impl GridPoint {
    /// Creates a new [`GridPoint`].
    #[must_use]
    pub const fn new(line: AbsLine, col: usize) -> Self {
        Self { line, col }
    }
}

/// Zero-based live viewport coordinate `(row, col)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ViewportPos {
    /// Zero-based live viewport row (`0..rows`).
    pub row: usize,
    /// Zero-based column (`0..cols`).
    pub col: usize,
}

/// Per-viewport-row damage tracker used by incremental renderers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirtyTracker {
    full_damage: bool,
    dirty_viewport_rows: Vec<bool>,
}

impl DirtyTracker {
    /// Creates a [`DirtyTracker`] for `viewport_rows` rows, initially marked dirty.
    #[must_use]
    pub fn new(viewport_rows: usize) -> Self {
        Self {
            full_damage: true,
            dirty_viewport_rows: vec![true; viewport_rows],
        }
    }

    /// Returns `true` if the entire viewport must be treated as damaged (e.g., scroll or resize).
    #[must_use]
    pub const fn is_full_damage(&self) -> bool {
        self.full_damage
    }

    /// Returns the per-viewport-row dirty flags.
    #[must_use]
    pub fn dirty_rows(&self) -> &[bool] {
        &self.dirty_viewport_rows
    }

    /// Returns `true` if `vp_row` is damaged (either individually or via `full_damage`).
    #[must_use]
    pub fn is_row_dirty(&self, vp_row: usize) -> bool {
        self.full_damage
            || self
                .dirty_viewport_rows
                .get(vp_row)
                .copied()
                .unwrap_or(false)
    }

    /// Marks the entire viewport as dirty.
    pub fn mark_all(&mut self) {
        self.full_damage = true;
        self.dirty_viewport_rows.fill(true);
    }

    /// Marks a single live viewport row as dirty.
    pub fn mark_row(&mut self, vp_row: usize) {
        if let Some(slot) = self.dirty_viewport_rows.get_mut(vp_row) {
            *slot = true;
        }
    }

    /// Clears all dirty flags after a renderer consumes the frame.
    pub fn clear(&mut self) {
        self.full_damage = false;
        self.dirty_viewport_rows.fill(false);
    }

    /// Resizes the tracked viewport height and marks all rows dirty.
    pub fn resize(&mut self, viewport_rows: usize) {
        self.dirty_viewport_rows.resize(viewport_rows, true);
        self.mark_all();
    }
}

/// Serializable snapshot of a [`Grid`]'s retained rows, extras, and epoch state.
#[cfg(feature = "serde")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct GridSnapshot {
    pub(crate) cols: usize,
    pub(crate) viewport_rows: usize,
    pub(crate) rows: Vec<Row>,
    pub(crate) extras: CellExtraTable,
    pub(crate) display_offset: usize,
    pub(crate) total_evicted: u64,
    pub(crate) damage_epoch: u64,
    pub(crate) content_epoch: u64,
}

/// Terminal screen buffer holding scrollback history, visible rows, row pool, and sparse extras.
#[derive(Debug, Clone)]
pub struct Grid {
    kind: ScreenKind,
    cols: usize,
    viewport_rows: usize,
    scrollback_limit: usize,
    rows: VecDeque<Row>,
    row_pool: Vec<Row>,
    row_pool_cap: usize,
    extras: CellExtraTable,
    display_offset: usize,
    total_evicted: u64,
    damage_epoch: u64,
    content_epoch: u64,
    dirty: DirtyTracker,
}

impl Grid {
    /// Creates a new [`Grid`] with minimum clamped dimensions `(cols >= 1, viewport_rows >= 1)`.
    #[must_use]
    pub fn new(
        kind: ScreenKind,
        cols: usize,
        viewport_rows: usize,
        scrollback_limit: usize,
    ) -> Self {
        let cols = cols.max(1);
        let viewport_rows = viewport_rows.max(1);
        let (scrollback_limit, row_pool_cap, initial_epoch) = match kind {
            ScreenKind::Primary => (scrollback_limit, ROW_POOL_CAP, 1),
            ScreenKind::Alternate => (0, 0, (1_u64 << 63) + 1),
        };

        let mut rows = VecDeque::with_capacity(viewport_rows);
        for _ in 0..viewport_rows {
            rows.push_back(Row::new(cols, initial_epoch));
        }

        Self {
            kind,
            cols,
            viewport_rows,
            scrollback_limit,
            rows,
            row_pool: Vec::new(),
            row_pool_cap,
            extras: CellExtraTable::new(),
            display_offset: 0,
            total_evicted: 0,
            damage_epoch: initial_epoch,
            content_epoch: initial_epoch,
            dirty: DirtyTracker::new(viewport_rows),
        }
    }

    /// Returns whether this grid is [`ScreenKind::Primary`] or [`ScreenKind::Alternate`].
    #[must_use]
    pub const fn kind(&self) -> ScreenKind {
        self.kind
    }

    /// Returns the number of columns in the grid.
    #[must_use]
    pub const fn cols(&self) -> usize {
        self.cols
    }

    /// Returns the number of visible rows in the viewport.
    #[must_use]
    pub const fn viewport_rows(&self) -> usize {
        self.viewport_rows
    }

    /// Returns the maximum number of scrollback lines retained above the viewport.
    #[must_use]
    pub const fn scrollback_limit(&self) -> usize {
        self.scrollback_limit
    }

    /// Returns the number of scrollback history lines currently stored above the viewport.
    #[must_use]
    pub fn history_len(&self) -> usize {
        self.rows.len().saturating_sub(self.viewport_rows)
    }

    /// Returns total retained lines (`history_len() + viewport_rows()`).
    #[must_use]
    pub fn total_len(&self) -> usize {
        self.rows.len()
    }

    /// Returns the number of oldest lines permanently evicted from the front of the ring buffer.
    #[must_use]
    pub const fn total_evicted(&self) -> u64 {
        self.total_evicted
    }

    /// Returns the [`AbsLine`] of the oldest currently retained line.
    #[must_use]
    pub const fn oldest_abs_line(&self) -> AbsLine {
        AbsLine(self.total_evicted)
    }

    /// Returns the [`AbsLine`] of the bottom-most live viewport line.
    #[must_use]
    pub fn newest_abs_line(&self) -> AbsLine {
        AbsLine(self.total_evicted + self.rows.len().saturating_sub(1) as u64)
    }

    /// Converts a buffer index `0..self.rows.len()` into an [`AbsLine`].
    #[must_use]
    pub const fn abs_line_of_buf(&self, buf_idx: usize) -> AbsLine {
        AbsLine(self.total_evicted + buf_idx as u64)
    }

    /// Converts an [`AbsLine`] into a buffer index inside `self.rows`, if still retained.
    #[must_use]
    pub fn buf_idx_of_abs(&self, line: AbsLine) -> Option<usize> {
        let rel = line.0.checked_sub(self.total_evicted)?;
        let idx = usize::try_from(rel).ok()?;
        (idx < self.rows.len()).then_some(idx)
    }

    /// Converts a live viewport row `0..viewport_rows` into its [`AbsLine`].
    #[must_use]
    pub fn live_viewport_to_abs(&self, vp_row: usize) -> AbsLine {
        let clamped = vp_row.min(self.viewport_rows.saturating_sub(1));
        self.abs_line_of_buf(self.history_len() + clamped)
    }

    /// Converts a displayed screen row `0..viewport_rows` (accounting for `display_offset`)
    /// into its [`AbsLine`].
    #[must_use]
    pub fn screen_to_abs(&self, screen_y: usize) -> AbsLine {
        let clamped = screen_y.min(self.viewport_rows.saturating_sub(1));
        let top = self.history_len().saturating_sub(self.display_offset);
        self.abs_line_of_buf(top + clamped)
    }

    /// Converts an [`AbsLine`] into a currently visible `screen_y` (`0..viewport_rows`), if in view.
    #[must_use]
    pub fn abs_to_screen(&self, line: AbsLine) -> Option<usize> {
        let buf_idx = self.buf_idx_of_abs(line)?;
        let top = self.history_len().saturating_sub(self.display_offset);
        let screen_y = buf_idx.checked_sub(top)?;
        (screen_y < self.viewport_rows).then_some(screen_y)
    }

    /// Returns the current scrollback display offset (`0` = live bottom).
    #[must_use]
    pub const fn display_offset(&self) -> usize {
        self.display_offset
    }

    /// Sets `display_offset` clamped to `history_len()`, bumping `damage_epoch` if it changed.
    pub fn set_display_offset(&mut self, offset: usize) -> bool {
        let clamped = if self.kind == ScreenKind::Alternate {
            0
        } else {
            offset.min(self.history_len())
        };
        if clamped == self.display_offset {
            false
        } else {
            self.display_offset = clamped;
            self.bump_damage_epoch();
            self.dirty.mark_all();
            true
        }
    }

    /// Scrolls the viewport display offset (`delta_lines > 0` scrolls up into history).
    pub fn scroll_display(&mut self, delta_lines: i32) -> bool {
        if self.kind == ScreenKind::Alternate || delta_lines == 0 {
            return false;
        }
        let target = if delta_lines > 0 {
            self.display_offset
                .saturating_add(delta_lines.unsigned_abs() as usize)
        } else {
            self.display_offset
                .saturating_sub(delta_lines.unsigned_abs() as usize)
        };
        self.set_display_offset(target)
    }

    /// Resets `display_offset` to `0` (live bottom).
    pub fn scroll_to_bottom(&mut self) -> bool {
        self.set_display_offset(0)
    }

    /// Returns the visible row at `screen_y` (`0..viewport_rows`) accounting for `display_offset`.
    #[must_use]
    pub fn visible_row(&self, screen_y: usize) -> Option<&Row> {
        if screen_y >= self.viewport_rows {
            return None;
        }
        let top = self.history_len().saturating_sub(self.display_offset);
        self.rows.get(top + screen_y)
    }

    /// Returns the live viewport row at `vp_row` (`0..viewport_rows`), ignoring `display_offset`.
    #[must_use]
    pub fn viewport_row(&self, vp_row: usize) -> Option<&Row> {
        if vp_row >= self.viewport_rows {
            return None;
        }
        self.rows.get(self.history_len() + vp_row)
    }

    /// Returns a row by [`AbsLine`], if still retained.
    #[must_use]
    pub fn row_by_abs(&self, line: AbsLine) -> Option<&Row> {
        let idx = self.buf_idx_of_abs(line)?;
        self.rows.get(idx)
    }

    /// Returns a row by raw buffer index `0..total_len()`.
    #[must_use]
    pub fn row_by_buf(&self, buf_idx: usize) -> Option<&Row> {
        self.rows.get(buf_idx)
    }

    /// Returns the grid's [`CellExtraTable`].
    #[must_use]
    pub const fn extras(&self) -> &CellExtraTable {
        &self.extras
    }

    /// Looks up a [`CellExtra`] by its 1-based `extra_id`.
    #[must_use]
    pub fn cell_extra(&self, extra_id: u32) -> Option<&CellExtra> {
        self.extras.get(extra_id)
    }

    /// Returns the monotonic mutation epoch for this grid (includes viewport scroll changes).
    #[must_use]
    pub const fn damage_epoch(&self) -> u64 {
        self.damage_epoch
    }

    /// Returns the monotonic content-mutation epoch (invariant across viewport scrolling).
    #[must_use]
    pub const fn content_epoch(&self) -> u64 {
        self.content_epoch
    }

    /// Returns the buffer index of the last row that is not completely empty, if any.
    #[cfg(any(feature = "search", feature = "hyperlinks"))]
    #[must_use]
    pub(crate) fn last_non_empty_buf_idx(&self) -> Option<usize> {
        (0..self.rows.len())
            .rev()
            .find(|&idx| !self.rows[idx].is_completely_empty())
    }

    /// Returns a read-only reference to the internal row ring buffer.
    #[cfg(feature = "hyperlinks")]
    #[must_use]
    pub(crate) const fn rows_deque(&self) -> &VecDeque<Row> {
        &self.rows
    }

    /// Returns the per-row [`DirtyTracker`].
    #[must_use]
    pub const fn dirty(&self) -> &DirtyTracker {
        &self.dirty
    }

    /// Clears the [`DirtyTracker`] flags.
    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    /// Increments and returns the monotonic `damage_epoch`.
    pub fn bump_damage_epoch(&mut self) -> u64 {
        self.damage_epoch += 1;
        self.damage_epoch
    }

    /// Marks all viewport rows dirty and bumps `damage_epoch`.
    pub(crate) fn mark_all_dirty(&mut self) {
        self.damage_epoch += 1;
        self.dirty.mark_all();
    }

    /// Returns the number of pooled rows currently held (for diagnostics/tests).
    #[must_use]
    pub fn pooled_row_count(&self) -> usize {
        self.row_pool.len()
    }

    fn acquire_row(&mut self, fill: Cell, generation: u64) -> Row {
        if let Some(mut row) = self.row_pool.pop() {
            row.reset_for_reuse(self.cols, fill, generation, &mut self.extras);
            row
        } else {
            Row::filled(self.cols, fill, generation)
        }
    }

    fn recycle_row(&mut self, mut row: Row) {
        row.free_all_extras(&mut self.extras);
        if self.row_pool.len() < self.row_pool_cap {
            self.row_pool.push(row);
        }
    }

    /// Writes a single-width `cell` at `(vp_row, col)`, optionally attaching `extra`, and cleans
    /// wide-char overlaps and overwritten `CellExtra` entries.
    pub(crate) fn write_cell(
        &mut self,
        vp_row: usize,
        col: usize,
        mut cell: Cell,
        extra: Option<CellExtra>,
        bg: PackedColor,
    ) {
        if vp_row >= self.viewport_rows || col >= self.cols {
            return;
        }
        let hist = self.history_len();
        let cols = self.cols;
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];

        row.clear_wide_overlap_at(col, bg, &mut self.extras);
        if row.may_have_extras() && row.cells()[col].extra_id != 0 {
            self.extras.free(row.cells()[col].extra_id);
        }

        if let Some(ext) = extra.filter(|e| !e.is_empty()) {
            cell.extra_id = self.extras.insert(ext);
            row.mark_has_extras();
        } else {
            cell.extra_id = 0;
        }

        row.cells_mut()[col] = cell;
        if col + 1 == cols && !cell.is_wrap_spacer() {
            row.set_soft_wrapped(false);
        }
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
    }

    /// Writes a 2-column wide character at `(vp_row, col)` and its `WIDE_SPACER` at `col + 1`.
    pub(crate) fn write_wide_cell(
        &mut self,
        vp_row: usize,
        col: usize,
        mut lead: Cell,
        spacer: Cell,
        extra: Option<CellExtra>,
        bg: PackedColor,
    ) {
        if vp_row >= self.viewport_rows || col + 1 >= self.cols {
            return;
        }
        let hist = self.history_len();
        let cols = self.cols;
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];

        row.clear_wide_overlap_at(col, bg, &mut self.extras);
        row.clear_wide_overlap_at(col + 1, bg, &mut self.extras);

        if row.may_have_extras() {
            if row.cells()[col].extra_id != 0 {
                self.extras.free(row.cells()[col].extra_id);
            }
            if row.cells()[col + 1].extra_id != 0 {
                self.extras.free(row.cells()[col + 1].extra_id);
            }
        }

        if let Some(ext) = extra.filter(|e| !e.is_empty()) {
            lead.extra_id = self.extras.insert(ext);
            row.mark_has_extras();
        } else {
            lead.extra_id = 0;
        }

        let cells = row.cells_mut();
        cells[col] = lead;
        cells[col + 1] = spacer;
        if col + 2 == cols {
            row.set_soft_wrapped(false);
        }
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
    }

    /// Fast-path batch write for contiguous printable ASCII bytes on a single row (`extra_id == 0`).
    pub(crate) fn write_ascii_slice(
        &mut self,
        vp_row: usize,
        start_col: usize,
        bytes: &[u8],
        fg: PackedColor,
        bg: PackedColor,
        flags: CellFlags,
    ) {
        if bytes.is_empty() || vp_row >= self.viewport_rows || start_col >= self.cols {
            return;
        }
        let end_col = (start_col + bytes.len()).min(self.cols);
        let hist = self.history_len();
        let cols = self.cols;
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];

        row.clear_wide_overlap_at(start_col, bg, &mut self.extras);
        if end_col > start_col + 1 {
            row.clear_wide_overlap_at(end_col - 1, bg, &mut self.extras);
        }

        if row.may_have_extras() {
            for cell in &mut row.cells_mut()[start_col..end_col] {
                if cell.extra_id != 0 {
                    self.extras.free(cell.extra_id);
                }
            }
        }

        let clean_flags = flags & CellFlags::SGR_MASK;
        for (dst, &b) in row.cells_mut()[start_col..end_col].iter_mut().zip(bytes) {
            *dst = Cell::new(char::from(b), fg, bg, clean_flags);
        }

        if end_col == cols {
            row.set_soft_wrapped(false);
        }
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
    }

    /// Marks `vp_row` as soft-wrapped (`soft_wrapped = wrapped`) and bumps its generation.
    pub(crate) fn set_row_soft_wrapped(&mut self, vp_row: usize, wrapped: bool) {
        if vp_row >= self.viewport_rows {
            return;
        }
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];
        row.set_soft_wrapped(wrapped);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
    }

    /// Places a `WRAP_SPACER` at `(vp_row, cols - 1)` and sets `soft_wrapped = true`.
    pub(crate) fn set_wrap_spacer(&mut self, vp_row: usize, bg: PackedColor) {
        if vp_row >= self.viewport_rows || self.cols == 0 {
            return;
        }
        let last_col = self.cols - 1;
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];

        row.clear_wide_overlap_at(last_col, bg, &mut self.extras);
        if row.may_have_extras() && row.cells()[last_col].extra_id != 0 {
            self.extras.free(row.cells()[last_col].extra_id);
        }
        row.cells_mut()[last_col] = Cell::new(
            ' ',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::WRAP_SPACER,
        );
        row.set_soft_wrapped(true);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
    }

    /// Appends a combining/ZWJ/VS scalar to an existing cell at `(vp_row, col)`, returning the
    /// cell's new flags/codepoint status. Does not resize the cell width itself.
    pub(crate) fn append_combining_scalar(&mut self, vp_row: usize, col: usize, ch: char) -> bool {
        if vp_row >= self.viewport_rows || col >= self.cols {
            return false;
        }
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];
        let cell = &mut row.cells_mut()[col];
        let extra = self.extras.ensure_for_cell(cell);
        extra.push_combining(ch);
        row.mark_has_extras();
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
        true
    }

    /// Promotes a 1-column cell at `(vp_row, col)` to 2 columns (`WIDE_LEAD` + `WIDE_SPACER` at
    /// `col + 1`), preserving its `extra_id`. Caller guarantees `col + 1 < cols`.
    pub(crate) fn promote_cell_to_wide(&mut self, vp_row: usize, col: usize, bg: PackedColor) {
        if vp_row >= self.viewport_rows || col + 1 >= self.cols {
            return;
        }
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];
        row.clear_wide_overlap_at(col + 1, bg, &mut self.extras);
        if row.may_have_extras() && row.cells()[col + 1].extra_id != 0 {
            self.extras.free(row.cells()[col + 1].extra_id);
        }
        let cells = row.cells_mut();
        cells[col].flags.insert(CellFlags::WIDE_LEAD);
        let spacer_flags = CellFlags::WIDE_SPACER | (cells[col].flags & CellFlags::SGR_MASK);
        cells[col + 1] = Cell::new(' ', cells[col].fg, cells[col].bg, spacer_flags);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
    }

    /// Demotes a 2-column cell at `(vp_row, col)` back to 1 column and clears its `WIDE_SPACER`.
    pub(crate) fn demote_wide_to_narrow(&mut self, vp_row: usize, col: usize, bg: PackedColor) {
        if vp_row >= self.viewport_rows || col + 1 >= self.cols {
            return;
        }
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];
        let cells = row.cells_mut();
        cells[col].flags.remove(CellFlags::WIDE_LEAD);
        if cells[col + 1].is_wide_spacer() {
            cells[col + 1].reset(bg);
        }
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
    }

    /// Replaces `(vp_row, col)` with a `WRAP_SPACER` (`soft_wrapped = true`) and returns the
    /// displaced `Cell` (with `WIDE_LEAD` set, `extra_id` zeroed) plus any detached `CellExtra`.
    pub(crate) fn take_cell_as_wrap_spacer(
        &mut self,
        vp_row: usize,
        col: usize,
    ) -> Option<(Cell, Option<CellExtra>)> {
        if vp_row >= self.viewport_rows || col >= self.cols {
            return None;
        }
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];
        let mut moved = row.cells()[col];
        let extra = if moved.extra_id != 0 {
            let ext = self.extras.get(moved.extra_id).cloned();
            self.extras.free(moved.extra_id);
            moved.extra_id = 0;
            ext
        } else {
            None
        };
        moved.flags.insert(CellFlags::WIDE_LEAD);
        row.cells_mut()[col] = Cell::new(
            ' ',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::WRAP_SPACER,
        );
        row.set_soft_wrapped(true);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
        Some((moved, extra))
    }

    /// Moves a cell (and its `extra_id`, without cloning or freeing) from `(src_row, src_col)` to
    /// `(dst_row, 0..=1)`, leaving a `WRAP_SPACER` at `(src_row, src_col)`.
    pub(crate) fn move_cell_to_wide_wrap(
        &mut self,
        src_row: usize,
        src_col: usize,
        dst_row: usize,
        bg: PackedColor,
    ) {
        if src_row >= self.viewport_rows
            || dst_row >= self.viewport_rows
            || src_row == dst_row
            || self.cols < 2
        {
            return;
        }
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;

        let mut moved = self.rows[hist + src_row].cells()[src_col];
        moved.flags.insert(CellFlags::WIDE_LEAD);
        let spacer = Cell::new(
            ' ',
            moved.fg,
            moved.bg,
            CellFlags::WIDE_SPACER | (moved.flags & CellFlags::SGR_MASK),
        );

        // Put WRAP_SPACER at src_col (replacing the moved cell without freeing its extra_id).
        {
            let srow = &mut self.rows[hist + src_row];
            srow.cells_mut()[src_col] = Cell::new(
                ' ',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::WRAP_SPACER,
            );
            srow.set_soft_wrapped(true);
            srow.set_generation(epoch);
        }

        // Write moved lead + spacer at (dst_row, 0..=1).
        self.damage_epoch += 1;
        let epoch2 = self.damage_epoch;
        {
            let drow = &mut self.rows[hist + dst_row];
            drow.clear_wide_overlap_at(0, bg, &mut self.extras);
            drow.clear_wide_overlap_at(1, bg, &mut self.extras);
            if drow.may_have_extras() {
                if drow.cells()[0].extra_id != 0 {
                    self.extras.free(drow.cells()[0].extra_id);
                }
                if drow.cells()[1].extra_id != 0 {
                    self.extras.free(drow.cells()[1].extra_id);
                }
            }
            if moved.has_extra() {
                drow.mark_has_extras();
            }
            drow.cells_mut()[0] = moved;
            drow.cells_mut()[1] = spacer;
            if self.cols == 2 {
                drow.set_soft_wrapped(false);
            }
            drow.set_generation(epoch2);
        }

        self.dirty.mark_row(src_row);
        self.dirty.mark_row(dst_row);
    }

    /// Scrolls lines `[top..=bottom]` up by `count` rows.
    ///
    /// When `push_to_history` is `true` and the region covers the full primary viewport
    /// (`top == 0 && bottom == viewport_rows - 1`), evicted top rows enter scrollback history
    /// while surviving rows keep their exact `Row::generation` values.
    pub(crate) fn scroll_up_region(
        &mut self,
        top: usize,
        bottom: usize,
        count: usize,
        bg: PackedColor,
        push_to_history: bool,
    ) {
        let top = top.min(self.viewport_rows.saturating_sub(1));
        let bottom = bottom.min(self.viewport_rows.saturating_sub(1));
        if top > bottom || count == 0 {
            return;
        }
        let region_h = bottom - top + 1;
        let n = count.min(region_h);
        self.content_epoch += 1;
        let fill = Cell::new(' ', PackedColor::Default, bg, CellFlags::empty());

        if push_to_history && top == 0 && bottom == self.viewport_rows - 1 {
            match self.kind {
                ScreenKind::Primary => {
                    for _ in 0..n {
                        self.damage_epoch += 1;
                        let epoch = self.damage_epoch;
                        if self.scrollback_limit == 0 {
                            let mut front = self
                                .rows
                                .pop_front()
                                .expect("grid always holds at least viewport_rows");
                            self.total_evicted += 1;
                            front.reset_for_reuse(self.cols, fill, epoch, &mut self.extras);
                            self.rows.push_back(front);
                        } else if self.history_len() < self.scrollback_limit {
                            let fresh = self.acquire_row(fill, epoch);
                            self.rows.push_back(fresh);
                            if self.display_offset > 0 {
                                self.display_offset =
                                    (self.display_offset + 1).min(self.history_len());
                            }
                        } else {
                            let mut front = self
                                .rows
                                .pop_front()
                                .expect("grid always holds at least viewport_rows");
                            self.total_evicted += 1;
                            front.reset_for_reuse(self.cols, fill, epoch, &mut self.extras);
                            self.rows.push_back(front);
                            if self.display_offset > 0 {
                                self.display_offset =
                                    (self.display_offset + 1).min(self.history_len());
                            }
                        }
                    }
                    self.dirty.mark_all();
                    return;
                }
                ScreenKind::Alternate => {
                    for _ in 0..n {
                        self.damage_epoch += 1;
                        let epoch = self.damage_epoch;
                        let mut front = self
                            .rows
                            .pop_front()
                            .expect("alt grid always holds viewport_rows");
                        front.reset_for_reuse(self.cols, fill, epoch, &mut self.extras);
                        self.rows.push_back(front);
                    }
                    self.damage_epoch += 1;
                    let shift_epoch = self.damage_epoch;
                    for row in &mut self.rows {
                        row.set_generation(shift_epoch);
                    }
                    self.dirty.mark_all();
                    return;
                }
            }
        }

        let hist = self.history_len();
        for _ in 0..n {
            self.damage_epoch += 1;
            let epoch = self.damage_epoch;
            let mut row = self
                .rows
                .remove(hist + top)
                .expect("scroll region index within grid");
            row.reset_for_reuse(self.cols, fill, epoch, &mut self.extras);
            self.rows.insert(hist + bottom, row);
        }
        self.damage_epoch += 1;
        let shift_epoch = self.damage_epoch;
        for row in self.rows.range_mut((hist + top)..=(hist + bottom)) {
            row.set_generation(shift_epoch);
        }
        if top == 0 && bottom == self.viewport_rows - 1 {
            self.dirty.mark_all();
        } else {
            for r in top..=bottom {
                self.dirty.mark_row(r);
            }
        }
    }

    /// Scrolls lines `[top..=bottom]` down by `count` rows, inserting blank BCE rows at `top`.
    pub(crate) fn scroll_down_region(
        &mut self,
        top: usize,
        bottom: usize,
        count: usize,
        bg: PackedColor,
    ) {
        let top = top.min(self.viewport_rows.saturating_sub(1));
        let bottom = bottom.min(self.viewport_rows.saturating_sub(1));
        if top > bottom || count == 0 {
            return;
        }
        let region_h = bottom - top + 1;
        let n = count.min(region_h);
        self.content_epoch += 1;
        let fill = Cell::new(' ', PackedColor::Default, bg, CellFlags::empty());
        let hist = self.history_len();

        for _ in 0..n {
            self.damage_epoch += 1;
            let epoch = self.damage_epoch;
            let mut row = self
                .rows
                .remove(hist + bottom)
                .expect("scroll region index within grid");
            row.reset_for_reuse(self.cols, fill, epoch, &mut self.extras);
            self.rows.insert(hist + top, row);
        }
        self.damage_epoch += 1;
        let shift_epoch = self.damage_epoch;
        for row in self.rows.range_mut((hist + top)..=(hist + bottom)) {
            row.set_generation(shift_epoch);
        }

        if top == 0 && bottom == self.viewport_rows - 1 {
            self.dirty.mark_all();
        } else {
            for r in top..=bottom {
                self.dirty.mark_row(r);
            }
        }
    }

    /// Scrolls the rectangular box `[top..=bottom] x [left..=right]` up by `count` rows.
    pub(crate) fn scroll_up_box(
        &mut self,
        top: usize,
        bottom: usize,
        left: usize,
        right: usize,
        count: usize,
        bg: PackedColor,
    ) {
        let top = top.min(self.viewport_rows.saturating_sub(1));
        let bottom = bottom.min(self.viewport_rows.saturating_sub(1));
        let right = right.min(self.cols.saturating_sub(1));
        if top > bottom || left > right || count == 0 {
            return;
        }
        let region_h = bottom - top + 1;
        let n = count.min(region_h);
        self.content_epoch += 1;
        let blank = Cell::new(' ', PackedColor::Default, bg, CellFlags::empty());
        let hist = self.history_len();

        for offset in 0..region_h {
            self.damage_epoch += 1;
            let epoch = self.damage_epoch;
            let dst_idx = hist + top + offset;
            if offset + n < region_h {
                let src_idx = hist + top + offset + n;
                self.rows[dst_idx].clear_range(left..(right + 1), bg, &mut self.extras);
                self.rows[src_idx].clear_wide_overlap_at(left, bg, &mut self.extras);
                self.rows[src_idx].clear_wide_overlap_at(right, bg, &mut self.extras);
                for col in left..=right {
                    let cell = self.rows[src_idx].cells()[col];
                    self.rows[src_idx].cells_mut()[col] = blank;
                    self.rows[dst_idx].cells_mut()[col] = cell;
                }
                self.rows[src_idx].set_soft_wrapped(false);
                self.rows[src_idx].set_generation(epoch);
            } else {
                self.rows[dst_idx].clear_range(left..(right + 1), bg, &mut self.extras);
            }
            self.rows[dst_idx].set_soft_wrapped(false);
            self.rows[dst_idx].set_generation(epoch);
            self.dirty.mark_row(top + offset);
        }
    }

    /// Scrolls the rectangular box `[top..=bottom] x [left..=right]` down by `count` rows.
    pub(crate) fn scroll_down_box(
        &mut self,
        top: usize,
        bottom: usize,
        left: usize,
        right: usize,
        count: usize,
        bg: PackedColor,
    ) {
        let top = top.min(self.viewport_rows.saturating_sub(1));
        let bottom = bottom.min(self.viewport_rows.saturating_sub(1));
        let right = right.min(self.cols.saturating_sub(1));
        if top > bottom || left > right || count == 0 {
            return;
        }
        let region_h = bottom - top + 1;
        let n = count.min(region_h);
        self.content_epoch += 1;
        let blank = Cell::new(' ', PackedColor::Default, bg, CellFlags::empty());
        let hist = self.history_len();

        for offset in (0..region_h).rev() {
            self.damage_epoch += 1;
            let epoch = self.damage_epoch;
            let dst_idx = hist + top + offset;
            if offset >= n {
                let src_idx = hist + top + offset - n;
                self.rows[dst_idx].clear_range(left..(right + 1), bg, &mut self.extras);
                self.rows[src_idx].clear_wide_overlap_at(left, bg, &mut self.extras);
                self.rows[src_idx].clear_wide_overlap_at(right, bg, &mut self.extras);
                for col in left..=right {
                    let cell = self.rows[src_idx].cells()[col];
                    self.rows[src_idx].cells_mut()[col] = blank;
                    self.rows[dst_idx].cells_mut()[col] = cell;
                }
                self.rows[src_idx].set_soft_wrapped(false);
                self.rows[src_idx].set_generation(epoch);
            } else {
                self.rows[dst_idx].clear_range(left..(right + 1), bg, &mut self.extras);
            }
            self.rows[dst_idx].set_soft_wrapped(false);
            self.rows[dst_idx].set_generation(epoch);
            self.dirty.mark_row(top + offset);
        }
    }

    /// Inserts `count` blank cells (`ICH`) at `(vp_row, col)`, shifting remaining cells right
    /// up to `right_margin` (inclusive).
    pub(crate) fn insert_cells_in_margin(
        &mut self,
        vp_row: usize,
        col: usize,
        right_margin: usize,
        count: usize,
        bg: PackedColor,
    ) {
        let right = right_margin.min(self.cols.saturating_sub(1));
        if vp_row >= self.viewport_rows || col > right || count == 0 {
            return;
        }
        let end = right + 1;
        let n = count.min(end - col);
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];

        row.clear_wide_overlap_at(col, bg, &mut self.extras);
        let shift_end = end - n;
        if shift_end > col && row.cells()[shift_end - 1].is_wide_lead() {
            row.clear_wide_overlap_at(shift_end - 1, bg, &mut self.extras);
        }
        if row.may_have_extras() {
            for cell in &mut row.cells_mut()[shift_end..end] {
                if cell.extra_id != 0 {
                    self.extras.free(cell.extra_id);
                    cell.extra_id = 0;
                }
            }
        }

        row.cells_mut().copy_within(col..shift_end, col + n);
        let blank = Cell::new(' ', PackedColor::Default, bg, CellFlags::empty());
        row.cells_mut()[col..col + n].fill(blank);
        row.set_soft_wrapped(false);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
    }

    /// Deletes `count` cells (`DCH`) at `(vp_row, col)`, shifting remaining cells left
    /// within `col..=right_margin`.
    pub(crate) fn delete_cells_in_margin(
        &mut self,
        vp_row: usize,
        col: usize,
        right_margin: usize,
        count: usize,
        bg: PackedColor,
    ) {
        let right = right_margin.min(self.cols.saturating_sub(1));
        if vp_row >= self.viewport_rows || col > right || count == 0 {
            return;
        }
        let end = right + 1;
        let n = count.min(end - col);
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];

        row.clear_wide_overlap_at(col, bg, &mut self.extras);
        row.clear_wide_overlap_at(col + n - 1, bg, &mut self.extras);
        if row.may_have_extras() {
            for cell in &mut row.cells_mut()[col..col + n] {
                if cell.extra_id != 0 {
                    self.extras.free(cell.extra_id);
                    cell.extra_id = 0;
                }
            }
        }

        if end == self.cols && end > 0 && row.cells()[end - 1].is_wrap_spacer() {
            row.cells_mut()[end - 1] = Cell::BLANK;
        }
        row.cells_mut().copy_within(col + n..end, col);
        let blank = Cell::new(' ', PackedColor::Default, bg, CellFlags::empty());
        row.cells_mut()[end - n..end].fill(blank);
        row.set_soft_wrapped(false);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
    }

    /// Inserts `count` blank cells (`ICH`) at `(vp_row, col)`, shifting remaining cells right.
    pub(crate) fn insert_cells(
        &mut self,
        vp_row: usize,
        col: usize,
        count: usize,
        bg: PackedColor,
    ) {
        if vp_row >= self.viewport_rows || col >= self.cols || count == 0 {
            return;
        }
        let cols = self.cols;
        let n = count.min(cols - col);
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];

        row.clear_wide_overlap_at(col, bg, &mut self.extras);
        let shift_end = cols - n;
        if shift_end > 0 && row.cells()[shift_end - 1].is_wide_lead() {
            row.clear_wide_overlap_at(shift_end - 1, bg, &mut self.extras);
        }
        if row.may_have_extras() {
            for cell in &mut row.cells_mut()[shift_end..cols] {
                if cell.extra_id != 0 {
                    self.extras.free(cell.extra_id);
                    cell.extra_id = 0;
                }
            }
        }

        row.cells_mut().copy_within(col..shift_end, col + n);
        let blank = Cell::new(' ', PackedColor::Default, bg, CellFlags::empty());
        row.cells_mut()[col..col + n].fill(blank);
        row.set_soft_wrapped(false);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
    }

    /// Deletes `count` cells (`DCH`) at `(vp_row, col)`, shifting remaining cells left.
    pub(crate) fn delete_cells(
        &mut self,
        vp_row: usize,
        col: usize,
        count: usize,
        bg: PackedColor,
    ) {
        if vp_row >= self.viewport_rows || col >= self.cols || count == 0 {
            return;
        }
        let cols = self.cols;
        let n = count.min(cols - col);
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];

        row.clear_wide_overlap_at(col, bg, &mut self.extras);
        row.clear_wide_overlap_at(col + n - 1, bg, &mut self.extras);
        if row.may_have_extras() {
            for cell in &mut row.cells_mut()[col..col + n] {
                if cell.extra_id != 0 {
                    self.extras.free(cell.extra_id);
                    cell.extra_id = 0;
                }
            }
        }

        if cols > 0 && row.cells()[cols - 1].is_wrap_spacer() {
            row.cells_mut()[cols - 1] = Cell::BLANK;
        }
        row.cells_mut().copy_within(col + n..cols, col);
        let blank = Cell::new(' ', PackedColor::Default, bg, CellFlags::empty());
        row.cells_mut()[cols - n..cols].fill(blank);
        row.set_soft_wrapped(false);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
    }

    /// Erases `count` cells (`ECH`) starting at `(vp_row, col)` without shifting.
    pub(crate) fn erase_chars(&mut self, vp_row: usize, col: usize, count: usize, bg: PackedColor) {
        if vp_row >= self.viewport_rows || col >= self.cols || count == 0 {
            return;
        }
        let end = (col + count).min(self.cols);
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];
        row.clear_range(col..end, bg, &mut self.extras);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
    }

    /// Erases within `vp_row` according to `EL` mode (`0`: cursor..end, `1`: start..=cursor, `2`: whole row).
    pub(crate) fn erase_in_line(&mut self, vp_row: usize, col: usize, mode: u16, bg: PackedColor) {
        if vp_row >= self.viewport_rows {
            return;
        }
        let cols = self.cols;
        let range = match mode {
            0 => col.min(cols)..cols,
            1 => 0..col.saturating_add(1).min(cols),
            2 => 0..cols,
            _ => return,
        };
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];
        row.clear_range(range, bg, &mut self.extras);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
    }

    /// Erases display cells according to `ED` mode (`0`, `1`, `2`, `3`).
    pub(crate) fn erase_in_display(
        &mut self,
        vp_row: usize,
        col: usize,
        mode: u16,
        bg: PackedColor,
    ) {
        let vrows = self.viewport_rows;
        let cols = self.cols;
        let hist = self.history_len();
        let fill = Cell::new(' ', PackedColor::Default, bg, CellFlags::empty());

        match mode {
            0 => {
                self.erase_in_line(vp_row, col, 0, bg);
                for r in (vp_row + 1)..vrows {
                    self.damage_epoch += 1;
                    let epoch = self.damage_epoch;
                    self.rows[hist + r].reset_for_reuse(cols, fill, epoch, &mut self.extras);
                    self.dirty.mark_row(r);
                }
            }
            1 => {
                for r in 0..vp_row.min(vrows) {
                    self.damage_epoch += 1;
                    let epoch = self.damage_epoch;
                    self.rows[hist + r].reset_for_reuse(cols, fill, epoch, &mut self.extras);
                    self.dirty.mark_row(r);
                }
                self.erase_in_line(vp_row, col, 1, bg);
            }
            2 => self.clear_visible(bg),
            3 => self.clear_history(),
            _ => {}
        }
    }

    /// Clears all visible viewport rows with BCE `bg` without touching scrollback history.
    pub(crate) fn clear_visible(&mut self, bg: PackedColor) {
        let hist = self.history_len();
        let cols = self.cols;
        let fill = Cell::new(' ', PackedColor::Default, bg, CellFlags::empty());
        self.content_epoch += 1;
        for r in 0..self.viewport_rows {
            self.damage_epoch += 1;
            let epoch = self.damage_epoch;
            self.rows[hist + r].reset_for_reuse(cols, fill, epoch, &mut self.extras);
        }
        self.dirty.mark_all();
    }

    /// Clears all scrollback history above the live viewport (`ED 3`), reclaiming extras and
    /// pooling evicted rows.
    pub(crate) fn clear_history(&mut self) {
        let hist = self.history_len();
        if hist == 0 {
            self.display_offset = 0;
            return;
        }
        self.content_epoch += 1;
        for _ in 0..hist {
            if let Some(row) = self.rows.pop_front() {
                self.total_evicted += 1;
                self.recycle_row(row);
            }
        }
        if self.display_offset != 0 {
            self.display_offset = 0;
            self.bump_damage_epoch();
            self.dirty.mark_all();
        }
    }

    /// Resets the entire grid (including scrollback, extras, and row pool) for `RIS` (`ESC c`).
    pub(crate) fn clear_all(&mut self) {
        self.extras.clear();
        self.rows.clear();
        self.row_pool.clear();
        self.display_offset = 0;
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        for _ in 0..self.viewport_rows {
            self.rows.push_back(Row::new(self.cols, epoch));
        }
        self.dirty.mark_all();
    }

    /// Clones visible viewport rows and all referenced `CellExtra`s for a DEC Mode 2026
    /// synchronized snapshot.
    #[must_use]
    pub fn clone_visible_snapshot(&self) -> (Vec<Row>, Vec<(u32, CellExtra)>) {
        let top = self.history_len().saturating_sub(self.display_offset);
        let mut rows = Vec::with_capacity(self.viewport_rows);
        let mut extras = Vec::new();
        for y in 0..self.viewport_rows {
            if let Some(row) = self.rows.get(top + y) {
                rows.push(row.clone());
                if row.may_have_extras() {
                    for cell in row.cells() {
                        if cell.extra_id != 0
                            && let Some(ext) = self.extras.get(cell.extra_id)
                        {
                            extras.push((cell.extra_id, ext.clone()));
                        }
                    }
                }
            }
        }
        extras.sort_unstable_by_key(|(id, _)| *id);
        (rows, extras)
    }

    /// Exports a serializable snapshot of this [`Grid`].
    #[cfg(feature = "serde")]
    pub(crate) fn export_snapshot(&self) -> GridSnapshot {
        GridSnapshot {
            cols: self.cols,
            viewport_rows: self.viewport_rows,
            rows: self.rows.iter().cloned().collect(),
            extras: self.extras.clone(),
            display_offset: self.display_offset,
            total_evicted: self.total_evicted,
            damage_epoch: self.damage_epoch,
            content_epoch: self.content_epoch,
        }
    }

    /// Reconstructs a [`Grid`] from a [`GridSnapshot`], defensively sanitizing row widths,
    /// wide/wrap spacer invariants, hyperlink references, and `CellExtraTable` ownership.
    #[cfg(feature = "serde")]
    pub(crate) fn from_snapshot(
        kind: ScreenKind,
        snap: GridSnapshot,
        scrollback_limit: usize,
        hyperlink_pool: &crate::cell::HyperlinkPool,
    ) -> Self {
        let cols = snap.cols.clamp(1, usize::from(u16::MAX));
        let viewport_rows = snap.viewport_rows.clamp(1, usize::from(u16::MAX));
        let (scrollback_limit, row_pool_cap, min_epoch) = match kind {
            ScreenKind::Primary => (scrollback_limit, ROW_POOL_CAP, 1),
            ScreenKind::Alternate => (0, 0, (1_u64 << 63) + 1),
        };
        const MAX_SAFE_EPOCH: u64 = u64::MAX - (1_u64 << 32);
        let damage_epoch = snap.damage_epoch.clamp(min_epoch, MAX_SAFE_EPOCH);
        let content_epoch = snap.content_epoch.clamp(min_epoch, MAX_SAFE_EPOCH);

        let mut rows: VecDeque<Row> = snap.rows.into();
        let max_rows = scrollback_limit.saturating_add(viewport_rows);
        let mut total_evicted = match kind {
            ScreenKind::Primary => snap.total_evicted.min(u64::MAX / 2),
            ScreenKind::Alternate => 0,
        };
        while rows.len() > max_rows {
            rows.pop_front();
            if kind == ScreenKind::Primary {
                total_evicted = total_evicted.saturating_add(1);
            }
        }
        while rows.len() < viewport_rows {
            rows.push_back(Row::new(cols, damage_epoch));
        }

        let mut new_extras = CellExtraTable::new();
        for row in &mut rows {
            let was_soft_wrapped = row.soft_wrapped();
            let row_gen = row.generation().clamp(min_epoch, damage_epoch);
            let mut cells = row.take_cells();
            let mut soft_wrapped = was_soft_wrapped;
            if cells.len() != cols {
                soft_wrapped = false;
                cells.resize(cols, Cell::BLANK);
            }

            // Pass 1: sanitize unknown flag bits and WRAP_SPACER before validating WIDE_LEAD / WIDE_SPACER pairs.
            for (c, cell) in cells.iter_mut().enumerate() {
                cell.flags = CellFlags::from_bits_truncate(cell.flags.bits());
                if cell.is_wrap_spacer() {
                    let is_last = c + 1 == cols;
                    if is_last && soft_wrapped {
                        cell.flags
                            .remove(CellFlags::WIDE_LEAD | CellFlags::WIDE_SPACER);
                        cell.extra_id = 0;
                    } else {
                        cell.flags.remove(CellFlags::WRAP_SPACER);
                    }
                }
            }

            // Pass 2: sanitize mutually-exclusive WIDE_LEAD / WIDE_SPACER flags and rebuild extras.
            for c in 0..cols {
                if cells[c].is_wrap_spacer() {
                    continue;
                }
                if cells[c].is_wide_spacer() {
                    if c > 0 && cells[c - 1].is_wide_lead() {
                        cells[c]
                            .flags
                            .remove(CellFlags::WIDE_LEAD | CellFlags::WRAP_SPACER);
                        cells[c].extra_id = 0;
                        continue;
                    }
                    cells[c].flags.remove(CellFlags::WIDE_SPACER);
                }
                if cells[c].is_wide_lead() {
                    let is_last = c + 1 == cols;
                    if is_last || !cells[c + 1].is_wide_spacer() {
                        cells[c].flags.remove(CellFlags::WIDE_LEAD);
                    } else {
                        cells[c]
                            .flags
                            .remove(CellFlags::WIDE_SPACER | CellFlags::WRAP_SPACER);
                    }
                }
                if cells[c].extra_id != 0 {
                    if let Some(mut ext) = snap.extras.get(cells[c].extra_id).cloned() {
                        if let Some(hid) = ext.hyperlink_id
                            && hyperlink_pool.get(hid).is_none()
                        {
                            ext.hyperlink_id = None;
                        }
                        if !ext.is_empty() {
                            cells[c].extra_id = new_extras.insert(ext);
                        } else {
                            cells[c].extra_id = 0;
                        }
                    } else {
                        cells[c].extra_id = 0;
                    }
                }
            }
            row.replace_cells(cells, soft_wrapped, row_gen);
        }

        let history_len = rows.len().saturating_sub(viewport_rows);
        let display_offset = if kind == ScreenKind::Alternate {
            0
        } else {
            snap.display_offset.min(history_len)
        };

        Self {
            kind,
            cols,
            viewport_rows,
            scrollback_limit,
            rows,
            row_pool: Vec::new(),
            row_pool_cap,
            extras: new_extras,
            display_offset,
            total_evicted,
            damage_epoch,
            content_epoch,
            dirty: DirtyTracker::new(viewport_rows),
        }
    }

    /// Verifies that every non-zero `extra_id` in `rows` is unique, valid, and matches
    /// `extras.live_count()`, and that `row_pool` holds zero `extra_id`s.
    #[cfg(test)]
    pub(crate) fn debug_verify_extra_invariants(&self) {
        use std::collections::HashSet;

        let mut seen = HashSet::new();
        for row in &self.rows {
            for cell in row.cells() {
                if cell.extra_id != 0 {
                    assert!(
                        row.may_have_extras(),
                        "row with extra_id={} had may_have_extras=false",
                        cell.extra_id
                    );
                    assert!(
                        self.extras.get(cell.extra_id).is_some(),
                        "dangling extra_id={}",
                        cell.extra_id
                    );
                    assert!(
                        seen.insert(cell.extra_id),
                        "duplicate extra_id={}",
                        cell.extra_id
                    );
                }
            }
        }
        for pooled in &self.row_pool {
            assert!(!pooled.may_have_extras());
            for cell in pooled.cells() {
                assert_eq!(cell.extra_id, 0, "pooled row leaked extra_id");
            }
        }
        assert_eq!(
            seen.len(),
            self.extras.live_count(),
            "CellExtraTable leak/mismatch: seen {} vs live {}",
            seen.len(),
            self.extras.live_count()
        );
    }

    /// Verifies structural `WIDE_LEAD`/`WIDE_SPACER`/`WRAP_SPACER` invariants on every row.
    #[cfg(test)]
    pub(crate) fn debug_verify_wide_invariants(&self) {
        for (r_idx, row) in self.rows.iter().enumerate() {
            let cells = row.cells();
            assert_eq!(cells.len(), self.cols);
            for (c_idx, cell) in cells.iter().enumerate() {
                let structural_count = usize::from(cell.is_wide_lead())
                    + usize::from(cell.is_wide_spacer())
                    + usize::from(cell.is_wrap_spacer());
                assert!(
                    structural_count <= 1,
                    "row {r_idx} col {c_idx}: mutually exclusive structural flags {:?}",
                    cell.flags
                );
                if cell.is_wide_lead() {
                    assert!(
                        c_idx + 1 < self.cols && cells[c_idx + 1].is_wide_spacer(),
                        "row {r_idx} col {c_idx}: WIDE_LEAD without WIDE_SPACER"
                    );
                }
                if cell.is_wide_spacer() {
                    assert!(
                        c_idx > 0 && cells[c_idx - 1].is_wide_lead(),
                        "row {r_idx} col {c_idx}: orphan WIDE_SPACER"
                    );
                    assert_eq!(cell.extra_id, 0, "WIDE_SPACER must not have extra_id");
                }
                if cell.is_wrap_spacer() {
                    assert_eq!(c_idx + 1, self.cols, "WRAP_SPACER not at last column");
                    assert!(row.soft_wrapped(), "WRAP_SPACER without soft_wrapped");
                    assert_eq!(cell.extra_id, 0, "WRAP_SPACER must not have extra_id");
                }
            }
        }
    }
}
