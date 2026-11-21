//! Terminal grid row storage and cell/wide-char/extra-cleanup helpers.

use std::ops::Range;

use crate::cell::{Cell, CellFlags};
use crate::color::PackedColor;
use crate::grid::extras::CellExtraTable;

const UNKNOWN_TRIMMED_LEN: u32 = u32::MAX;

#[cfg(feature = "serde")]
const fn unknown_trimmed_len() -> u32 {
    UNKNOWN_TRIMMED_LEN
}

/// Controls whether erase operations respect `DECSCA` character protection (`CellFlags::PROTECTED`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EraseProtection {
    /// Erase all cells in range, clearing `CellFlags::PROTECTED` as well (`ED`/`EL`/`DECERA`).
    All,
    /// Erase only cells without `CellFlags::PROTECTED` (`DECSED`/`DECSEL`/`DECSERA`).
    UnprotectedOnly,
}

/// A single physical row in the terminal grid.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Row {
    cells: Vec<Cell>,
    soft_wrapped: bool,
    generation: u64,
    /// Conservative flag: `false` guarantees every cell in `cells` has `extra_id == 0`.
    may_have_extras: bool,
    /// Cached `trimmed_len()` or [`UNKNOWN_TRIMMED_LEN`] when invalidated.
    #[cfg_attr(feature = "serde", serde(skip, default = "unknown_trimmed_len"))]
    trimmed_len_cache: u32,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Row>() == 40);

impl PartialEq for Row {
    fn eq(&self, other: &Self) -> bool {
        self.soft_wrapped == other.soft_wrapped
            && self.generation == other.generation
            && self.may_have_extras == other.may_have_extras
            && self.cells == other.cells
    }
}

impl Eq for Row {}

impl Row {
    const fn encode_trimmed_len(len: usize) -> u32 {
        if len < UNKNOWN_TRIMMED_LEN as usize {
            len as u32
        } else {
            UNKNOWN_TRIMMED_LEN
        }
    }

    /// Creates a blank row of `cols` cells with the given `generation`.
    #[must_use]
    pub fn new(cols: usize, generation: u64) -> Self {
        Self {
            cells: vec![Cell::BLANK; cols],
            soft_wrapped: false,
            generation,
            may_have_extras: false,
            trimmed_len_cache: 0,
        }
    }

    /// Creates a row filled with `fill` cells (must have `fill.extra_id == 0`).
    #[must_use]
    pub(crate) fn filled(cols: usize, fill: Cell, generation: u64) -> Self {
        debug_assert_eq!(fill.extra_id, 0);
        let trimmed_len_cache = if fill.is_empty() {
            0
        } else {
            Self::encode_trimmed_len(cols)
        };
        Self {
            cells: vec![fill; cols],
            soft_wrapped: false,
            generation,
            may_have_extras: false,
            trimmed_len_cache,
        }
    }

    /// Constructs a [`Row`] directly from pre-built components and a known `trimmed_len`.
    #[must_use]
    pub(crate) fn from_parts(
        cells: Vec<Cell>,
        soft_wrapped: bool,
        generation: u64,
        may_have_extras: bool,
        trimmed_len: usize,
    ) -> Self {
        let clamped = trimmed_len.min(cells.len());
        Self {
            cells,
            soft_wrapped,
            generation,
            may_have_extras,
            trimmed_len_cache: Self::encode_trimmed_len(clamped),
        }
    }

