//! Terminal grid storage, scrollback ring buffer, row pooling, dirty tracking, and reflow.

pub mod extras;
pub mod reflow;
pub mod row;

use std::collections::VecDeque;

use crate::cell::{Cell, CellExtra, CellFlags};
use crate::color::PackedColor;

pub use self::extras::CellExtraTable;
pub use self::reflow::{ReflowAnchors, ReflowResult};
pub(crate) use self::row::EraseProtection;
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
    #[cfg(any(test, feature = "hyperlinks", feature = "search"))]
    dirty_abs_start: Option<AbsLine>,
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
            #[cfg(any(test, feature = "hyperlinks", feature = "search"))]
            dirty_abs_start: None,
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

    /// Returns the two contiguous slices covering the visible viewport (`0..viewport_rows`)
    /// accounting for `display_offset`, without allocating.
    #[must_use]
    pub fn visible_slices(&self) -> (&[Row], &[Row]) {
        let top = self.history_len().saturating_sub(self.display_offset);
        let end = (top + self.viewport_rows).min(self.rows.len());
        if top >= end {
            return (&[], &[]);
        }
        let (head, tail) = self.rows.as_slices();
        if end <= head.len() {
            (&head[top..end], &[])
        } else if top >= head.len() {
            (&tail[(top - head.len())..(end - head.len())], &[])
        } else {
            (&head[top..], &tail[..(end - head.len())])
        }
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

    /// Returns the per-row [`DirtyTracker`].
    #[must_use]
    pub const fn dirty(&self) -> &DirtyTracker {
        &self.dirty
    }

    /// Clears the [`DirtyTracker`] flags.
    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    /// Records `abs` as mutated so incremental scanners (e.g. `ImplicitLinkStore`) can
    /// start from `min(prev, abs)`.
    #[cfg(any(test, feature = "hyperlinks", feature = "search"))]
    pub(crate) fn mark_dirty_abs(&mut self, abs: AbsLine) {
        self.dirty_abs_start = Some(match self.dirty_abs_start {
            Some(prev) => prev.min(abs),
            None => abs,
        });
    }

    /// No-op when `hyperlinks`, `search`, and `test` are all disabled.
    #[cfg(not(any(test, feature = "hyperlinks", feature = "search")))]
    pub(crate) const fn mark_dirty_abs(&mut self, _abs: AbsLine) {}

    /// Returns the earliest [`AbsLine`] mutated since the last [`take_dirty_abs_start`](Self::take_dirty_abs_start).
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn min_dirty_abs(&self) -> Option<AbsLine> {
        self.dirty_abs_start
    }

    /// Returns and clears the earliest mutated [`AbsLine`] marker.
    #[cfg(any(test, feature = "hyperlinks", feature = "search"))]
    pub(crate) fn take_dirty_abs_start(&mut self) -> Option<AbsLine> {
        self.dirty_abs_start.take()
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
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;

        if let Some(ext) = extra.filter(|e| !e.is_empty()) {
            cell.extra_id = self.extras.insert(ext);
        } else {
            cell.extra_id = 0;
        }

        let row = &mut self.rows[hist + vp_row];
        row.write_single_cell(col, cell, bg, &mut self.extras);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
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
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;

        if let Some(ext) = extra.filter(|e| !e.is_empty()) {
            lead.extra_id = self.extras.insert(ext);
        } else {
            lead.extra_id = 0;
        }

        let row = &mut self.rows[hist + vp_row];
        row.write_wide_pair(col, lead, spacer, bg, &mut self.extras);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
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
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let clean_flags = flags & CellFlags::WRITE_ATTR_MASK;
        let row = &mut self.rows[hist + vp_row];
        row.write_ascii_run(start_col, bytes, fg, bg, clean_flags, &mut self.extras);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
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
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
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
        let spacer = Cell::new(
            ' ',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::WRAP_SPACER,
        );
        let row = &mut self.rows[hist + vp_row];
        row.write_single_cell(last_col, spacer, bg, &mut self.extras);
        row.set_soft_wrapped(true);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
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
        if !self.extras.push_combining_for_cell(cell, ch) {
            row.refresh_trimmed_len();
            return false;
        }
        row.mark_has_extras();
        row.refresh_trimmed_len();
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
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
        let spacer_flags = CellFlags::WIDE_SPACER | (cells[col].flags & CellFlags::WRITE_ATTR_MASK);
        cells[col + 1] = Cell::new(' ', cells[col].fg, cells[col].bg, spacer_flags);
        row.refresh_trimmed_len();
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
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
        row.refresh_trimmed_len();
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
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
        let wrap_spacer = Cell::new(
            ' ',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::WRAP_SPACER,
        );
        row.write_single_cell(col, wrap_spacer, PackedColor::Default, &mut self.extras);
        row.set_soft_wrapped(true);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
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
            CellFlags::WIDE_SPACER | (moved.flags & CellFlags::WRITE_ATTR_MASK),
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
            srow.refresh_trimmed_len();
            srow.set_generation(epoch);
        }

        // Write moved lead + spacer at (dst_row, 0..=1).
        self.damage_epoch += 1;
        let epoch2 = self.damage_epoch;
        {
            let drow = &mut self.rows[hist + dst_row];
            drow.write_wide_pair(0, moved, spacer, bg, &mut self.extras);
            if self.cols == 2 {
                drow.set_soft_wrapped(false);
            }
            drow.set_generation(epoch2);
        }

        self.dirty.mark_row(src_row);
        self.dirty.mark_row(dst_row);
        self.mark_dirty_abs(self.live_viewport_to_abs(src_row.min(dst_row)));
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
                            let Some(mut front) = self.rows.pop_front() else {
                                break;
                            };
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
                            let Some(mut front) = self.rows.pop_front() else {
                                break;
                            };
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
                    // Only the live viewport (not older scrollback) changed/received blank lines.
                    self.mark_dirty_abs(self.live_viewport_to_abs(0));
                    return;
                }
                ScreenKind::Alternate => {
                    for _ in 0..n {
                        self.damage_epoch += 1;
                        let epoch = self.damage_epoch;
                        let Some(mut front) = self.rows.pop_front() else {
                            break;
                        };
                        front.reset_for_reuse(self.cols, fill, epoch, &mut self.extras);
                        self.rows.push_back(front);
                    }
                    self.damage_epoch += 1;
                    let shift_epoch = self.damage_epoch;
                    for row in &mut self.rows {
                        row.set_generation(shift_epoch);
                    }
                    self.dirty.mark_all();
                    self.mark_dirty_abs(self.live_viewport_to_abs(0));
                    return;
                }
            }
        }

        let hist = self.history_len();
        for _ in 0..n {
            self.damage_epoch += 1;
            let epoch = self.damage_epoch;
            let Some(mut row) = self.rows.remove(hist + top) else {
                break;
            };
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
        self.mark_dirty_abs(self.live_viewport_to_abs(top));
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
            let Some(mut row) = self.rows.remove(hist + bottom) else {
                break;
            };
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
        self.mark_dirty_abs(self.live_viewport_to_abs(top));
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
                let mut dst_has_extra = false;
                for col in left..=right {
                    let cell = self.rows[src_idx].cells()[col];
                    if cell.has_extra() {
                        dst_has_extra = true;
                    }
                    self.rows[src_idx].cells_mut()[col] = blank;
                    self.rows[dst_idx].cells_mut()[col] = cell;
                }
                if dst_has_extra {
                    self.rows[dst_idx].mark_has_extras();
                }
                self.rows[src_idx].set_soft_wrapped(false);
                self.rows[src_idx].refresh_trimmed_len();
                self.rows[src_idx].set_generation(epoch);
            } else {
                self.rows[dst_idx].clear_range(left..(right + 1), bg, &mut self.extras);
            }
            self.rows[dst_idx].set_soft_wrapped(false);
            self.rows[dst_idx].refresh_trimmed_len();
            self.rows[dst_idx].set_generation(epoch);
            self.dirty.mark_row(top + offset);
        }
        self.mark_dirty_abs(self.live_viewport_to_abs(top));
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
                let mut dst_has_extra = false;
                for col in left..=right {
                    let cell = self.rows[src_idx].cells()[col];
                    if cell.has_extra() {
                        dst_has_extra = true;
                    }
                    self.rows[src_idx].cells_mut()[col] = blank;
                    self.rows[dst_idx].cells_mut()[col] = cell;
                }
                if dst_has_extra {
                    self.rows[dst_idx].mark_has_extras();
                }
                self.rows[src_idx].set_soft_wrapped(false);
                self.rows[src_idx].refresh_trimmed_len();
                self.rows[src_idx].set_generation(epoch);
            } else {
                self.rows[dst_idx].clear_range(left..(right + 1), bg, &mut self.extras);
            }
            self.rows[dst_idx].set_soft_wrapped(false);
            self.rows[dst_idx].refresh_trimmed_len();
            self.rows[dst_idx].set_generation(epoch);
            self.dirty.mark_row(top + offset);
        }
        self.mark_dirty_abs(self.live_viewport_to_abs(top));
    }

    /// Shifts cells left by `count` columns within `[top..=bottom] x [left..=right]` (`SL`).
    pub(crate) fn shift_left_box(
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
        self.content_epoch += 1;
        let hist = self.history_len();
        for r in top..=bottom {
            self.damage_epoch += 1;
            let epoch = self.damage_epoch;
            let row = &mut self.rows[hist + r];
            row.shift_left_in_range(left, right, count, bg, &mut self.extras);
            row.set_soft_wrapped(false);
            row.set_generation(epoch);
            self.dirty.mark_row(r);
        }
        self.mark_dirty_abs(self.live_viewport_to_abs(top));
    }

    /// Shifts cells right by `count` columns within `[top..=bottom] x [left..=right]` (`SR`).
    pub(crate) fn shift_right_box(
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
        self.content_epoch += 1;
        let hist = self.history_len();
        for r in top..=bottom {
            self.damage_epoch += 1;
            let epoch = self.damage_epoch;
            let row = &mut self.rows[hist + r];
            row.shift_right_in_range(left, right, count, bg, &mut self.extras);
            row.set_soft_wrapped(false);
            row.set_generation(epoch);
            self.dirty.mark_row(r);
        }
        self.mark_dirty_abs(self.live_viewport_to_abs(top));
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
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];
        row.shift_right_in_range(col, right, count, bg, &mut self.extras);
        row.set_soft_wrapped(false);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
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
        let hist = self.history_len();
        self.damage_epoch += 1;
        self.content_epoch += 1;
        let epoch = self.damage_epoch;
        let row = &mut self.rows[hist + vp_row];
        row.shift_left_in_range(col, right, count, bg, &mut self.extras);
        row.set_soft_wrapped(false);
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
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
        self.insert_cells_in_margin(vp_row, col, self.cols - 1, count, bg);
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
        self.delete_cells_in_margin(vp_row, col, self.cols - 1, count, bg);
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
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
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
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
    }

    /// Selectively erases unprotected cells within `vp_row` (`DECSEL`, `CSI ? Ps K`), preserving
    /// visual SGR attributes and `DECSCA`-protected cells.
    pub(crate) fn erase_in_line_selective(
        &mut self,
        vp_row: usize,
        col: usize,
        mode: u16,
        bg: PackedColor,
    ) {
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
        row.clear_range_with_protection(
            range,
            bg,
            EraseProtection::UnprotectedOnly,
            &mut self.extras,
        );
        row.set_generation(epoch);
        self.dirty.mark_row(vp_row);
        self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
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
                self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
            }
            1 => {
                for r in 0..vp_row.min(vrows) {
                    self.damage_epoch += 1;
                    let epoch = self.damage_epoch;
                    self.rows[hist + r].reset_for_reuse(cols, fill, epoch, &mut self.extras);
                    self.dirty.mark_row(r);
                }
                self.erase_in_line(vp_row, col, 1, bg);
                self.mark_dirty_abs(self.live_viewport_to_abs(0));
            }
            2 => self.clear_visible(bg),
            3 => self.clear_history(),
            _ => {}
        }
    }

    /// Selectively erases unprotected cells across the display (`DECSED`, `CSI ? Ps J`, modes `0..=3`).
    ///
    /// Modes `0..=2` operate on the live viewport; mode `3` selectively erases unprotected cells
    /// in scrollback history while keeping `DECSCA`-protected cells and visual SGR attributes intact.
    pub(crate) fn erase_in_display_selective(
        &mut self,
        vp_row: usize,
        col: usize,
        mode: u16,
        bg: PackedColor,
    ) {
        let vrows = self.viewport_rows;
        let cols = self.cols;
        let hist = self.history_len();

        match mode {
            0 => {
                self.erase_in_line_selective(vp_row, col, 0, bg);
                for r in (vp_row + 1)..vrows {
                    self.damage_epoch += 1;
                    let epoch = self.damage_epoch;
                    self.rows[hist + r].clear_range_with_protection(
                        0..cols,
                        bg,
                        EraseProtection::UnprotectedOnly,
                        &mut self.extras,
                    );
                    self.rows[hist + r].set_generation(epoch);
                    self.dirty.mark_row(r);
                }
                self.mark_dirty_abs(self.live_viewport_to_abs(vp_row));
            }
            1 => {
                for r in 0..vp_row.min(vrows) {
                    self.damage_epoch += 1;
                    let epoch = self.damage_epoch;
                    self.rows[hist + r].clear_range_with_protection(
                        0..cols,
                        bg,
                        EraseProtection::UnprotectedOnly,
                        &mut self.extras,
                    );
                    self.rows[hist + r].set_generation(epoch);
                    self.dirty.mark_row(r);
                }
                self.erase_in_line_selective(vp_row, col, 1, bg);
                self.mark_dirty_abs(self.live_viewport_to_abs(0));
            }
            2 => {
                self.content_epoch += 1;
                for r in 0..vrows {
                    self.damage_epoch += 1;
                    let epoch = self.damage_epoch;
                    self.rows[hist + r].clear_range_with_protection(
                        0..cols,
                        bg,
                        EraseProtection::UnprotectedOnly,
                        &mut self.extras,
                    );
                    self.rows[hist + r].set_generation(epoch);
                }
                self.dirty.mark_all();
                self.mark_dirty_abs(self.live_viewport_to_abs(0));
            }
            3 if hist > 0 => {
                self.content_epoch += 1;
                for idx in 0..hist {
                    self.damage_epoch += 1;
                    let epoch = self.damage_epoch;
                    self.rows[idx].clear_range_with_protection(
                        0..cols,
                        bg,
                        EraseProtection::UnprotectedOnly,
                        &mut self.extras,
                    );
                    self.rows[idx].set_generation(epoch);
                }
                if self.display_offset > 0 {
                    self.dirty.mark_all();
                }
                self.mark_dirty_abs(self.oldest_abs_line());
            }
            _ => {}
        }
    }

    /// Copies the rectangular block `[src_top..=src_bottom] x [src_left..=src_right]` to
    /// `(dst_top, dst_left)` safely under overlap (`DECCRA`).
    #[expect(
        clippy::too_many_arguments,
        reason = "VT420 DECCRA rectangular copy parameters"
    )]
    pub(crate) fn copy_rect(
        &mut self,
        src_top: usize,
        src_left: usize,
        src_bottom: usize,
        src_right: usize,
        dst_top: usize,
        dst_left: usize,
        bg: PackedColor,
    ) {
        if src_top > src_bottom
            || src_left > src_right
            || src_top >= self.viewport_rows
            || src_left >= self.cols
            || dst_top >= self.viewport_rows
            || dst_left >= self.cols
        {
            return;
        }
        let src_bottom = src_bottom.min(self.viewport_rows - 1);
        let src_right = src_right.min(self.cols - 1);
        let copy_h = (src_bottom - src_top + 1).min(self.viewport_rows - dst_top);
        let copy_w = (src_right - src_left + 1).min(self.cols - dst_left);
        if copy_h == 0 || copy_w == 0 {
            return;
        }

        let hist = self.history_len();
        let mut snapshot: Vec<(Cell, Option<CellExtra>)> = Vec::with_capacity(copy_h * copy_w);
        for dy in 0..copy_h {
            let srow = &self.rows[hist + src_top + dy];
            let scells = srow.cells();
            for dx in 0..copy_w {
                let mut cell = scells[src_left + dx];
                let split_left = dx == 0 && cell.is_wide_spacer();
                let split_right = dx + 1 == copy_w && cell.is_wide_lead();
                if cell.is_wrap_spacer() || split_left || split_right {
                    cell = Cell::new(
                        ' ',
                        cell.fg,
                        cell.bg,
                        cell.flags & CellFlags::WRITE_ATTR_MASK,
                    );
                    snapshot.push((cell, None));
                } else {
                    let ext = if cell.extra_id != 0 {
                        self.extras.get(cell.extra_id).cloned()
                    } else {
                        None
                    };
                    cell.extra_id = 0;
                    snapshot.push((cell, ext));
                }
            }
        }

        self.content_epoch += 1;
        for dy in 0..copy_h {
            let dst_vp = dst_top + dy;
            self.damage_epoch += 1;
            let epoch = self.damage_epoch;
            let drow = &mut self.rows[hist + dst_vp];

            drow.clear_wide_overlap_at(dst_left, bg, &mut self.extras);
            if copy_w > 1 {
                drow.clear_wide_overlap_at(dst_left + copy_w - 1, bg, &mut self.extras);
            }
            if drow.may_have_extras() {
                for c in dst_left..(dst_left + copy_w) {
                    let eid = drow.cells()[c].extra_id;
                    if eid != 0 {
                        self.extras.free(eid);
                    }
                }
            }

            let mut wrote_extra = false;
            {
                let dcells = drow.cells_mut();
                for dx in 0..copy_w {
                    let entry = &mut snapshot[dy * copy_w + dx];
                    let mut cell = entry.0;
                    if let Some(ext) = entry.1.take().filter(|e| !e.is_empty()) {
                        let new_id = self.extras.insert(ext);
                        cell.extra_id = new_id;
                        if new_id != 0 {
                            wrote_extra = true;
                        }
                    } else {
                        cell.extra_id = 0;
                    }
                    dcells[dst_left + dx] = cell;
                }
            }
            if wrote_extra {
                drow.mark_has_extras();
            }
            if dst_left + copy_w == self.cols {
                drow.set_soft_wrapped(false);
            }
            drow.refresh_trimmed_len();
            drow.set_generation(epoch);
            self.dirty.mark_row(dst_vp);
        }
        self.mark_dirty_abs(self.live_viewport_to_abs(dst_top));
    }

    /// Erases cells in the rectangle `[top..=bottom] x [left..=right]` (`DECERA` / `DECSERA`).
    pub(crate) fn erase_rect(
        &mut self,
        top: usize,
        left: usize,
        bottom: usize,
        right: usize,
        bg: PackedColor,
        protection: EraseProtection,
    ) {
        if top > bottom || left > right || top >= self.viewport_rows || left >= self.cols {
            return;
        }
        let bottom = bottom.min(self.viewport_rows - 1);
        let right = right.min(self.cols - 1);
        let hist = self.history_len();
        self.content_epoch += 1;

        for r in top..=bottom {
            self.damage_epoch += 1;
            let epoch = self.damage_epoch;
            let row = &mut self.rows[hist + r];
            row.clear_range_with_protection(left..(right + 1), bg, protection, &mut self.extras);
            row.set_generation(epoch);
            self.dirty.mark_row(r);
        }
        self.mark_dirty_abs(self.live_viewport_to_abs(top));
    }

    /// Fills `[top..=bottom] x [left..=right]` with `ch` and `fg`/`bg`/`flags` (`DECFRA`).
    #[expect(
        clippy::too_many_arguments,
        reason = "VT420 DECFRA rectangular fill parameters"
    )]
    pub(crate) fn fill_rect(
        &mut self,
        top: usize,
        left: usize,
        bottom: usize,
        right: usize,
        ch: char,
        fg: PackedColor,
        bg: PackedColor,
        flags: CellFlags,
    ) {
        if top > bottom || left > right || top >= self.viewport_rows || left >= self.cols {
            return;
        }
        let bottom = bottom.min(self.viewport_rows - 1);
        let right = right.min(self.cols - 1);
        let clean_flags = flags & CellFlags::WRITE_ATTR_MASK;
        let fill = Cell::new(ch, fg, bg, clean_flags);
        let hist = self.history_len();
        self.content_epoch += 1;

        for r in top..=bottom {
            self.damage_epoch += 1;
            let epoch = self.damage_epoch;
            let row = &mut self.rows[hist + r];
            row.fill_range(left..(right + 1), fill, &mut self.extras);
            row.set_generation(epoch);
            self.dirty.mark_row(r);
        }
        self.mark_dirty_abs(self.live_viewport_to_abs(top));
    }

    /// Fills all visible viewport rows with `'E'` using default attributes (`DECALN`, `ESC # 8`).
    pub(crate) fn fill_alignment_pattern(&mut self) {
        let hist = self.history_len();
        let cols = self.cols;
        let fill = Cell::new(
            'E',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::empty(),
        );
        self.content_epoch += 1;
        for r in 0..self.viewport_rows {
            self.damage_epoch += 1;
            let epoch = self.damage_epoch;
            self.rows[hist + r].reset_for_reuse(cols, fill, epoch, &mut self.extras);
        }
        self.dirty.mark_all();
        self.mark_dirty_abs(self.live_viewport_to_abs(0));
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
        self.mark_dirty_abs(self.live_viewport_to_abs(0));
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
        self.mark_dirty_abs(self.oldest_abs_line());
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
        self.mark_dirty_abs(self.oldest_abs_line());
    }

    /// Clones visible viewport rows and all referenced `CellExtra`s into caller-provided buffers,
    /// reusing their existing heap allocations.
    pub fn clone_visible_snapshot_into(
        &self,
        rows: &mut Vec<Row>,
        extras: &mut Vec<(u32, CellExtra)>,
    ) {
        rows.clear();
        extras.clear();
        let (s0, s1) = self.visible_slices();
        for row in s0.iter().chain(s1.iter()) {
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
        extras.sort_unstable_by_key(|(id, _)| *id);
    }

    /// Clones visible viewport rows and all referenced `CellExtra`s for a DEC Mode 2026
    /// synchronized snapshot.
    #[must_use]
    pub fn clone_visible_snapshot(&self) -> (Vec<Row>, Vec<(u32, CellExtra)>) {
        let mut rows = Vec::with_capacity(self.viewport_rows);
        let mut extras = Vec::new();
        self.clone_visible_snapshot_into(&mut rows, &mut extras);
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
                        cell.flags.remove(
                            CellFlags::WIDE_LEAD | CellFlags::WIDE_SPACER | CellFlags::SIZED_SPACER,
                        );
                        cell.extra_id = 0;
                    } else {
                        cell.flags.remove(CellFlags::WRAP_SPACER);
                    }
                }
            }

            // Pass 2: sanitize mutually-exclusive WIDE_LEAD / WIDE_SPACER / SIZED_SPACER flags and rebuild extras.
            for c in 0..cols {
                if cells[c].is_wrap_spacer() {
                    continue;
                }
                if cells[c].is_wide_spacer() {
                    if c > 0 && cells[c - 1].is_wide_lead() {
                        cells[c].flags.remove(
                            CellFlags::WIDE_LEAD | CellFlags::WRAP_SPACER | CellFlags::SIZED_SPACER,
                        );
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
                        cells[c].flags.remove(
                            CellFlags::WIDE_SPACER
                                | CellFlags::WRAP_SPACER
                                | CellFlags::SIZED_SPACER,
                        );
                    }
                }
                if cells[c].is_sized_spacer() {
                    cells[c].extra_id = 0;
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
            #[cfg(any(test, feature = "hyperlinks", feature = "search"))]
            dirty_abs_start: Some(AbsLine(total_evicted)),
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

    /// Verifies structural `WIDE_LEAD`/`WIDE_SPACER`/`WRAP_SPACER`/`SIZED_SPACER` invariants on every row.
    #[cfg(test)]
    pub(crate) fn debug_verify_wide_invariants(&self) {
        for (r_idx, row) in self.rows.iter().enumerate() {
            let cells = row.cells();
            assert_eq!(cells.len(), self.cols);
            for (c_idx, cell) in cells.iter().enumerate() {
                let structural_count = usize::from(cell.is_wide_lead())
                    + usize::from(cell.is_wide_spacer())
                    + usize::from(cell.is_wrap_spacer())
                    + usize::from(cell.is_sized_spacer());
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
                if cell.is_sized_spacer() {
                    assert_eq!(cell.extra_id, 0, "SIZED_SPACER must not have extra_id");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_ascii_row(grid: &mut Grid, vp_row: usize, text: &str, flags: CellFlags) {
        grid.write_ascii_slice(
            vp_row,
            0,
            text.as_bytes(),
            PackedColor::Default,
            PackedColor::Default,
            flags,
        );
    }

    #[test]
    fn visible_slices_and_snapshot_into_cover_wraparound_and_reuse_capacity() {
        let mut grid = Grid::new(ScreenKind::Primary, 8, 3, 5);
        for i in 0..7 {
            write_ascii_row(&mut grid, 2, &format!("row{i}"), CellFlags::empty());
            grid.scroll_up_region(0, 2, 1, PackedColor::Default, true);
        }
        grid.set_display_offset(2);
        let (s0, s1) = grid.visible_slices();
        assert_eq!(s0.len() + s1.len(), 3);
        for (y, row) in s0.iter().chain(s1.iter()).enumerate() {
            assert_eq!(row, grid.visible_row(y).unwrap());
        }

        let mut rows_buf = Vec::with_capacity(16);
        let mut extras_buf = Vec::with_capacity(8);
        let rows_cap = rows_buf.capacity();
        let extras_cap = extras_buf.capacity();
        grid.clone_visible_snapshot_into(&mut rows_buf, &mut extras_buf);
        assert_eq!(rows_buf.len(), 3);
        assert_eq!(rows_buf.capacity(), rows_cap);
        assert_eq!(extras_buf.capacity(), extras_cap);
    }

    #[test]
    fn dirty_abs_start_tracks_scroll_and_mutations_precisely() {
        let mut grid = Grid::new(ScreenKind::Primary, 10, 4, 10);
        assert_eq!(grid.min_dirty_abs(), None);

        // Multiple unconsumed scroll_up_region calls retain the minimum dirtied
        // live_viewport_to_abs(0) (AbsLine(1) from the first scroll, not oldest_abs_line()).
        for _ in 0..2 {
            grid.scroll_up_region(0, 3, 1, PackedColor::Default, true);
        }
        assert_eq!(grid.history_len(), 2);
        assert_eq!(grid.min_dirty_abs(), Some(AbsLine(1)));
        assert_ne!(grid.min_dirty_abs(), Some(grid.oldest_abs_line()));
        assert_eq!(grid.take_dirty_abs_start(), Some(AbsLine(1)));
        assert_eq!(grid.min_dirty_abs(), None);

        // Primary scroll_up_region with n < viewport_rows dirties live_viewport_to_abs(0), NOT oldest_abs_line().
        grid.scroll_up_region(0, 3, 1, PackedColor::Default, true);
        assert_eq!(grid.history_len(), 3);
        assert_eq!(grid.min_dirty_abs(), Some(grid.live_viewport_to_abs(0)));
        assert_ne!(grid.min_dirty_abs(), Some(grid.oldest_abs_line()));
        assert_eq!(
            grid.take_dirty_abs_start(),
            Some(grid.live_viewport_to_abs(0))
        );
        assert_eq!(grid.min_dirty_abs(), None);

        write_ascii_row(&mut grid, 2, "hello", CellFlags::empty());
        assert_eq!(grid.min_dirty_abs(), Some(grid.live_viewport_to_abs(2)));
        write_ascii_row(&mut grid, 1, "world", CellFlags::empty());
        assert_eq!(grid.min_dirty_abs(), Some(grid.live_viewport_to_abs(1)));
    }

    #[test]
    fn shift_left_right_box_preserves_margins_and_wide_invariants() {
        let mut grid = Grid::new(ScreenKind::Primary, 10, 3, 10);
        write_ascii_row(&mut grid, 0, "ABCDEFGHIJ", CellFlags::empty());
        write_ascii_row(&mut grid, 1, "0123456789", CellFlags::empty());

        grid.shift_left_box(0, 1, 2, 7, 2, PackedColor::indexed(1));
        assert_eq!(
            grid.viewport_row(0).unwrap().text(grid.extras()),
            "ABEFGH  IJ"
        );
        assert_eq!(
            grid.viewport_row(1).unwrap().text(grid.extras()),
            "014567  89"
        );
        assert_eq!(
            grid.viewport_row(0).unwrap().cells()[6].bg,
            PackedColor::indexed(1)
        );

        grid.shift_right_box(0, 0, 2, 7, 1, PackedColor::indexed(2));
        assert_eq!(
            grid.viewport_row(0).unwrap().text(grid.extras()),
            "AB EFGH IJ"
        );
        assert_eq!(
            grid.viewport_row(0).unwrap().cells()[2].bg,
            PackedColor::indexed(2)
        );

        // Wide character straddling box boundary is cleaned without orphan spacers.
        grid.write_wide_cell(
            2,
            1,
            Cell::new(
                '中',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::WIDE_LEAD,
            ),
            Cell::new(
                ' ',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::WIDE_SPACER,
            ),
            None,
            PackedColor::Default,
        );
        grid.shift_left_box(2, 2, 2, 6, 1, PackedColor::Default);
        grid.debug_verify_wide_invariants();
        grid.debug_verify_extra_invariants();
    }

    #[test]
    fn selective_erase_line_and_display_respect_protected_and_scrollback() {
        let mut grid = Grid::new(ScreenKind::Primary, 8, 2, 4);
        // Push one row into scrollback with protected + unprotected cells.
        grid.write_ascii_slice(
            0,
            0,
            b"PROT",
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::PROTECTED,
        );
        grid.write_ascii_slice(
            0,
            4,
            b"free",
            PackedColor::indexed(3),
            PackedColor::indexed(4),
            CellFlags::BOLD,
        );
        grid.scroll_up_region(0, 1, 1, PackedColor::Default, true);
        grid.scroll_up_region(0, 1, 1, PackedColor::Default, true);
        assert_eq!(grid.history_len(), 2);

        // Viewport row 0: protected "AA", unprotected styled "BB", protected "CC", unprotected "DD".
        grid.write_ascii_slice(
            0,
            0,
            b"AA",
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::PROTECTED,
        );
        grid.write_ascii_slice(
            0,
            2,
            b"BB",
            PackedColor::indexed(1),
            PackedColor::indexed(2),
            CellFlags::BOLD,
        );
        grid.write_ascii_slice(
            0,
            4,
            b"CC",
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::PROTECTED,
        );
        grid.write_ascii_slice(
            0,
            6,
            b"DD",
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::empty(),
        );

        grid.erase_in_line_selective(0, 2, 0, PackedColor::Default);
        assert_eq!(grid.viewport_row(0).unwrap().text(grid.extras()), "AA  CC");
        let erased = grid.viewport_row(0).unwrap().cells()[2];
        assert_eq!(erased.codepoint, ' ');
        assert_eq!(erased.fg, PackedColor::indexed(1));
        assert_eq!(erased.bg, PackedColor::indexed(2));
        assert!(erased.flags.contains(CellFlags::BOLD));

        // Mode 3 selectively erases scrollback without touching protected "PROT".
        grid.erase_in_display_selective(0, 0, 3, PackedColor::Default);
        let sb_row = grid.row_by_buf(0).unwrap();
        assert_eq!(sb_row.text(grid.extras()), "PROT    ");
        assert_eq!(sb_row.cells()[5].bg, PackedColor::indexed(4));
        grid.debug_verify_extra_invariants();
        grid.debug_verify_wide_invariants();
    }

    #[test]
    fn copy_erase_fill_rect_and_alignment_pattern() {
        let mut grid = Grid::new(ScreenKind::Primary, 8, 4, 4);
        write_ascii_row(&mut grid, 0, "ABCD1234", CellFlags::empty());
        write_ascii_row(&mut grid, 1, "EFGH5678", CellFlags::empty());
        write_ascii_row(&mut grid, 2, "IJKL9012", CellFlags::empty());
        grid.write_wide_cell(
            0,
            1,
            Cell::new(
                '中',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::WIDE_LEAD,
            ),
            Cell::new(
                ' ',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::WIDE_SPACER,
            ),
            Some(CellExtra {
                combining: vec!['\u{0301}'],
                underline_color: Some(PackedColor::rgb(255, 0, 0)),
                hyperlink_id: None,
                text_sizing: None,
            }),
            PackedColor::Default,
        );

        // Overlapping copy (0..=1, 0..=3) -> (1, 1) duplicates extra and normalizes wide edges.
        grid.copy_rect(0, 0, 1, 3, 1, 1, PackedColor::Default);
        grid.debug_verify_extra_invariants();
        grid.debug_verify_wide_invariants();
        assert_eq!(grid.extras().live_count(), 2);

        // DECFRA fill_rect and DECERA / DECSERA erase_rect.
        grid.fill_rect(
            2,
            2,
            3,
            5,
            '#',
            PackedColor::indexed(5),
            PackedColor::Default,
            CellFlags::PROTECTED,
        );
        assert_eq!(grid.viewport_row(2).unwrap().cells()[3].codepoint, '#');
        assert!(grid.viewport_row(2).unwrap().cells()[3].is_protected());

        // Selective erase_rect leaves protected '#'.
        grid.erase_rect(
            2,
            2,
            3,
            5,
            PackedColor::Default,
            EraseProtection::UnprotectedOnly,
        );
        assert_eq!(grid.viewport_row(2).unwrap().cells()[3].codepoint, '#');

        // Full erase_rect clears protected '#'.
        grid.erase_rect(2, 2, 3, 5, PackedColor::Default, EraseProtection::All);
        assert_eq!(grid.viewport_row(2).unwrap().cells()[3].codepoint, ' ');
        assert!(!grid.viewport_row(2).unwrap().cells()[3].is_protected());

        // DECALN fills viewport with 'E'.
        grid.fill_alignment_pattern();
        for r in 0..4 {
            assert_eq!(
                grid.viewport_row(r).unwrap().text(grid.extras()),
                "EEEEEEEE"
            );
        }
        assert_eq!(grid.extras().live_count(), 0);
        grid.debug_verify_extra_invariants();
        grid.debug_verify_wide_invariants();
    }
}
