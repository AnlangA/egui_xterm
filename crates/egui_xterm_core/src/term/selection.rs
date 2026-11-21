//! Interactive text selection engine (`Stream`, `Word`, `Line`, `Block`) with wide-char
//! boundary normalization, trailing whitespace trimming, and soft-wrap logical line joining.

use crate::grid::{AbsLine, Grid, GridPoint, Row};

/// Selection geometry mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SelectionMode {
    /// Linear character stream from anchor to head.
    #[default]
    Stream,
    /// Word-delimited selection expanded around the clicked token.
    Word,
    /// Full logical line selection (following `soft_wrapped` physical lines).
    Line,
    /// Rectangular column-by-row block selection.
    Block,
}

/// Type alias matching `SelectionMode` for ergonomics.
pub type SelectionKind = SelectionMode;

/// Active or finished terminal text selection anchored in monotonic [`AbsLine`] coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Selection {
    /// Selection geometry mode.
    pub mode: SelectionMode,
    /// Anchor coordinate where the selection began.
    pub anchor: GridPoint,
    /// Moving head coordinate.
    pub head: GridPoint,
    /// Whether the user has completed dragging (`selection_finish`).
    pub finished: bool,
    /// Initial start bound captured at `selection_start` for `Word`/`Line` backwards drag.
    pub(crate) origin_start: GridPoint,
    /// Initial end bound captured at `selection_start` for `Word`/`Line` backwards drag.
    pub(crate) origin_end: GridPoint,
    /// Whether the original `Word`/`Line` origin token was completely evicted off scrollback.
    #[cfg_attr(feature = "serde", serde(default))]
    pub(crate) origin_evicted: bool,
    /// Whether the original `Word`/`Line` origin token on a trailing blank row was cropped off the bottom.
    #[cfg_attr(feature = "serde", serde(default))]
    pub(crate) origin_cropped_bottom: bool,
    /// Whether `anchor` represents a virtual past-EOL boundary (cropped off the bottom).
    #[cfg_attr(feature = "serde", serde(default))]
    pub(crate) anchor_past_eol: bool,
    /// Whether `head` represents a virtual past-EOL boundary (cropped off the bottom).
    #[cfg_attr(feature = "serde", serde(default))]
    pub(crate) head_past_eol: bool,
    /// Whether `origin_start` represents a virtual past-EOL boundary (cropped off the bottom).
    #[cfg_attr(feature = "serde", serde(default))]
    pub(crate) origin_start_past_eol: bool,
    /// Whether `origin_end` represents a virtual past-EOL boundary (cropped off the bottom).
    #[cfg_attr(feature = "serde", serde(default))]
    pub(crate) origin_end_past_eol: bool,
}

impl Selection {
    /// Creates a new active selection at `anchor`.
    #[must_use]
    pub const fn new(mode: SelectionMode, anchor: GridPoint, head: GridPoint) -> Self {
        let anchor_le_head =
            anchor.line.0 < head.line.0 || (anchor.line.0 == head.line.0 && anchor.col <= head.col);
        let (origin_start, origin_end) = if anchor_le_head {
            (anchor, head)
        } else {
            (head, anchor)
        };
        Self {
            mode,
            anchor,
            head,
            finished: false,
            origin_start,
            origin_end,
            origin_evicted: false,
            origin_cropped_bottom: false,
            anchor_past_eol: false,
            head_past_eol: false,
            origin_start_past_eol: false,
            origin_end_past_eol: false,
        }
    }

    /// Returns ordered `(start, start_past_eol, end, end_past_eol)` in grid order.
    #[must_use]
    pub(crate) fn ordered_endpoints_with_eol(&self) -> (GridPoint, bool, GridPoint, bool) {
        let anchor_le_head = if self.anchor.line != self.head.line {
            self.anchor.line < self.head.line
        } else if self.anchor_past_eol != self.head_past_eol {
            !self.anchor_past_eol
        } else {
            self.anchor.col <= self.head.col
        };
        if anchor_le_head {
            (
                self.anchor,
                self.anchor_past_eol,
                self.head,
                self.head_past_eol,
            )
        } else {
            (
                self.head,
                self.head_past_eol,
                self.anchor,
                self.anchor_past_eol,
            )
        }
    }

    /// Returns ordered `(start, end)` endpoints in grid order.
    #[must_use]
    pub fn ordered_endpoints(&self) -> (GridPoint, GridPoint) {
        let (start, _, end, _) = self.ordered_endpoints_with_eol();
        (start, end)
    }