    /// Returns an immutable slice of cells in this row.
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Returns a mutable slice of cells in this row, invalidating `trimmed_len_cache` defensively.
    pub(crate) fn cells_mut(&mut self) -> &mut [Cell] {
        self.trimmed_len_cache = UNKNOWN_TRIMMED_LEN;
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

    #[must_use]
    fn compute_trimmed_len(&self, limit: usize) -> usize {
        let end = limit.min(self.cells.len());
        self.cells[..end]
            .iter()
            .rposition(|c| !c.is_empty())
            .map_or(0, |idx| idx + 1)
    }

    /// Recomputes and stores `trimmed_len_cache` after direct slice edits.
    pub(crate) fn refresh_trimmed_len(&mut self) {
        let len = self.compute_trimmed_len(self.cells.len());
        self.trimmed_len_cache = Self::encode_trimmed_len(len);
    }

    /// Returns the index after the last non-empty cell (`0` if all cells are [`Cell::is_empty`]).
    #[must_use]
    pub fn trimmed_len(&self) -> usize {
        if self.trimmed_len_cache != UNKNOWN_TRIMMED_LEN {
            return (self.trimmed_len_cache as usize).min(self.cells.len());
        }
        self.compute_trimmed_len(self.cells.len())
    }

    /// Returns `true` if the row is not soft-wrapped and all cells are empty.
    #[must_use]
    pub fn is_completely_empty(&self) -> bool {
        !self.soft_wrapped && self.trimmed_len() == 0
    }

    /// Extracts trimmed plain text for this row, omitting `WIDE_SPACER`, `WRAP_SPACER`, and
    /// `SIZED_SPACER` cells and appending combining marks from `extras`.
    #[must_use]
    pub fn text(&self, extras: &CellExtraTable) -> String {
        let mut out = String::new();
        let end = self.trimmed_len();
        for cell in &self.cells[..end] {
            if cell.is_wide_spacer() || cell.is_wrap_spacer() || cell.is_sized_spacer() {
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
        self.trimmed_len_cache = if fill.is_empty() {
            0
        } else {
            Self::encode_trimmed_len(cols)
        };
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
        let mut modified = false;
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
            modified = true;
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
            modified = true;
        }
        if modified && self.trimmed_len_cache != UNKNOWN_TRIMMED_LEN {
            let old = (self.trimmed_len_cache as usize).min(cols);
            if bg.is_default() {
                if col + 2 >= old {
                    self.trimmed_len_cache =
                        Self::encode_trimmed_len(self.compute_trimmed_len(old));
                }
            } else {
                let touched_end = (col + 2).min(cols);
                self.trimmed_len_cache = Self::encode_trimmed_len(old.max(touched_end));
            }
        }
    }

    /// Writes a single cell at `col`, cleaning wide overlaps, freeing any overwritten `extra_id`,
    /// and updating `trimmed_len_cache` in O(1) (rescanning only when erasing at the trimmed tail).
    pub(crate) fn write_single_cell(
        &mut self,
        col: usize,
        cell: Cell,
        bg: PackedColor,
        extras: &mut CellExtraTable,
    ) {
        let cols = self.cells.len();
        if col >= cols {
            return;
        }
        let was_wide_lead = self.cells[col].is_wide_lead();
        let old_cache = self.trimmed_len_cache;
        self.clear_wide_overlap_at(col, bg, extras);
        if self.may_have_extras && self.cells[col].extra_id != 0 {
            extras.free(self.cells[col].extra_id);
        }
        if cell.has_extra() {
            self.may_have_extras = true;
        }
        self.cells[col] = cell;
        if col + 1 == cols && !cell.is_wrap_spacer() {
            self.soft_wrapped = false;
        }

        if old_cache != UNKNOWN_TRIMMED_LEN {
            let old = (old_cache as usize).min(cols);
            if !cell.is_empty() {
                if was_wide_lead && col + 2 == old && bg.is_default() {
                    self.trimmed_len_cache = Self::encode_trimmed_len(col + 1);
                } else {
                    self.trimmed_len_cache = Self::encode_trimmed_len(old.max(col + 1));
                }
            } else if col + 1 < old && !(was_wide_lead && col + 2 == old && bg.is_default()) {
                self.trimmed_len_cache = Self::encode_trimmed_len(old);
            } else {
                let limit = old.min(cols);
                self.trimmed_len_cache = Self::encode_trimmed_len(self.compute_trimmed_len(limit));
            }
        } else if !cell.is_empty() && col + 1 == cols {
            self.trimmed_len_cache = Self::encode_trimmed_len(cols);
        }
    }

    /// Writes a double-width lead + spacer pair at `col..col + 2`, updating `trimmed_len_cache`.
    pub(crate) fn write_wide_pair(
        &mut self,
        col: usize,
        lead: Cell,
        spacer: Cell,
        bg: PackedColor,
        extras: &mut CellExtraTable,
    ) {
        let cols = self.cells.len();
        if col + 1 >= cols {
            return;
        }
        let was_col1_wide_lead = self.cells[col + 1].is_wide_lead();
        let old_cache = self.trimmed_len_cache;

        self.clear_wide_overlap_at(col, bg, extras);
        self.clear_wide_overlap_at(col + 1, bg, extras);

        if self.may_have_extras {
            if self.cells[col].extra_id != 0 {
                extras.free(self.cells[col].extra_id);
            }
            if self.cells[col + 1].extra_id != 0 {
                extras.free(self.cells[col + 1].extra_id);
            }
        }
        if lead.has_extra() || spacer.has_extra() {
            self.may_have_extras = true;
        }
        self.cells[col] = lead;
        self.cells[col + 1] = spacer;
        if col + 2 == cols {
            self.soft_wrapped = false;
        }

        if old_cache != UNKNOWN_TRIMMED_LEN {
            let old = (old_cache as usize).min(cols);
            if was_col1_wide_lead && col + 3 == old && bg.is_default() {
                self.trimmed_len_cache = Self::encode_trimmed_len(col + 2);
            } else {
                self.trimmed_len_cache = Self::encode_trimmed_len(old.max(col + 2));
            }
        } else if col + 2 == cols {
            self.trimmed_len_cache = Self::encode_trimmed_len(cols);
        }
    }

    /// Batch-writes printable ASCII `bytes` starting at `start_col` with `clean_flags`,
    /// maintaining `trimmed_len_cache` in O(1) for styled runs or scanning only `bytes`.
    pub(crate) fn write_ascii_run(
        &mut self,
        start_col: usize,
        bytes: &[u8],
        fg: PackedColor,
        bg: PackedColor,
        clean_flags: CellFlags,
        extras: &mut CellExtraTable,
    ) {
        let cols = self.cells.len();
        if bytes.is_empty() || start_col >= cols {
            return;
        }
        let end_col = (start_col + bytes.len()).min(cols);
        let write_len = end_col - start_col;
        let was_end_wide_lead = end_col < cols && self.cells[end_col - 1].is_wide_lead();
        let old_cache = self.trimmed_len_cache;

        self.clear_wide_overlap_at(start_col, bg, extras);
        if write_len > 1 {
            self.clear_wide_overlap_at(end_col - 1, bg, extras);
        }

        if self.may_have_extras {
            for cell in &mut self.cells[start_col..end_col] {
                if cell.extra_id != 0 {
                    extras.free(cell.extra_id);
                }
            }
        }

        for (dst, &b) in self.cells[start_col..end_col]
            .iter_mut()
            .zip(&bytes[..write_len])
        {
            *dst = Cell::new(char::from(b), fg, bg, clean_flags);
        }

        if end_col == cols {
            self.soft_wrapped = false;
        }

        if old_cache != UNKNOWN_TRIMMED_LEN {
            let old = (old_cache as usize).min(cols);
            if !fg.is_default() || !bg.is_default() || !clean_flags.is_empty() {
                let next = if was_end_wide_lead && end_col + 1 == old && bg.is_default() {
                    end_col
                } else {
                    old.max(end_col)
                };
                self.trimmed_len_cache = Self::encode_trimmed_len(next);
            } else if let Some(rel) = bytes[..write_len].iter().rposition(|&b| b != b' ') {
                let last_non_blank = start_col + rel + 1;
                let next = if end_col >= old || (was_end_wide_lead && end_col + 1 == old) {
                    last_non_blank
                } else {
                    old.max(last_non_blank)
                };
                self.trimmed_len_cache = Self::encode_trimmed_len(next);
            } else if end_col < old && !(was_end_wide_lead && end_col + 1 == old) {
                self.trimmed_len_cache = Self::encode_trimmed_len(old);
            } else {
                let next = self.compute_trimmed_len(start_col.min(old));
                self.trimmed_len_cache = Self::encode_trimmed_len(next);
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
        self.clear_range_with_protection(range, bg, EraseProtection::All, extras);
    }

    /// Erases `range` respecting `protection`, preserving visual SGR attributes and `underline_color`
    /// under [`EraseProtection::UnprotectedOnly`].
    pub(crate) fn clear_range_with_protection(
        &mut self,
        range: Range<usize>,
        bg: PackedColor,
        protection: EraseProtection,
        extras: &mut CellExtraTable,
    ) {
        let cols = self.cells.len();
        let start = range.start.min(cols);
        let end = range.end.min(cols);
        if start >= end {
            return;
        }

        let old_cache = self.trimmed_len_cache;

        match protection {
            EraseProtection::All => {
                let was_end_wide_lead = end < cols && self.cells[end - 1].is_wide_lead();
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

                if old_cache != UNKNOWN_TRIMMED_LEN {
                    let old = (old_cache as usize).min(cols);
                    if !bg.is_default() {
                        self.trimmed_len_cache = Self::encode_trimmed_len(old.max(end));
                    } else if end < old && !(was_end_wide_lead && end + 1 == old) {
                        self.trimmed_len_cache = Self::encode_trimmed_len(old);
                    } else {
                        let next = self.compute_trimmed_len(start.min(old));
                        self.trimmed_len_cache = Self::encode_trimmed_len(next);
                    }
                }
            }
            EraseProtection::UnprotectedOnly => {
                if start > 0
                    && self.cells[start].is_wide_spacer()
                    && !self.cells[start].is_protected()
                {
                    if self.cells[start - 1].is_wide_lead() {
                        if !self.cells[start - 1].is_protected() {
                            Self::selective_erase_cell(&mut self.cells[start - 1], extras);
                        } else {
                            self.cells[start - 1].flags.remove(CellFlags::WIDE_LEAD);
                        }
                    }
                    Self::selective_erase_cell(&mut self.cells[start], extras);
                }

                if end < cols
                    && self.cells[end - 1].is_wide_lead()
                    && !self.cells[end - 1].is_protected()
                    && self.cells[end].is_wide_spacer()
                {
                    if !self.cells[end].is_protected() {
                        Self::selective_erase_cell(&mut self.cells[end], extras);
                    } else {
                        self.cells[end].flags.remove(CellFlags::WIDE_SPACER);
                    }
                }

                for cell in &mut self.cells[start..end] {
                    if !cell.is_protected() {
                        Self::selective_erase_cell(cell, extras);
                    }
                }

                if end == cols && !self.cells[cols - 1].is_protected() {
                    self.soft_wrapped = false;
                }

                if old_cache != UNKNOWN_TRIMMED_LEN {
                    let old = (old_cache as usize).min(cols);
                    if end + 1 < old {
                        self.trimmed_len_cache = Self::encode_trimmed_len(old);
                    } else {
                        let next = self.compute_trimmed_len(old);
                        self.trimmed_len_cache = Self::encode_trimmed_len(next);
                    }
                }
            }
        }
    }

    fn selective_erase_cell(cell: &mut Cell, extras: &mut CellExtraTable) {
        cell.codepoint = ' ';
        cell.flags &= CellFlags::SGR_MASK;
        if cell.extra_id != 0 {
            extras.clear_non_underline_for_selective_erase(cell);
        }
    }

    /// Shifts cells left by `count` columns inside `left..=right`, filling vacated right columns
    /// with BCE blanks (`bg`).
    pub(crate) fn shift_left_in_range(
        &mut self,
        left: usize,
        right: usize,
        count: usize,
        bg: PackedColor,
        extras: &mut CellExtraTable,
    ) {
        let cols = self.cells.len();
        if cols == 0 {
            return;
        }
        let right = right.min(cols - 1);
        if left > right || count == 0 {
            return;
        }
        let region_w = right - left + 1;
        let n = count.min(region_w);
        let old_cache = self.trimmed_len_cache;
        let was_right_wide_lead = right + 1 < cols && self.cells[right].is_wide_lead();

        if right + 1 == cols && self.cells[right].is_wrap_spacer() {
            self.cells[right].reset(bg);
            self.soft_wrapped = false;
        }
        if left > 0 && self.cells[left].is_wide_spacer() {
            self.clear_wide_overlap_at(left, bg, extras);
        }
        if right + 1 < cols && self.cells[right].is_wide_lead() {
            self.clear_wide_overlap_at(right, bg, extras);
        }
        if n < region_w && self.cells[left + n].is_wide_spacer() {
            self.clear_wide_overlap_at(left + n, bg, extras);
        }

        if self.may_have_extras {
            for cell in &mut self.cells[left..(left + n)] {
                if cell.extra_id != 0 {
                    extras.free(cell.extra_id);
                    cell.extra_id = 0;
                }
            }
        }

        if n < region_w {
            self.cells.copy_within((left + n)..=right, left);
        }
        let blank = Cell::new(' ', PackedColor::Default, bg, CellFlags::empty());
        self.cells[(right + 1 - n)..=right].fill(blank);

        if right + 1 == cols {
            self.soft_wrapped = false;
        }

        if old_cache != UNKNOWN_TRIMMED_LEN {
            let old = (old_cache as usize).min(cols);
            if right + 1 < old && !(was_right_wide_lead && right + 2 == old && bg.is_default()) {
                self.trimmed_len_cache = Self::encode_trimmed_len(old);
            } else if !bg.is_default() {
                self.trimmed_len_cache = Self::encode_trimmed_len(old.max(right + 1));
            } else {
                let next = self.compute_trimmed_len(old);
                self.trimmed_len_cache = Self::encode_trimmed_len(next);
            }
        }
    }

    /// Shifts cells right by `count` columns inside `left..=right`, filling vacated left columns
    /// with BCE blanks (`bg`).
    pub(crate) fn shift_right_in_range(
        &mut self,
        left: usize,
        right: usize,
        count: usize,
        bg: PackedColor,
        extras: &mut CellExtraTable,
    ) {
        let cols = self.cells.len();
        if cols == 0 {
            return;
        }
        let right = right.min(cols - 1);
        if left > right || count == 0 {
            return;
        }
        let region_w = right - left + 1;
        let n = count.min(region_w);
        let split = right + 1 - n;
        let old_cache = self.trimmed_len_cache;
        let was_right_wide_lead = right + 1 < cols && self.cells[right].is_wide_lead();

        if right + 1 == cols && self.cells[right].is_wrap_spacer() {
            self.cells[right].reset(bg);
            self.soft_wrapped = false;
        }
        if left > 0 && self.cells[left].is_wide_spacer() {
            self.clear_wide_overlap_at(left, bg, extras);
        }
        if right + 1 < cols && self.cells[right].is_wide_lead() {
            self.clear_wide_overlap_at(right, bg, extras);
        }
        if split > left && self.cells[split - 1].is_wide_lead() {
            self.clear_wide_overlap_at(split - 1, bg, extras);
        }

        if self.may_have_extras {
            for cell in &mut self.cells[split..=right] {
                if cell.extra_id != 0 {
                    extras.free(cell.extra_id);
                    cell.extra_id = 0;
                }
            }
        }

        if split > left {
            self.cells.copy_within(left..split, left + n);
        }
        let blank = Cell::new(' ', PackedColor::Default, bg, CellFlags::empty());
        self.cells[left..(left + n)].fill(blank);

        if right + 1 == cols {
            self.soft_wrapped = false;
        }

        if old_cache != UNKNOWN_TRIMMED_LEN {
            let old = (old_cache as usize).min(cols);
            if right + 1 < old && !(was_right_wide_lead && right + 2 == old && bg.is_default()) {
                self.trimmed_len_cache = Self::encode_trimmed_len(old);
            } else if !bg.is_default() {
                self.trimmed_len_cache = Self::encode_trimmed_len(old.max(right + 1));
            } else if old <= left {
                self.trimmed_len_cache = Self::encode_trimmed_len(old);
            } else {
                let limit = (old + n).min(right + 1).max(old);
                let next = self.compute_trimmed_len(limit.min(cols));
                self.trimmed_len_cache = Self::encode_trimmed_len(next);
            }
        }
    }

    /// Fills `range` with `fill` (must have `fill.extra_id == 0`), cleaning wide overlaps and
    /// freeing overwritten `CellExtra`s while maintaining `trimmed_len_cache`.
    pub(crate) fn fill_range(
        &mut self,
        range: Range<usize>,
        fill: Cell,
        extras: &mut CellExtraTable,
    ) {
        debug_assert_eq!(fill.extra_id, 0);
        let cols = self.cells.len();
        let start = range.start.min(cols);
        let end = range.end.min(cols);
        if start >= end {
            return;
        }

        let old_cache = self.trimmed_len_cache;
        let was_end_wide_lead = end < cols && self.cells[end - 1].is_wide_lead();
        let overlap_bg = fill.bg;

        if start > 0 && self.cells[start].is_wide_spacer() {
            self.clear_wide_overlap_at(start, overlap_bg, extras);
        }
        if end < cols && self.cells[end - 1].is_wide_lead() {
            self.clear_wide_overlap_at(end - 1, overlap_bg, extras);
        }

        if self.may_have_extras {
            for cell in &mut self.cells[start..end] {
                if cell.extra_id != 0 {
                    extras.free(cell.extra_id);
                }
            }
        }

        self.cells[start..end].fill(fill);
        if end == cols && !fill.is_wrap_spacer() {
            self.soft_wrapped = false;
        }

        if old_cache != UNKNOWN_TRIMMED_LEN {
            let old = (old_cache as usize).min(cols);
            if !fill.is_empty() {
                let next = if was_end_wide_lead && end + 1 == old && overlap_bg.is_default() {
                    end
                } else {
                    old.max(end)
                };
                self.trimmed_len_cache = Self::encode_trimmed_len(next);
            } else if end < old && !(was_end_wide_lead && end + 1 == old) {
                self.trimmed_len_cache = Self::encode_trimmed_len(old);
            } else {
                let next = self.compute_trimmed_len(start.min(old));
                self.trimmed_len_cache = Self::encode_trimmed_len(next);
            }
        }
    }

    /// Resizes a non-wrapped row whose `trimmed_len <= new_cols` in place without reallocation.
    pub(crate) fn resize_cols_in_place_fast(
        &mut self,
        new_cols: usize,
        generation: u64,
        trimmed_len: usize,
    ) {
        let old_cols = self.cells.len();
        debug_assert!(!self.soft_wrapped);
        debug_assert!(trimmed_len <= new_cols);
        debug_assert!(trimmed_len <= old_cols);
        if new_cols < old_cols {
            self.cells.truncate(new_cols);
        } else if new_cols > old_cols {
            self.cells.resize(new_cols, Cell::BLANK);
        }
        self.generation = generation;
        self.trimmed_len_cache = Self::encode_trimmed_len(trimmed_len);
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
        let old_cache = self.trimmed_len_cache;
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
            if old_cache != UNKNOWN_TRIMMED_LEN {
                let old = old_cache as usize;
                if old <= new_cols {
                    if old == new_cols && new_cols > 0 && self.cells[new_cols - 1].is_empty() {
                        let next = self.compute_trimmed_len(new_cols);
                        self.trimmed_len_cache = Self::encode_trimmed_len(next);
                    }
                } else {
                    let next = self.compute_trimmed_len(new_cols);
                    self.trimmed_len_cache = Self::encode_trimmed_len(next);
                }
            }
        } else if new_cols > old_cols {
            let mut cleared_wrap_spacer = false;
            if old_cols > 0 && self.cells[old_cols - 1].is_wrap_spacer() {
                self.cells[old_cols - 1].reset(PackedColor::Default);
                cleared_wrap_spacer = true;
            }
            self.soft_wrapped = false;
            self.cells.resize(new_cols, Cell::BLANK);
            if cleared_wrap_spacer && old_cache != UNKNOWN_TRIMMED_LEN {
                let next = self.compute_trimmed_len(old_cols);
                self.trimmed_len_cache = Self::encode_trimmed_len(next);
            }
        }
        self.generation = generation;
    }

    /// Replaces internal cell vector (used by snapshot reconstruction).
    #[cfg(feature = "serde")]
    pub(crate) fn replace_cells(&mut self, cells: Vec<Cell>, soft_wrapped: bool, generation: u64) {
        self.may_have_extras = cells.iter().any(Cell::has_extra);
        self.cells = cells;
        self.soft_wrapped = soft_wrapped;
        self.generation = generation;
        self.refresh_trimmed_len();
    }

    /// Takes ownership of the underlying `Vec<Cell>` buffer, resetting the row to empty.
    pub(crate) fn take_cells(&mut self) -> Vec<Cell> {
        self.may_have_extras = false;
        self.soft_wrapped = false;
        self.trimmed_len_cache = 0;
        std::mem::take(&mut self.cells)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_trimmed_len_exact(row: &Row) {
        let expected = row
            .cells()
            .iter()
            .rposition(|c| !c.is_empty())
            .map_or(0, |idx| idx + 1);
        assert_eq!(row.trimmed_len(), expected);
        assert_eq!(row.trimmed_len_cache, Row::encode_trimmed_len(expected));
    }

    #[test]
    fn row_size_and_trimmed_len_cache_invariants() {
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<Row>(), 40);

        let mut extras = CellExtraTable::new();
        let mut row = Row::new(10, 1);
        assert_trimmed_len_exact(&row);

        row.write_ascii_run(
            0,
            b"abc   ",
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::empty(),
            &mut extras,
        );
        assert_eq!(row.trimmed_len(), 3);
        assert_trimmed_len_exact(&row);

        row.write_wide_pair(
            4,
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
            PackedColor::Default,
            &mut extras,
        );
        assert_eq!(row.trimmed_len(), 6);
        assert_trimmed_len_exact(&row);

        // Overwrite wide lead with single cell at tail -> trimmed_len drops from 6 to 5.
        row.write_single_cell(
            4,
            Cell::new(
                'x',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::empty(),
            ),
            PackedColor::Default,
            &mut extras,
        );
        assert_eq!(row.trimmed_len(), 5);
        assert_trimmed_len_exact(&row);

        // Overwrite with blank -> trimmed_len drops back to 3.
        row.write_single_cell(4, Cell::BLANK, PackedColor::Default, &mut extras);
        assert_eq!(row.trimmed_len(), 3);
        assert_trimmed_len_exact(&row);

        // Shift right / shift left / fill_range / clear_range_with_protection.
        row.shift_right_in_range(0, 9, 2, PackedColor::Default, &mut extras);
        assert_eq!(row.trimmed_len(), 5);
        assert_trimmed_len_exact(&row);

        row.shift_left_in_range(0, 9, 3, PackedColor::Default, &mut extras);
        assert_eq!(row.trimmed_len(), 2);
        assert_trimmed_len_exact(&row);

        row.fill_range(
            7..9,
            Cell::new(
                'E',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::PROTECTED,
            ),
            &mut extras,
        );
        assert_eq!(row.trimmed_len(), 9);
        assert_trimmed_len_exact(&row);

        // Selective erase skips protected cells at 7..9 and erases 0..2.
        row.clear_range_with_protection(
            0..10,
            PackedColor::Default,
            EraseProtection::UnprotectedOnly,
            &mut extras,
        );
        assert_eq!(row.trimmed_len(), 9);
        assert_trimmed_len_exact(&row);
        assert_eq!(row.cells()[0].codepoint, ' ');
        assert_eq!(row.cells()[7].codepoint, 'E');

        // Full erase clears protected cells too.
        row.clear_range_with_protection(
            0..10,
            PackedColor::Default,
            EraseProtection::All,
            &mut extras,
        );
        assert_eq!(row.trimmed_len(), 0);
        assert_trimmed_len_exact(&row);

        // Direct cells_mut invalidates cache and recomputes accurately.
        row.cells_mut()[3] = Cell::new(
            'z',
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::empty(),
        );
        assert_eq!(row.trimmed_len_cache, UNKNOWN_TRIMMED_LEN);
        assert_eq!(row.trimmed_len(), 4);
    }

    #[test]
    fn row_text_skips_sized_spacers_and_partial_eq_ignores_cache() {
        let mut extras = CellExtraTable::new();
        let mut a = Row::new(4, 1);
        a.write_single_cell(
            0,
            Cell::new(
                'A',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::empty(),
            ),
            PackedColor::Default,
            &mut extras,
        );
        a.write_single_cell(
            1,
            Cell::new(
                ' ',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::SIZED_SPACER,
            ),
            PackedColor::Default,
            &mut extras,
        );
        assert_eq!(a.text(&extras), "A");

        let mut b = a.clone();
        b.cells_mut(); // invalidates cache on b without mutating cells
        assert_eq!(a, b);
    }

    #[test]
    fn clear_range_with_protection_preserves_soft_wrapped_when_last_cell_protected() {
        let mut extras = CellExtraTable::new();

        // Protected normal cell in the last column keeps soft_wrapped=true on UnprotectedOnly.
        let mut row = Row::new(6, 1);
        row.write_ascii_run(
            0,
            b"abcde",
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::empty(),
            &mut extras,
        );
        row.write_single_cell(
            5,
            Cell::new(
                'P',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::PROTECTED,
            ),
            PackedColor::Default,
            &mut extras,
        );
        row.set_soft_wrapped(true);
        row.clear_range_with_protection(
            0..6,
            PackedColor::Default,
            EraseProtection::UnprotectedOnly,
            &mut extras,
        );
        assert!(row.soft_wrapped());
        assert_eq!(row.cells()[0].codepoint, ' ');
        assert_eq!(row.cells()[5].codepoint, 'P');
        assert_trimmed_len_exact(&row);

        // Protected WRAP_SPACER in the last column also preserves soft_wrapped=true.
        row.write_single_cell(
            5,
            Cell::new(
                ' ',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::PROTECTED | CellFlags::WRAP_SPACER,
            ),
            PackedColor::Default,
            &mut extras,
        );
        row.clear_range_with_protection(
            0..6,
            PackedColor::Default,
            EraseProtection::UnprotectedOnly,
            &mut extras,
        );
        assert!(row.soft_wrapped());
        assert!(row.cells()[5].is_wrap_spacer());

        // EraseProtection::All erases protected last cell and clears soft_wrapped.
        row.clear_range_with_protection(
            0..6,
            PackedColor::Default,
            EraseProtection::All,
            &mut extras,
        );
        assert!(!row.soft_wrapped());
        assert_trimmed_len_exact(&row);

        // Unprotected last cell erased by UnprotectedOnly clears soft_wrapped.
        row.write_single_cell(
            5,
            Cell::new(
                'z',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::empty(),
            ),
            PackedColor::Default,
            &mut extras,
        );
        row.set_soft_wrapped(true);
        row.clear_range_with_protection(
            0..6,
            PackedColor::Default,
            EraseProtection::UnprotectedOnly,
            &mut extras,
        );
        assert!(!row.soft_wrapped());
        assert_eq!(row.cells()[5].codepoint, ' ');
        assert_trimmed_len_exact(&row);
    }
}
