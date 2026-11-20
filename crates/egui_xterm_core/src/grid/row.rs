//! Terminal grid row storage and cell/wide-char/extra-cleanup helpers.

use std::ops::Range;

use crate::cell::{Cell, CellFlags};
use crate::color::PackedColor;
use crate::grid::extras::CellExtraTable;

/// A single physical row in the terminal grid.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Row {
    cells: Vec<Cell>,
    soft_wrapped: bool,
    generation: u64,
    /// Conservative flag: `false` guarantees every cell in `cells` has `extra_id == 0`.
    may_have_extras: bool,
}

impl Row {
    /// Creates a blank row of `cols` cells with the given `generation`.
    #[must_use]
    pub fn new(cols: usize, generation: u64) -> Self {
        Self {
            cells: vec![Cell::BLANK; cols],
            soft_wrapped: false,
            generation,
            may_have_extras: false,
        }
    }

    /// Creates a row filled with `fill` cells (must have `fill.extra_id == 0`).
    #[must_use]
    pub(crate) fn filled(cols: usize, fill: Cell, generation: u64) -> Self {
        debug_assert_eq!(fill.extra_id, 0);
        Self {
            cells: vec![fill; cols],
            soft_wrapped: false,
            generation,
            may_have_extras: false,
        }
    }

    /// Returns an immutable slice of cells in this row.
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Returns a mutable slice of cells in this row.
    pub(crate) fn cells_mut(&mut self) -> &mut [Cell] {
        &mut self.cells
    }

    /// Returns the number of columns in this row.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Returns `true` if this row has zero columns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Returns `true` if this row wrapped softly onto the following physical row.
    #[must_use]
    pub const fn soft_wrapped(&self) -> bool {
        self.soft_wrapped
    }

    /// Updates the soft-wrap flag on this row.
    pub(crate) fn set_soft_wrapped(&mut self, wrapped: bool) {
        self.soft_wrapped = wrapped;
    }

    /// Returns the monotonic mutation generation counter for this row.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Sets the generation counter on this row.
    pub(crate) fn set_generation(&mut self, generation: u64) {
        self.generation = generation;
    }

    /// Returns `true` if any cell in this row may reference [`CellExtraTable`].
    #[must_use]
    pub const fn may_have_extras(&self) -> bool {
        self.may_have_extras
    }

    /// Marks this row as potentially holding non-zero `extra_id` references.
    pub(crate) fn mark_has_extras(&mut self) {
        self.may_have_extras = true;
    }

    /// Clears `may_have_extras` when all `extra_id`s have been moved elsewhere.
    pub(crate) fn clear_may_have_extras(&mut self) {
        self.may_have_extras = false;
    }

    /// Returns the index after the last non-empty cell (`0` if all cells are [`Cell::is_empty`]).
    #[must_use]
    pub fn trimmed_len(&self) -> usize {
        self.cells
            .iter()
            .rposition(|c| !c.is_empty())
            .map_or(0, |idx| idx + 1)
    }

    /// Returns `true` if the row is not soft-wrapped and all cells are empty.
    #[must_use]
    pub fn is_completely_empty(&self) -> bool {
        !self.soft_wrapped && self.trimmed_len() == 0
    }

    /// Extracts trimmed plain text for this row, omitting `WIDE_SPACER` and `WRAP_SPACER` cells
    /// and appending combining marks from `extras`.
    #[must_use]
    pub fn text(&self, extras: &CellExtraTable) -> String {
        let mut out = String::new();
        let end = self.trimmed_len();
        for cell in &self.cells[..end] {
            if cell.is_wide_spacer() || cell.is_wrap_spacer() {
                continue;
            }
            if cell.extra_id != 0
                && let Some(extra) = extras.get(cell.extra_id)
            {
                extra.write_grapheme(cell.codepoint, &mut out);
                continue;
            }
            out.push(cell.codepoint);
        }
        out
    }

    /// Frees all `CellExtra` entries referenced by cells in this row.
    pub(crate) fn free_all_extras(&mut self, extras: &mut CellExtraTable) {
        if !self.may_have_extras {
            return;
        }
        for cell in &mut self.cells {
            if cell.extra_id != 0 {
                extras.free(cell.extra_id);
                cell.extra_id = 0;
            }
        }
        self.may_have_extras = false;
    }

    /// Resets this row in place for reuse from a row pool or scroll eviction.
    pub(crate) fn reset_for_reuse(
        &mut self,
        cols: usize,
        fill: Cell,
        generation: u64,
        extras: &mut CellExtraTable,
    ) {
        debug_assert_eq!(fill.extra_id, 0);
        self.free_all_extras(extras);
        self.cells.resize(cols, fill);
        self.cells.fill(fill);
        self.soft_wrapped = false;
        self.generation = generation;
        self.may_have_extras = false;
    }