    /// Computes the inclusive selected column range `start_col..=end_col` on `line` for `row`,
    /// normalizing wide-character boundaries so wide characters are never split.
    #[must_use]
    pub fn row_col_range(&self, line: AbsLine, row: &Row) -> Option<(usize, usize)> {
        let cols = row.len();
        if cols == 0 {
            return None;
        }
        let cells = row.cells();
        let (start, start_past_eol, end, end_past_eol) = self.ordered_endpoints_with_eol();
        if line < start.line || line > end.line {
            return None;
        }

        let (raw_start, raw_end) = match self.mode {
            SelectionMode::Block => {
                let c0 = self.anchor.col.min(self.head.col);
                if c0 >= cols {
                    return None;
                }
                let c1 = self.anchor.col.max(self.head.col).min(cols - 1);
                (c0, c1)
            }
            SelectionMode::Line => {
                if line == start.line && (start_past_eol || start.col >= cols) {
                    return None;
                }
                (0, cols - 1)
            }
            SelectionMode::Stream | SelectionMode::Word => {
                let start_is_past_eol = start_past_eol || start.col >= cols;
                let end_col = if end_past_eol {
                    cols - 1
                } else {
                    end.col.min(cols - 1)
                };
                if start.line == end.line {
                    if start_is_past_eol {
                        return None;
                    }
                    (start.col, end_col)
                } else if line == start.line {
                    if start_is_past_eol {
                        return None;
                    }
                    (start.col, cols - 1)
                } else if line == end.line {
                    (0, end_col)
                } else {
                    (0, cols - 1)
                }
            }
        };

        let mut c0 = raw_start;
        if c0 > 0 && cells[c0].is_wide_spacer() {
            c0 -= 1;
        }
        let mut c1 = raw_end;
        if c1 + 1 < cols && cells[c1].is_wide_lead() && cells[c1 + 1].is_wide_spacer() {
            c1 += 1;
        }

        if c0 <= c1 { Some((c0, c1)) } else { None }
    }
}

fn is_cell_separator(grid: &Grid, row: &Row, col: usize, separators: &str) -> bool {
    let raw_cell = row.cells()[col];
    if raw_cell.is_wrap_spacer() {
        return true;
    }
    let cell = if raw_cell.is_wide_spacer() && col > 0 {
        row.cells()[col - 1]
    } else {
        raw_cell
    };
    if cell.has_extra()
        && let Some(ext) = grid.cell_extra(cell.extra_id)
        && !ext.combining.is_empty()
    {
        return false;
    }
    separators.contains(cell.codepoint)
}

/// Computes `(start, end)` inclusive endpoints for word selection at `point`.
#[must_use]
pub fn word_bounds_at(
    grid: &Grid,
    mut point: GridPoint,
    separators: &str,
) -> (GridPoint, GridPoint) {
    let cols = grid.cols();
    if cols == 0 {
        return (point, point);
    }
    point.col = point.col.min(cols - 1);

    let Some(mut cur_idx) = grid.buf_idx_of_abs(point.line) else {
        return (point, point);
    };
    let Some(row) = grid.row_by_buf(cur_idx) else {
        return (point, point);
    };
    if row.cells()[point.col].is_wide_spacer() && point.col > 0 {
        point.col -= 1;
    }

    if is_cell_separator(grid, row, point.col, separators) {
        let mut end = point;
        if row.cells()[point.col].is_wide_lead() && point.col + 1 < cols {
            end.col = point.col + 1;
        }
        return (point, end);
    }

    // Expand backward across contiguous non-separators (following soft-wraps).
    let mut start_idx = cur_idx;
    let mut start_col = point.col;
    loop {
        if start_col > 0 {
            let prev_col = if grid
                .row_by_buf(start_idx)
                .is_some_and(|r| r.cells()[start_col - 1].is_wide_spacer())
                && start_col >= 2
            {
                start_col - 2
            } else {
                start_col - 1
            };
            if grid
                .row_by_buf(start_idx)
                .is_some_and(|r| !is_cell_separator(grid, r, prev_col, separators))
            {
                start_col = prev_col;
                continue;
            }
            break;
        }
        if start_idx > 0
            && grid
                .row_by_buf(start_idx - 1)
                .is_some_and(Row::soft_wrapped)
        {
            start_idx -= 1;
            let Some(prev_row) = grid.row_by_buf(start_idx) else {
                break;
            };
            let mut candidate = cols - 1;
            if prev_row.cells()[candidate].is_wrap_spacer() && candidate > 0 {
                candidate -= 1;
            }
            if prev_row.cells()[candidate].is_wide_spacer() && candidate > 0 {
                candidate -= 1;
            }
            if !is_cell_separator(grid, prev_row, candidate, separators) {
                start_col = candidate;
                continue;
            }
            start_idx += 1;
        }
        break;
    }

    // Expand forward across contiguous non-separators (following soft-wraps).
    let mut end_col = point.col;
    if row.cells()[end_col].is_wide_lead() && end_col + 1 < cols {
        end_col += 1;
    }
    while let Some(cur_row) = grid.row_by_buf(cur_idx) {
        let row_limit = if cur_row.soft_wrapped() && cur_row.cells()[cols - 1].is_wrap_spacer() {
            cols - 1
        } else {
            cols
        };

        if end_col + 1 < row_limit {
            let next_col = end_col + 1;
            if !is_cell_separator(grid, cur_row, next_col, separators) {
                end_col = if cur_row.cells()[next_col].is_wide_lead() && next_col + 1 < row_limit {
                    next_col + 1
                } else {
                    next_col
                };
                continue;
            }
            break;
        }
        if cur_row.soft_wrapped()
            && let Some(next_row) = grid.row_by_buf(cur_idx + 1)
            && !is_cell_separator(grid, next_row, 0, separators)
        {
            cur_idx += 1;
            end_col = usize::from(next_row.cells()[0].is_wide_lead() && cols >= 2);
            continue;
        }
        break;
    }

    (
        GridPoint::new(grid.abs_line_of_buf(start_idx), start_col),
        GridPoint::new(grid.abs_line_of_buf(cur_idx), end_col),
    )
}

