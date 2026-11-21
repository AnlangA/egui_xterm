//! Optional feature-gated terminal capabilities (`search`, `hyperlinks`, `shell-integration`,
//! `kitty-keyboard`, `kitty-graphics`, `sixel`, `metrics`).

#[cfg(feature = "hyperlinks")]
pub mod hyperlinks;
#[cfg(any(feature = "kitty-graphics", feature = "sixel"))]
pub mod inline_images;
#[cfg(feature = "kitty-keyboard")]
pub mod kitty_keyboard;
#[cfg(feature = "metrics")]
pub mod metrics;
#[cfg(feature = "search")]
pub mod search;
#[cfg(feature = "shell-integration")]
pub mod shell_integration;

#[cfg(any(feature = "search", feature = "hyperlinks"))]
use crate::grid::Grid;

/// Maps a single rendered grapheme cluster's UTF-8 byte slice inside a logical line back to its
/// physical grid buffer row and half-open column range `[start_col, end_col)`.
#[cfg(any(feature = "search", feature = "hyperlinks"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellSpan {
    pub byte_start: usize,
    pub byte_end: usize,
    pub buf_row: usize,
    pub start_col: u16,
    pub end_col: u16,
}

/// Extracts the logical line spanning physical buffer rows `phys_start..=phys_end` into
/// `text_out` alongside per-cell [`CellSpan`] byte-to-coordinate mappings.
#[cfg(any(feature = "search", feature = "hyperlinks"))]
pub(crate) fn extract_logical_line_with_spans(
    grid: &Grid,
    phys_start: usize,
    phys_end: usize,
    text_out: &mut String,
    spans_out: &mut Vec<CellSpan>,
) {
    text_out.clear();
    spans_out.clear();

    for r in phys_start..=phys_end {
        let Some(row) = grid.row_by_buf(r) else {
            continue;
        };
        let cells = row.cells();
        let cols = cells.len();
        let take_end = if r < phys_end {
            if cols > 0 && cells[cols - 1].is_wrap_spacer() {
                cols - 1
            } else {
                cols
            }
        } else {
            row.trimmed_len()
        };

        let mut c = 0usize;
        while c < take_end {
            let cell = cells[c];
            if cell.is_wide_spacer() || cell.is_wrap_spacer() {
                c += 1;
                continue;
            }
            let col_end =
                if cell.is_wide_lead() && c + 1 < take_end && cells[c + 1].is_wide_spacer() {
                    c + 2
                } else {
                    c + 1
                };

            let byte_start = text_out.len();
            if cell.extra_id != 0
                && let Some(ext) = grid.cell_extra(cell.extra_id)
            {
                ext.write_grapheme(cell.codepoint, text_out);
            } else {
                text_out.push(cell.codepoint);
            }
            let byte_end = text_out.len();

            spans_out.push(CellSpan {
                byte_start,
                byte_end,
                buf_row: r,
                start_col: u16::try_from(c).unwrap_or(u16::MAX),
                end_col: u16::try_from(col_end).unwrap_or(u16::MAX),
            });
            c = col_end;
        }
    }
}