    /// Cleans up a split double-width glyph if `col` points to either half (`WIDE_LEAD` or `WIDE_SPACER`).
    pub(crate) fn clear_wide_overlap_at(
        &mut self,
        col: usize,
        bg: PackedColor,
        extras: &mut CellExtraTable,
    ) {
        let cols = self.cells.len();
        if col >= cols {
            return;
        }
        if self.cells[col].is_wide_spacer() {
            if col > 0 && self.cells[col - 1].is_wide_lead() {
                if self.may_have_extras && self.cells[col - 1].extra_id != 0 {
                    extras.free(self.cells[col - 1].extra_id);
                }
                self.cells[col - 1].reset(bg);
            }
            if self.may_have_extras && self.cells[col].extra_id != 0 {
                extras.free(self.cells[col].extra_id);
            }
            self.cells[col].reset(bg);
        } else if self.cells[col].is_wide_lead() {
            if self.may_have_extras && self.cells[col].extra_id != 0 {
                extras.free(self.cells[col].extra_id);
            }
            self.cells[col].reset(bg);
            if col + 1 < cols && self.cells[col + 1].is_wide_spacer() {
                if self.may_have_extras && self.cells[col + 1].extra_id != 0 {
                    extras.free(self.cells[col + 1].extra_id);
                }
                self.cells[col + 1].reset(bg);
            }
        }
    }

    /// Erases `range` to BCE blanks with `bg`, reclaiming any `CellExtra`s and cleaning split
    /// wide-char pairs at the boundaries.
    pub(crate) fn clear_range(
        &mut self,
        range: Range<usize>,
        bg: PackedColor,
        extras: &mut CellExtraTable,
    ) {
        let cols = self.cells.len();
        let start = range.start.min(cols);
        let end = range.end.min(cols);
        if start >= end {
            return;
        }

        if start > 0 && self.cells[start].is_wide_spacer() {
            self.clear_wide_overlap_at(start, bg, extras);
        }
        if end < cols && self.cells[end - 1].is_wide_lead() {
            self.clear_wide_overlap_at(end - 1, bg, extras);
        }

        if self.may_have_extras {
            for cell in &mut self.cells[start..end] {
                if cell.extra_id != 0 {
                    extras.free(cell.extra_id);
                }
                cell.reset(bg);
            }
        } else {
            let blank = Cell::new(' ', PackedColor::Default, bg, CellFlags::empty());
            self.cells[start..end].fill(blank);
        }

        if end == cols {
            self.soft_wrapped = false;
        }
    }

    /// Resizes this row without reflow (used by alternate screen), reclaiming cropped extras and
    /// cleaning any wide character severed at the right edge.
    pub(crate) fn resize_cols(
        &mut self,
        new_cols: usize,
        generation: u64,
        extras: &mut CellExtraTable,
    ) {
        let old_cols = self.cells.len();
        if new_cols < old_cols {
            if self.may_have_extras {
                for cell in &mut self.cells[new_cols..] {
                    if cell.extra_id != 0 {
                        extras.free(cell.extra_id);
                        cell.extra_id = 0;
                    }
                }
            }
            self.cells.truncate(new_cols);
            if new_cols > 0 {
                let last = new_cols - 1;
                if self.cells[last].is_wide_lead() {
                    if self.may_have_extras && self.cells[last].extra_id != 0 {
                        extras.free(self.cells[last].extra_id);
                    }
                    self.cells[last].reset(PackedColor::Default);
                }
                if self.cells[last].is_wrap_spacer() {
                    self.cells[last].reset(PackedColor::Default);
                }
            }
            self.soft_wrapped = false;
        } else if new_cols > old_cols {
            if old_cols > 0 && self.cells[old_cols - 1].is_wrap_spacer() {
                self.cells[old_cols - 1].reset(PackedColor::Default);
            }
            self.soft_wrapped = false;
            self.cells.resize(new_cols, Cell::BLANK);
        }
        self.generation = generation;
    }

    /// Replaces internal cell vector (used by reflow).
    pub(crate) fn replace_cells(&mut self, cells: Vec<Cell>, soft_wrapped: bool, generation: u64) {
        self.may_have_extras = cells.iter().any(Cell::has_extra);
        self.cells = cells;
        self.soft_wrapped = soft_wrapped;
        self.generation = generation;
    }

    /// Takes ownership of the underlying `Vec<Cell>` buffer, resetting the row to empty.
    pub(crate) fn take_cells(&mut self) -> Vec<Cell> {
        self.may_have_extras = false;
        self.soft_wrapped = false;
        std::mem::take(&mut self.cells)
    }
}