/// Computes `(start, end)` inclusive endpoints for logical-line selection at `line`.
#[must_use]
pub fn line_bounds_at(grid: &Grid, line: AbsLine) -> (GridPoint, GridPoint) {
    let cols = grid.cols();
    let Some(idx) = grid.buf_idx_of_abs(line) else {
        return (
            GridPoint::new(line, 0),
            GridPoint::new(line, cols.saturating_sub(1)),
        );
    };
    let mut start_idx = idx;
    while start_idx > 0
        && grid
            .row_by_buf(start_idx - 1)
            .is_some_and(Row::soft_wrapped)
    {
        start_idx -= 1;
    }
    let mut end_idx = idx;
    while end_idx + 1 < grid.total_len() && grid.row_by_buf(end_idx).is_some_and(Row::soft_wrapped)
    {
        end_idx += 1;
    }
    (
        GridPoint::new(grid.abs_line_of_buf(start_idx), 0),
        GridPoint::new(grid.abs_line_of_buf(end_idx), cols.saturating_sub(1)),
    )
}

/// Extracts formatted text for `selection` from `grid`, honoring wide chars, combining marks,
/// trailing space trimming on hard-broken lines, and soft-wrap line joining.
#[must_use]
pub fn extract_text(grid: &Grid, selection: &Selection) -> Option<String> {
    let (start, start_past_eol, end, _) = selection.ordered_endpoints_with_eol();
    let lo = start.line.0.max(grid.oldest_abs_line().0);
    let hi = end.line.0.min(grid.newest_abs_line().0);
    if lo > hi {
        return None;
    }
    if lo == hi && start.line.0 == lo && start_past_eol {
        return None;
    }

    let mut out = String::new();
    let mut line_buf = String::new();

    for abs_u64 in lo..=hi {
        let abs = AbsLine(abs_u64);
        if abs == start.line && start_past_eol {
            continue;
        }
        let Some(row) = grid.row_by_abs(abs) else {
            continue;
        };
        line_buf.clear();
        if let Some((c0, c1)) = selection.row_col_range(abs, row) {
            let cells = row.cells();
            for cell in &cells[c0..=c1] {
                if cell.is_wide_spacer() || cell.is_wrap_spacer() || cell.is_sized_spacer() {
                    continue;
                }
                if cell.extra_id != 0
                    && let Some(ext) = grid.cell_extra(cell.extra_id)
                {
                    ext.write_grapheme(cell.codepoint, &mut line_buf);
                    continue;
                }
                line_buf.push(cell.codepoint);
            }
        }

        let is_last_row = abs_u64 == hi;
        let is_block = selection.mode == SelectionMode::Block;
        let hard_break = !row.soft_wrapped() || is_block;

        out.push_str(&line_buf);
        if hard_break || is_last_row {
            let trimmed_len = out.trim_end_matches(' ').len();
            out.truncate(trimmed_len);
        }
        if !is_last_row && hard_break {
            out.push('\n');
        }
    }

    Some(out)
}

/// Returns `true` if [`extract_text`] would produce a non-empty string, without allocating.
#[must_use]
pub(crate) fn has_non_empty_text(grid: &Grid, selection: &Selection) -> bool {
    let (start, start_past_eol, end, _) = selection.ordered_endpoints_with_eol();
    let lo = start.line.0.max(grid.oldest_abs_line().0);
    let hi = end.line.0.min(grid.newest_abs_line().0);
    if lo > hi {
        return false;
    }
    if lo == hi && start.line.0 == lo && start_past_eol {
        return false;
    }

    let is_block = selection.mode == SelectionMode::Block;
    for abs_u64 in lo..=hi {
        let abs = AbsLine(abs_u64);
        if abs == start.line && start_past_eol {
            continue;
        }
        let Some(row) = grid.row_by_abs(abs) else {
            continue;
        };
        if abs_u64 < hi && (!row.soft_wrapped() || is_block) {
            return true;
        }
        if let Some((c0, c1)) = selection.row_col_range(abs, row) {
            let cells = row.cells();
            for cell in &cells[c0..=c1] {
                if cell.is_wide_spacer() || cell.is_wrap_spacer() || cell.is_sized_spacer() {
                    continue;
                }
                if cell.codepoint != ' ' {
                    return true;
                }
                if cell.extra_id != 0
                    && let Some(ext) = grid.cell_extra(cell.extra_id)
                    && !ext.combining.is_empty()
                {
                    return true;
                }
            }
        }
    }

    false
}
