//! Lossless primary-screen resize reflow and non-reflowing alternate-screen resize.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::cell::{Cell, CellFlags};
use crate::color::PackedColor;
use crate::grid::extras::CellExtraTable;
use crate::grid::row::Row;
use crate::grid::{AbsLine, Grid, GridPoint, ScreenKind};

/// Anchors tracked across a [`Grid::resize`] operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReflowAnchors {
    /// Live viewport cursor row (`0..old_rows`).
    pub cursor_row: usize,
    /// Cursor column (`0..old_cols`).
    pub cursor_col: usize,
    /// Whether the cursor is in `pending_wrap` state at the right margin.
    pub pending_wrap: bool,
    /// Optional active selection endpoints `(anchor, head)`.
    pub selection: Option<(GridPoint, GridPoint)>,
    /// Additional grid points (e.g., prompt markers) to remap across reflow.
    pub extra_points: Vec<GridPoint>,
}

/// Remapped coordinates produced by [`Grid::resize`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReflowResult {
    /// New live viewport cursor row (`0..new_rows`).
    pub cursor_row: usize,
    /// New cursor column (`0..new_cols`).
    pub cursor_col: usize,
    /// Updated `pending_wrap` state.
    pub pending_wrap: bool,
    /// Remapped selection endpoints, or `None` if evicted.
    pub selection: Option<(GridPoint, GridPoint)>,
    /// Remapped auxiliary points (`None` if that line was evicted).
    pub extra_points: Vec<Option<GridPoint>>,
}

#[derive(Debug, Clone, Copy)]
struct LogicalGlyph {
    cell: Cell,
    width: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnchorKind {
    Cursor,
    ViewportTop,
    SelAnchor,
    SelHead,
    OriginStart,
    OriginEnd,
    Extra(usize),
}

impl AnchorKind {
    const fn is_selection_point(self) -> bool {
        matches!(
            self,
            Self::SelAnchor | Self::SelHead | Self::OriginStart | Self::OriginEnd
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvictionColPolicy {
    ResetToZero,
    PreserveColumn,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SelectionReflowInput {
    pub anchor: GridPoint,
    pub anchor_past_eol: bool,
    pub head: GridPoint,
    pub head_past_eol: bool,
    pub origin_start: GridPoint,
    pub origin_start_past_eol: bool,
    pub origin_end: GridPoint,
    pub origin_end_past_eol: bool,
    pub origin_evicted: bool,
    pub origin_cropped_bottom: bool,
    pub col_policy: EvictionColPolicy,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SelectionReflowResult {
    pub anchor: GridPoint,
    pub anchor_past_eol: bool,
    pub head: GridPoint,
    pub head_past_eol: bool,
    pub origin_start: GridPoint,
    pub origin_start_past_eol: bool,
    pub origin_end: GridPoint,
    pub origin_end_past_eol: bool,
    pub origin_evicted: bool,
    pub origin_cropped_bottom: bool,
}

#[derive(Debug, Clone, Copy)]
enum RemappedPoint {
    Kept { pt: GridPoint, past_eol: bool },
    TopEvicted { orig_col: usize },
    BottomCropped { row: AbsLine, orig_col: usize },
}

#[derive(Debug, Clone, Copy)]
struct ResolvedPoint {
    pt: GridPoint,
    past_eol: bool,
}

const fn evict_point(
    oldest_abs: AbsLine,
    orig_col: usize,
    col_policy: EvictionColPolicy,
) -> ResolvedPoint {
    let col = match col_policy {
        EvictionColPolicy::PreserveColumn => orig_col,
        EvictionColPolicy::ResetToZero => 0,
    };
    ResolvedPoint {
        pt: GridPoint::new(oldest_abs, col),
        past_eol: false,
    }
}

const fn tail_point(
    row: AbsLine,
    orig_col: usize,
    new_cols: usize,
    col_policy: EvictionColPolicy,
) -> ResolvedPoint {
    match col_policy {
        EvictionColPolicy::PreserveColumn => ResolvedPoint {
            pt: GridPoint::new(row, orig_col),
            past_eol: false,
        },
        EvictionColPolicy::ResetToZero => ResolvedPoint {
            pt: GridPoint::new(row, new_cols),
            past_eol: true,
        },
    }
}

fn resolve_selection_pair(
    a: RemappedPoint,
    h: RemappedPoint,
    oldest_abs: AbsLine,
    new_cols: usize,
    col_policy: EvictionColPolicy,
) -> Option<(ResolvedPoint, ResolvedPoint)> {
    match (a, h) {
        (RemappedPoint::TopEvicted { .. }, RemappedPoint::TopEvicted { .. })
        | (RemappedPoint::BottomCropped { .. }, RemappedPoint::BottomCropped { .. }) => None,
        (
            RemappedPoint::Kept {
                pt: pa,
                past_eol: pa_eol,
            },
            RemappedPoint::Kept {
                pt: ph,
                past_eol: ph_eol,
            },
        ) => Some((
            ResolvedPoint {
                pt: pa,
                past_eol: pa_eol,
            },
            ResolvedPoint {
                pt: ph,
                past_eol: ph_eol,
            },
        )),
        (
            RemappedPoint::Kept {
                pt: pa,
                past_eol: pa_eol,
            },
            RemappedPoint::TopEvicted { orig_col },
        ) => Some((
            ResolvedPoint {
                pt: pa,
                past_eol: pa_eol,
            },
            evict_point(oldest_abs, orig_col, col_policy),
        )),
        (
            RemappedPoint::TopEvicted { orig_col },
            RemappedPoint::Kept {
                pt: ph,
                past_eol: ph_eol,
            },
        ) => Some((
            evict_point(oldest_abs, orig_col, col_policy),
            ResolvedPoint {
                pt: ph,
                past_eol: ph_eol,
            },
        )),
        (
            RemappedPoint::Kept {
                pt: pa,
                past_eol: pa_eol,
            },
            RemappedPoint::BottomCropped { row, orig_col },
        ) => {
            if col_policy == EvictionColPolicy::ResetToZero
                && (pa_eol || pa.col >= new_cols)
                && pa.line == row
            {
                return None;
            }
            Some((
                ResolvedPoint {
                    pt: pa,
                    past_eol: pa_eol,
                },
                tail_point(row, orig_col, new_cols, col_policy),
            ))
        }
        (
            RemappedPoint::BottomCropped { row, orig_col },
            RemappedPoint::Kept {
                pt: ph,
                past_eol: ph_eol,
            },
        ) => {
            if col_policy == EvictionColPolicy::ResetToZero
                && (ph_eol || ph.col >= new_cols)
                && ph.line == row
            {
                return None;
            }
            Some((
                tail_point(row, orig_col, new_cols, col_policy),
                ResolvedPoint {
                    pt: ph,
                    past_eol: ph_eol,
                },
            ))
        }
        (
            RemappedPoint::TopEvicted { orig_col: a_col },
            RemappedPoint::BottomCropped {
                row,
                orig_col: h_col,
            },
        ) => Some((
            evict_point(oldest_abs, a_col, col_policy),
            tail_point(row, h_col, new_cols, col_policy),
        )),
        (
            RemappedPoint::BottomCropped {
                row,
                orig_col: a_col,
            },
            RemappedPoint::TopEvicted { orig_col: h_col },
        ) => Some((
            tail_point(row, a_col, new_cols, col_policy),
            evict_point(oldest_abs, h_col, col_policy),
        )),
    }
}

fn resolve_selection_result(
    anchor_pt: RemappedPoint,
    head_pt: RemappedPoint,
    origin_start_pt: RemappedPoint,
    origin_end_pt: RemappedPoint,
    input: SelectionReflowInput,
    oldest_abs: AbsLine,
    new_cols: usize,
) -> Option<SelectionReflowResult> {
    let (anchor, head) =
        resolve_selection_pair(anchor_pt, head_pt, oldest_abs, new_cols, input.col_policy)?;

    let oe_evicted = matches!(origin_end_pt, RemappedPoint::TopEvicted { .. });
    let origin_evicted = input.origin_evicted || oe_evicted;

    let os_cropped_bottom = matches!(origin_start_pt, RemappedPoint::BottomCropped { .. });
    let origin_cropped_bottom =
        !origin_evicted && (input.origin_cropped_bottom || os_cropped_bottom);

    let map_origin_pt = |pt: RemappedPoint| match pt {
        RemappedPoint::Kept { pt, past_eol } => ResolvedPoint { pt, past_eol },
        RemappedPoint::TopEvicted { orig_col } => {
            evict_point(oldest_abs, orig_col, input.col_policy)
        }
        RemappedPoint::BottomCropped { row, orig_col } => {
            tail_point(row, orig_col, new_cols, input.col_policy)
        }
    };

    let mut origin_start = map_origin_pt(origin_start_pt);
    let origin_end = map_origin_pt(origin_end_pt);
    if oe_evicted || os_cropped_bottom {
        origin_start = origin_end;
    }

    Some(SelectionReflowResult {
        anchor: anchor.pt,
        anchor_past_eol: anchor.past_eol,
        head: head.pt,
        head_past_eol: head.past_eol,
        origin_start: origin_start.pt,
        origin_start_past_eol: origin_start.past_eol,
        origin_end: origin_end.pt,
        origin_end_past_eol: origin_end.past_eol,
        origin_evicted,
        origin_cropped_bottom,
    })
}

#[derive(Debug, Clone, Copy)]
struct TrackedAnchor {
    kind: AnchorKind,
    buf_row: usize,
    col: usize,
    is_pending_wrap: bool,
    past_eol: bool,
    already_evicted: bool,
    blank_offset: usize,
    mapped_row: usize,
    mapped_col: usize,
    mapped_pending_wrap: bool,
    mapped_past_eol: bool,
    is_trailing_wide: bool,
    target_glyph: usize,
    trailing_delta: usize,
}

impl TrackedAnchor {
    fn new(
        kind: AnchorKind,
        buf_row: usize,
        col: usize,
        is_pending_wrap: bool,
        past_eol: bool,
    ) -> Self {
        Self {
            kind,
            buf_row,
            col,
            is_pending_wrap,
            past_eol,
            already_evicted: false,
            blank_offset: 0,
            mapped_row: 0,
            mapped_col: 0,
            mapped_pending_wrap: false,
            mapped_past_eol: false,
            is_trailing_wide: false,
            target_glyph: 0,
            trailing_delta: 0,
        }
    }

    fn evicted(kind: AnchorKind, col: usize) -> Self {
        Self {
            kind,
            buf_row: 0,
            col,
            is_pending_wrap: false,
            past_eol: false,
            already_evicted: true,
            blank_offset: 0,
            mapped_row: 0,
            mapped_col: col,
            mapped_pending_wrap: false,
            mapped_past_eol: false,
            is_trailing_wide: false,
            target_glyph: 0,
            trailing_delta: 0,
        }
    }
}

fn glyph_width_when_widening(cell: &Cell, extras: &CellExtraTable, scratch: &mut String) -> u8 {
    if cell.has_extra()
        && let Some(ext) = extras.get(cell.extra_id)
    {
        scratch.clear();
        ext.write_grapheme(cell.codepoint, scratch);
        let w = UnicodeWidthStr::width(scratch.as_str());
        return if w >= 2 { 2 } else { 1 };
    }
    let w = UnicodeWidthChar::width(cell.codepoint).unwrap_or(1);
    if w >= 2 { 2 } else { 1 }
}

impl Grid {
    /// Resizes the grid to `(new_cols, new_rows)`, performing lossless soft-wrap reflow on
    /// [`ScreenKind::Primary`] and non-reflowing crop/pad on [`ScreenKind::Alternate`].
    pub fn resize(
        &mut self,
        new_cols: usize,
        new_rows: usize,
        anchors: ReflowAnchors,
    ) -> ReflowResult {
        let (res, _, _) = self.resize_with_selection(new_cols, new_rows, anchors, None, None);
        res
    }

    pub(crate) fn resize_with_selection(
        &mut self,
        new_cols: usize,
        new_rows: usize,
        anchors: ReflowAnchors,
        sel_input: Option<SelectionReflowInput>,
        extra_pending_wrap: Option<usize>,
    ) -> (ReflowResult, Option<SelectionReflowResult>, bool) {
        let new_cols = new_cols.max(1);
        let new_rows = new_rows.max(1);

        if new_cols == self.cols && new_rows == self.viewport_rows {
            let sel_res = sel_input.map(|si| SelectionReflowResult {
                anchor: si.anchor,
                anchor_past_eol: si.anchor_past_eol,
                head: si.head,
                head_past_eol: si.head_past_eol,
                origin_start: si.origin_start,
                origin_start_past_eol: si.origin_start_past_eol,
                origin_end: si.origin_end,
                origin_end_past_eol: si.origin_end_past_eol,
                origin_evicted: si.origin_evicted,
                origin_cropped_bottom: si.origin_cropped_bottom,
            });
            let extra_pw_out = extra_pending_wrap.is_some_and(|idx| {
                anchors
                    .extra_points
                    .get(idx)
                    .is_some_and(|pt| pt.col.min(new_cols - 1) == new_cols - 1)
            });
            return (
                ReflowResult {
                    cursor_row: anchors.cursor_row.min(new_rows - 1),
                    cursor_col: anchors.cursor_col.min(new_cols - 1),
                    pending_wrap: anchors.pending_wrap && anchors.cursor_col >= new_cols - 1,
                    selection: anchors.selection,
                    extra_points: anchors.extra_points.into_iter().map(Some).collect(),
                },
                sel_res,
                extra_pw_out,
            );
        }

        match self.kind {
            ScreenKind::Alternate => self.resize_no_reflow(new_cols, new_rows, anchors, sel_input),
            ScreenKind::Primary => {
                self.resize_with_reflow(new_cols, new_rows, anchors, sel_input, extra_pending_wrap)
            }
        }
    }

    fn resize_no_reflow(
        &mut self,
        new_cols: usize,
        new_rows: usize,
        anchors: ReflowAnchors,
        sel_input: Option<SelectionReflowInput>,
    ) -> (ReflowResult, Option<SelectionReflowResult>, bool) {
        let old_rows = self.viewport_rows;
        let mut cursor_row = anchors.cursor_row.min(old_rows.saturating_sub(1));
        let mut removed_top = 0usize;

        if new_rows < old_rows {
            while self.rows.len() > new_rows
                && self.rows.len() > cursor_row + 1
                && self.rows.back().is_some_and(Row::is_completely_empty)
            {
                if let Some(row) = self.rows.pop_back() {
                    self.recycle_row(row);
                }
            }
            while self.rows.len() > new_rows {
                if let Some(row) = self.rows.pop_front() {
                    self.recycle_row(row);
                    removed_top += 1;
                }
            }
            cursor_row = cursor_row.saturating_sub(removed_top).min(new_rows - 1);
        } else if new_rows > old_rows {
            for _ in old_rows..new_rows {
                self.damage_epoch += 1;
                let epoch = self.damage_epoch;
                self.rows.push_back(Row::new(self.cols, epoch));
            }
        }

        for row in &mut self.rows {
            self.damage_epoch += 1;
            let epoch = self.damage_epoch;
            row.resize_cols(new_cols, epoch, &mut self.extras);
        }

        self.cols = new_cols;
        self.viewport_rows = new_rows;
        self.display_offset = 0;
        self.dirty.resize(new_rows);
        self.damage_epoch += 1;
        self.content_epoch += 1;
        self.mark_dirty_abs(self.oldest_abs_line());

        let oldest_abs = self.oldest_abs_line();
        let bottom_row_abs = AbsLine(self.total_evicted + (new_rows - 1) as u64);

        let remap_point = |pt: GridPoint, past_eol: bool, clamp_col: bool| -> RemappedPoint {
            let Some(rel) = pt.line.0.checked_sub(self.total_evicted) else {
                return RemappedPoint::TopEvicted { orig_col: pt.col };
            };
            let Ok(idx) = usize::try_from(rel) else {
                return RemappedPoint::BottomCropped {
                    row: bottom_row_abs,
                    orig_col: pt.col,
                };
            };
            if idx < removed_top {
                return RemappedPoint::TopEvicted { orig_col: pt.col };
            }
            let shifted = idx - removed_top;
            if shifted >= new_rows {
                return RemappedPoint::BottomCropped {
                    row: bottom_row_abs,
                    orig_col: pt.col,
                };
            }
            let col = if past_eol {
                new_cols
            } else if clamp_col {
                pt.col.min(new_cols - 1)
            } else {
                pt.col
            };
            RemappedPoint::Kept {
                pt: GridPoint::new(AbsLine(self.total_evicted + shifted as u64), col),
                past_eol,
            }
        };

        let sel_res = sel_input.and_then(|si| {
            resolve_selection_result(
                remap_point(si.anchor, si.anchor_past_eol, false),
                remap_point(si.head, si.head_past_eol, false),
                remap_point(si.origin_start, si.origin_start_past_eol, false),
                remap_point(si.origin_end, si.origin_end_past_eol, false),
                si,
                oldest_abs,
                new_cols,
            )
        });
        let selection = anchors.selection.and_then(|(a, h)| {
            resolve_selection_pair(
                remap_point(a, false, false),
                remap_point(h, false, false),
                oldest_abs,
                new_cols,
                EvictionColPolicy::ResetToZero,
            )
            .map(|(ra, rh)| (ra.pt, rh.pt))
        });
        let extra_points = anchors
            .extra_points
            .into_iter()
            .map(|pt| match remap_point(pt, false, true) {
                RemappedPoint::Kept { pt: p, .. } => Some(p),
                RemappedPoint::TopEvicted { .. } | RemappedPoint::BottomCropped { .. } => None,
            })
            .collect();

        (
            ReflowResult {
                cursor_row,
                cursor_col: anchors.cursor_col.min(new_cols - 1),
                pending_wrap: false,
                selection,
                extra_points,
            },
            sel_res,
            false,
        )
    }

    fn resize_with_reflow(
        &mut self,
        new_cols: usize,
        new_rows: usize,
        anchors: ReflowAnchors,
        sel_input: Option<SelectionReflowInput>,
        extra_pending_wrap: Option<usize>,
    ) -> (ReflowResult, Option<SelectionReflowResult>, bool) {
        let old_cols = self.cols;
        let old_rows = self.viewport_rows;
        let history_len = self.history_len();
        let cursor_buf_row = history_len + anchors.cursor_row.min(old_rows - 1);
        let last_non_empty = self
            .rows
            .iter()
            .rposition(|r| !r.is_completely_empty())
            .unwrap_or(0);
        let keep_until_row = last_non_empty.max(cursor_buf_row);
        let old_top_buf_row =
            (self.display_offset > 0).then(|| history_len.saturating_sub(self.display_offset));

        if new_cols == old_cols {
            while self.rows.len() > keep_until_row + 1 {
                if let Some(row) = self.rows.pop_back() {
                    self.recycle_row(row);
                }
            }
            let total_kept = self.rows.len();
            let evict_count = if total_kept < new_rows {
                for _ in total_kept..new_rows {
                    self.damage_epoch += 1;
                    let epoch = self.damage_epoch;
                    let row = self.acquire_row(Cell::BLANK, epoch);
                    self.rows.push_back(row);
                }
                0
            } else {
                let unclamped_hist = total_kept - new_rows;
                let evict = unclamped_hist.saturating_sub(self.scrollback_limit);
                for _ in 0..evict {
                    if let Some(row) = self.rows.pop_front() {
                        self.total_evicted += 1;
                        self.recycle_row(row);
                    }
                }
                evict
            };

            self.viewport_rows = new_rows;
            let new_history_len = self.history_len();
            let new_cursor_row = cursor_buf_row
                .saturating_sub(evict_count)
                .saturating_sub(new_history_len)
                .min(new_rows - 1);
            let new_cursor_col = anchors.cursor_col.min(new_cols - 1);

            self.display_offset = if let Some(top) = old_top_buf_row {
                new_history_len
                    .saturating_sub(top.saturating_sub(evict_count))
                    .min(new_history_len)
            } else {
                0
            };

            let old_evicted = self.total_evicted - evict_count as u64;
            let last_kept_buf_row = total_kept - 1 - evict_count;
            let has_blank_tail = total_kept < new_rows;
            let oldest_abs = self.oldest_abs_line();
            let last_kept_abs = AbsLine(self.total_evicted + last_kept_buf_row as u64);

            let remap_point = |pt: GridPoint, past_eol: bool, clamp_col: bool| -> RemappedPoint {
                if pt.line.0 < old_evicted {
                    return RemappedPoint::TopEvicted { orig_col: pt.col };
                }
                let raw_idx = usize::try_from(pt.line.0 - old_evicted).unwrap_or(usize::MAX);
                if raw_idx > keep_until_row {
                    let blank_offset = raw_idx - keep_until_row;
                    if has_blank_tail
                        && let Some(new_idx) = last_kept_buf_row.checked_add(blank_offset)
                        && new_idx < self.rows.len()
                    {
                        let col = if past_eol {
                            new_cols
                        } else if clamp_col {
                            pt.col.min(new_cols - 1)
                        } else {
                            pt.col
                        };
                        return RemappedPoint::Kept {
                            pt: GridPoint::new(AbsLine(self.total_evicted + new_idx as u64), col),
                            past_eol,
                        };
                    }
                    let bottom_abs = if has_blank_tail {
                        AbsLine(self.total_evicted + (self.rows.len() - 1) as u64)
                    } else {
                        last_kept_abs
                    };
                    return RemappedPoint::BottomCropped {
                        row: bottom_abs,
                        orig_col: pt.col,
                    };
                }
                if raw_idx < evict_count {
                    return RemappedPoint::TopEvicted { orig_col: pt.col };
                }
                let new_idx = raw_idx - evict_count;
                let col = if past_eol {
                    new_cols
                } else if clamp_col {
                    pt.col.min(new_cols - 1)
                } else {
                    pt.col
                };
                RemappedPoint::Kept {
                    pt: GridPoint::new(AbsLine(self.total_evicted + new_idx as u64), col),
                    past_eol,
                }
            };

            let sel_res = sel_input.and_then(|si| {
                resolve_selection_result(
                    remap_point(si.anchor, si.anchor_past_eol, false),
                    remap_point(si.head, si.head_past_eol, false),
                    remap_point(si.origin_start, si.origin_start_past_eol, false),
                    remap_point(si.origin_end, si.origin_end_past_eol, false),
                    si,
                    oldest_abs,
                    new_cols,
                )
            });
            let selection = anchors.selection.and_then(|(a, h)| {
                resolve_selection_pair(
                    remap_point(a, false, false),
                    remap_point(h, false, false),
                    oldest_abs,
                    new_cols,
                    EvictionColPolicy::ResetToZero,
                )
                .map(|(ra, rh)| (ra.pt, rh.pt))
            });
            let mut extra_pw_out = false;
            let extra_points = anchors
                .extra_points
                .into_iter()
                .enumerate()
                .map(|(i, pt)| {
                    let mapped = match remap_point(pt, false, true) {
                        RemappedPoint::Kept { pt: p, .. } => Some(p),
                        RemappedPoint::TopEvicted { .. } => None,
                        RemappedPoint::BottomCropped { row, orig_col } => {
                            Some(GridPoint::new(row, orig_col.min(new_cols - 1)))
                        }
                    };
                    if extra_pending_wrap == Some(i)
                        && let Some(p) = mapped
                    {
                        extra_pw_out = p.col == new_cols - 1;
                    }
                    mapped
                })
                .collect();

            self.dirty.resize(new_rows);
            self.damage_epoch += 1;
            self.content_epoch += 1;
            self.mark_dirty_abs(self.oldest_abs_line());
            return (
                ReflowResult {
                    cursor_row: new_cursor_row,
                    cursor_col: new_cursor_col,
                    pending_wrap: anchors.pending_wrap && new_cursor_col == new_cols - 1,
                    selection,
                    extra_points,
                },
                sel_res,
                extra_pw_out,
            );
        }

        // Full horizontal + vertical reflow (`new_cols != old_cols`).
        while self.rows.len() > keep_until_row + 1 {
            if let Some(row) = self.rows.pop_back() {
                self.recycle_row(row);
            }
        }

        let old_evicted = self.total_evicted;
        let mut tracked = Vec::with_capacity(6 + anchors.extra_points.len());
        tracked.push(TrackedAnchor::new(
            AnchorKind::Cursor,
            cursor_buf_row,
            anchors.cursor_col.min(old_cols - 1),
            anchors.pending_wrap,
            false,
        ));
        if let Some(top) = old_top_buf_row {
            tracked.push(TrackedAnchor::new(
                AnchorKind::ViewportTop,
                top,
                0,
                false,
                false,
            ));
        }
        let push_grid_point = |list: &mut Vec<TrackedAnchor>,
                               kind: AnchorKind,
                               pt: GridPoint,
                               past_eol: bool,
                               is_pending_wrap: bool| {
            if pt.line.0 < old_evicted {
                list.push(TrackedAnchor::evicted(kind, pt.col));
            } else {
                let idx = usize::try_from(pt.line.0 - old_evicted).unwrap_or(usize::MAX);
                let col = if kind.is_selection_point() {
                    pt.col
                } else {
                    pt.col.min(old_cols - 1)
                };
                if idx > keep_until_row {
                    let mut anchor =
                        TrackedAnchor::new(kind, keep_until_row, col, is_pending_wrap, past_eol);
                    anchor.blank_offset = idx - keep_until_row;
                    list.push(anchor);
                } else {
                    list.push(TrackedAnchor::new(
                        kind,
                        idx,
                        col,
                        is_pending_wrap,
                        past_eol,
                    ));
                }
            }
        };
        if let Some(si) = sel_input {
            push_grid_point(
                &mut tracked,
                AnchorKind::SelAnchor,
                si.anchor,
                si.anchor_past_eol,
                false,
            );
            push_grid_point(
                &mut tracked,
                AnchorKind::SelHead,
                si.head,
                si.head_past_eol,
                false,
            );
            push_grid_point(
                &mut tracked,
                AnchorKind::OriginStart,
                si.origin_start,
                si.origin_start_past_eol,
                false,
            );
            push_grid_point(
                &mut tracked,
                AnchorKind::OriginEnd,
                si.origin_end,
                si.origin_end_past_eol,
                false,
            );
        } else if let Some((a, h)) = anchors.selection {
            push_grid_point(&mut tracked, AnchorKind::SelAnchor, a, false, false);
            push_grid_point(&mut tracked, AnchorKind::SelHead, h, false, false);
        }
        for (i, &pt) in anchors.extra_points.iter().enumerate() {
            let is_pw = extra_pending_wrap == Some(i);
            push_grid_point(&mut tracked, AnchorKind::Extra(i), pt, false, is_pw);
        }

        let mut old_rows_vec: Vec<Row> = self.rows.drain(..).collect();
        let mut new_rows_vec: Vec<Row> = Vec::with_capacity(old_rows_vec.len());
        let mut logical: Vec<LogicalGlyph> = Vec::with_capacity(old_cols * 2);
        let mut scratch = String::new();

        // Pre-sort active anchors by (buf_row, col) so every logical line scans only its O(k) slice.
        let mut sorted_anchors: Vec<usize> = tracked
            .iter()
            .enumerate()
            .filter_map(|(idx, a)| (!a.already_evicted && a.blank_offset == 0).then_some(idx))
            .collect();
        sorted_anchors.sort_unstable_by_key(|&idx| (tracked[idx].buf_row, tracked[idx].col));
        let mut anchor_cursor = 0usize;

        // Recycle Vec<Cell> allocations across row_pool and consumed old rows.
        let pool_cap = self.row_pool_cap.max(128);
        let mut cell_vec_pool: Vec<Vec<Cell>> = Vec::with_capacity(pool_cap);
        for mut row in self.row_pool.drain(..) {
            let cells = row.take_cells();
            if cells.capacity() > 0 && cell_vec_pool.len() < pool_cap {
                cell_vec_pool.push(cells);
            }
        }

        let mut phys_start = 0usize;

        while phys_start < old_rows_vec.len() {
            let mut phys_end = phys_start;
            while phys_end + 1 < old_rows_vec.len() && old_rows_vec[phys_end].soft_wrapped() {
                phys_end += 1;
            }

            while anchor_cursor < sorted_anchors.len()
                && tracked[sorted_anchors[anchor_cursor]].buf_row < phys_start
            {
                anchor_cursor += 1;
            }
            let active_start = anchor_cursor;
            while anchor_cursor < sorted_anchors.len()
                && tracked[sorted_anchors[anchor_cursor]].buf_row <= phys_end
            {
                anchor_cursor += 1;
            }
            let active_slice = &mut sorted_anchors[active_start..anchor_cursor];

            // In-place fast path for non-wrapped single rows whose trimmed content fits `new_cols`.
            if phys_start == phys_end
                && !old_rows_vec[phys_start].soft_wrapped()
                && old_cols > 1
                && new_cols > 1
            {
                let take_end = old_rows_vec[phys_start].trimmed_len();
                let fast_ok = take_end <= new_cols
                    && active_slice.iter().all(|&idx| {
                        let a = &tracked[idx];
                        !a.is_pending_wrap && (a.past_eol || a.col < new_cols)
                    });
                if fast_ok {
                    self.damage_epoch += 1;
                    let epoch = self.damage_epoch;
                    let row_idx = new_rows_vec.len();
                    for &a_idx in active_slice.iter() {
                        let a = &mut tracked[a_idx];
                        if a.past_eol {
                            a.mapped_row = row_idx;
                            a.mapped_col = new_cols;
                            a.mapped_past_eol = true;
                            a.mapped_pending_wrap = false;
                        } else {
                            a.mapped_row = row_idx;
                            a.mapped_col = a.col;
                            a.mapped_past_eol = false;
                            a.mapped_pending_wrap = false;
                        }
                    }
                    let mut row = std::mem::replace(&mut old_rows_vec[phys_start], Row::new(0, 0));
                    row.resize_cols_in_place_fast(new_cols, epoch, take_end);
                    new_rows_vec.push(row);
                    phys_start = phys_end + 1;
                    continue;
                }
            }

            logical.clear();
            let mut col_cursor = 0usize;

            for (r, old_row) in old_rows_vec
                .iter()
                .enumerate()
                .take(phys_end + 1)
                .skip(phys_start)
            {
                while col_cursor < active_slice.len()
                    && tracked[active_slice[col_cursor]].buf_row < r
                {
                    col_cursor += 1;
                }

                let cells = old_row.cells();
                let take_end = if r < phys_end {
                    if old_cols > 0 && cells[old_cols - 1].is_wrap_spacer() {
                        old_cols - 1
                    } else {
                        old_cols
                    }
                } else if old_row.soft_wrapped()
                    && old_cols > 0
                    && cells[old_cols - 1].is_wrap_spacer()
                {
                    cells[..old_cols - 1]
                        .iter()
                        .rposition(|c| !c.is_empty())
                        .map_or(0, |idx| idx + 1)
                } else {
                    old_row.trimmed_len()
                };

                let mut c = 0usize;
                while c < take_end {
                    let cell = cells[c];
                    if cell.is_wide_spacer() {
                        c += 1;
                        continue;
                    }

                    while col_cursor < active_slice.len()
                        && tracked[active_slice[col_cursor]].buf_row == r
                        && tracked[active_slice[col_cursor]].col < c
                    {
                        let a = &mut tracked[active_slice[col_cursor]];
                        if a.past_eol {
                            a.target_glyph = usize::MAX;
                            a.trailing_delta = 0;
                        }
                        col_cursor += 1;
                    }

                    if cell.is_wide_lead() && c + 1 < take_end && cells[c + 1].is_wide_spacer() {
                        let glyph_idx = logical.len();
                        let mut clean = cell;
                        clean.flags.remove(
                            CellFlags::WIDE_LEAD | CellFlags::WIDE_SPACER | CellFlags::WRAP_SPACER,
                        );
                        logical.push(LogicalGlyph {
                            cell: clean,
                            width: 2,
                        });
                        while col_cursor < active_slice.len()
                            && tracked[active_slice[col_cursor]].buf_row == r
                            && tracked[active_slice[col_cursor]].col <= c + 1
                        {
                            let a = &mut tracked[active_slice[col_cursor]];
                            if a.past_eol {
                                a.target_glyph = usize::MAX;
                                a.trailing_delta = 0;
                            } else if a.is_pending_wrap && c + 1 == take_end - 1 && a.col == c + 1 {
                                if r == phys_end {
                                    // Anchor in pending_wrap after trailing wide char on full row.
                                    a.target_glyph = usize::MAX;
                                    a.trailing_delta = 0;
                                } else {
                                    a.target_glyph = glyph_idx + 1;
                                    a.is_trailing_wide = false;
                                }
                            } else if a.col == c {
                                a.target_glyph = glyph_idx;
                                a.is_trailing_wide = false;
                            } else {
                                a.target_glyph = glyph_idx;
                                a.is_trailing_wide = true;
                            }
                            col_cursor += 1;
                        }
                        c += 2;
                    } else {
                        let glyph_idx = logical.len();
                        let width = if old_cols == 1 && !cell.is_wide_lead() {
                            glyph_width_when_widening(&cell, &self.extras, &mut scratch)
                        } else {
                            1
                        };
                        let mut clean = cell;
                        clean.flags.remove(
                            CellFlags::WIDE_LEAD | CellFlags::WIDE_SPACER | CellFlags::WRAP_SPACER,
                        );
                        logical.push(LogicalGlyph { cell: clean, width });
                        while col_cursor < active_slice.len()
                            && tracked[active_slice[col_cursor]].buf_row == r
                            && tracked[active_slice[col_cursor]].col == c
                        {
                            let a = &mut tracked[active_slice[col_cursor]];
                            if a.past_eol {
                                a.target_glyph = usize::MAX;
                                a.trailing_delta = 0;
                            } else if a.is_pending_wrap && c == take_end - 1 {
                                if r == phys_end {
                                    a.target_glyph = usize::MAX;
                                    a.trailing_delta = 0;
                                } else {
                                    a.target_glyph = glyph_idx + 1;
                                    a.is_trailing_wide = false;
                                }
                            } else {
                                a.target_glyph = glyph_idx;
                                a.is_trailing_wide = false;
                            }
                            col_cursor += 1;
                        }
                        c += 1;
                    }
                }

                while col_cursor < active_slice.len()
                    && tracked[active_slice[col_cursor]].buf_row == r
                {
                    let a = &mut tracked[active_slice[col_cursor]];
                    if a.past_eol {
                        a.target_glyph = usize::MAX;
                        a.trailing_delta = 0;
                    } else if r < phys_end {
                        a.target_glyph = logical.len();
                        a.is_trailing_wide = false;
                    } else {
                        a.target_glyph = usize::MAX;
                        a.trailing_delta = if a.is_pending_wrap {
                            a.col - take_end + 1
                        } else {
                            a.col - take_end
                        };
                    }
                    col_cursor += 1;
                }
            }

            // Immediately recycle consumed physical cell vectors for subsequent output rows.
            for old_row in old_rows_vec.iter_mut().take(phys_end + 1).skip(phys_start) {
                let cells = old_row.take_cells();
                if cells.capacity() > 0 && cell_vec_pool.len() < pool_cap {
                    cell_vec_pool.push(cells);
                }
            }

            // Normalize target_glyph == usize::MAX to logical.len().
            let end_glyph = logical.len();
            for &a_idx in active_slice.iter() {
                if tracked[a_idx].target_glyph == usize::MAX {
                    tracked[a_idx].target_glyph = end_glyph;
                }
            }

            // Re-wrap logical glyphs into `new_cols`.
            if logical.is_empty() {
                let base_row_idx = new_rows_vec.len();
                let mut extra_blank_rows = 0usize;
                for &a_idx in active_slice.iter() {
                    let a = &mut tracked[a_idx];
                    if a.kind == AnchorKind::Cursor {
                        let total_pos = a.trailing_delta;
                        if total_pos < new_cols {
                            a.mapped_row = base_row_idx;
                            a.mapped_col = total_pos;
                            a.mapped_pending_wrap = false;
                        } else {
                            extra_blank_rows = (total_pos - 1) / new_cols;
                            let rem = total_pos - extra_blank_rows * new_cols;
                            a.mapped_row = base_row_idx + extra_blank_rows;
                            if rem == new_cols {
                                a.mapped_col = new_cols - 1;
                                a.mapped_pending_wrap = true;
                            } else {
                                a.mapped_col = rem;
                                a.mapped_pending_wrap = false;
                            }
                        }
                    }
                }
                for &a_idx in active_slice.iter() {
                    let a = &mut tracked[a_idx];
                    if a.kind != AnchorKind::Cursor {
                        if a.past_eol {
                            a.mapped_row = base_row_idx + extra_blank_rows;
                            a.mapped_col = new_cols;
                            a.mapped_past_eol = true;
                            a.mapped_pending_wrap = false;
                        } else {
                            let total_pos = a.trailing_delta;
                            let row_offset = (total_pos / new_cols).min(extra_blank_rows);
                            let col_in_row = total_pos - row_offset * new_cols;
                            a.mapped_row = base_row_idx + row_offset;
                            a.mapped_col = if a.kind.is_selection_point() {
                                col_in_row
                            } else {
                                col_in_row.min(new_cols - 1)
                            };
                            a.mapped_past_eol = false;
                            a.mapped_pending_wrap = !a.kind.is_selection_point()
                                && a.is_pending_wrap
                                && col_in_row >= new_cols;
                        }
                    }
                }
                for r_i in 0..=extra_blank_rows {
                    self.damage_epoch += 1;
                    let epoch = self.damage_epoch;
                    let cells = Self::acquire_cells(&mut cell_vec_pool, new_cols);
                    new_rows_vec.push(Row::from_parts(
                        cells,
                        r_i < extra_blank_rows,
                        epoch,
                        false,
                        0,
                    ));
                }
            } else {
                if active_slice.len() > 1 {
                    active_slice.sort_unstable_by_key(|&idx| tracked[idx].target_glyph);
                }
                let mut rewrap_cursor = 0usize;

                self.damage_epoch += 1;
                let mut epoch = self.damage_epoch;
                let mut cur_cells = Self::acquire_cells(&mut cell_vec_pool, new_cols);
                let mut cur_has_extras = false;
                let mut cur_trimmed_len = 0usize;
                let mut out_col = 0usize;

                for (g_idx, g) in logical.iter().enumerate() {
                    let w = if new_cols == 1 { 1 } else { g.width };
                    if w == 1 {
                        if out_col == new_cols {
                            new_rows_vec.push(Row::from_parts(
                                cur_cells,
                                true,
                                epoch,
                                cur_has_extras,
                                cur_trimmed_len,
                            ));
                            self.damage_epoch += 1;
                            epoch = self.damage_epoch;
                            cur_cells = Self::acquire_cells(&mut cell_vec_pool, new_cols);
                            cur_has_extras = false;
                            cur_trimmed_len = 0;
                            out_col = 0;
                        }
                        let row_idx = new_rows_vec.len();
                        cur_cells[out_col] = g.cell;
                        if g.cell.has_extra() {
                            cur_has_extras = true;
                        }
                        if !g.cell.is_empty() {
                            cur_trimmed_len = out_col + 1;
                        }
                        while rewrap_cursor < active_slice.len()
                            && tracked[active_slice[rewrap_cursor]].target_glyph == g_idx
                        {
                            let a = &mut tracked[active_slice[rewrap_cursor]];
                            a.mapped_row = row_idx;
                            a.mapped_col = out_col;
                            a.mapped_pending_wrap = false;
                            rewrap_cursor += 1;
                        }
                        out_col += 1;
                    } else {
                        if out_col == new_cols {
                            new_rows_vec.push(Row::from_parts(
                                cur_cells,
                                true,
                                epoch,
                                cur_has_extras,
                                cur_trimmed_len,
                            ));
                            self.damage_epoch += 1;
                            epoch = self.damage_epoch;
                            cur_cells = Self::acquire_cells(&mut cell_vec_pool, new_cols);
                            cur_has_extras = false;
                            out_col = 0;
                        } else if out_col == new_cols - 1 {
                            cur_cells[new_cols - 1] = Cell::new(
                                ' ',
                                PackedColor::Default,
                                PackedColor::Default,
                                CellFlags::WRAP_SPACER,
                            );
                            new_rows_vec.push(Row::from_parts(
                                cur_cells,
                                true,
                                epoch,
                                cur_has_extras,
                                new_cols,
                            ));
                            self.damage_epoch += 1;
                            epoch = self.damage_epoch;
                            cur_cells = Self::acquire_cells(&mut cell_vec_pool, new_cols);
                            cur_has_extras = false;
                            out_col = 0;
                        }
                        let row_idx = new_rows_vec.len();
                        let mut lead = g.cell;
                        lead.flags.insert(CellFlags::WIDE_LEAD);
                        let spacer = Cell::new(
                            ' ',
                            lead.fg,
                            lead.bg,
                            CellFlags::WIDE_SPACER | (lead.flags & CellFlags::WRITE_ATTR_MASK),
                        );
                        cur_cells[out_col] = lead;
                        cur_cells[out_col + 1] = spacer;
                        if lead.has_extra() {
                            cur_has_extras = true;
                        }
                        cur_trimmed_len = out_col + 2;
                        while rewrap_cursor < active_slice.len()
                            && tracked[active_slice[rewrap_cursor]].target_glyph == g_idx
                        {
                            let a = &mut tracked[active_slice[rewrap_cursor]];
                            a.mapped_row = row_idx;
                            a.mapped_col = out_col + usize::from(a.is_trailing_wide);
                            a.mapped_pending_wrap = false;
                            rewrap_cursor += 1;
                        }
                        out_col += 2;
                    }
                }

                let final_row_idx = new_rows_vec.len();
                let tail_anchors = &active_slice[rewrap_cursor..];
                let mut extra_blank_rows = 0usize;
                for &a_idx in tail_anchors {
                    let a = &mut tracked[a_idx];
                    if a.target_glyph == end_glyph && a.kind == AnchorKind::Cursor {
                        let total_pos = out_col + a.trailing_delta;
                        if total_pos < new_cols {
                            a.mapped_row = final_row_idx;
                            a.mapped_col = total_pos;
                            a.mapped_pending_wrap = false;
                        } else {
                            extra_blank_rows = (total_pos - 1) / new_cols;
                            let rem = total_pos - extra_blank_rows * new_cols;
                            a.mapped_row = final_row_idx + extra_blank_rows;
                            if rem == new_cols {
                                a.mapped_col = new_cols - 1;
                                a.mapped_pending_wrap = true;
                            } else {
                                a.mapped_col = rem;
                                a.mapped_pending_wrap = false;
                            }
                        }
                    }
                }
                for &a_idx in tail_anchors {
                    let a = &mut tracked[a_idx];
                    if a.target_glyph == end_glyph && a.kind != AnchorKind::Cursor {
                        if a.past_eol {
                            a.mapped_row = final_row_idx + extra_blank_rows;
                            a.mapped_col = new_cols;
                            a.mapped_past_eol = true;
                            a.mapped_pending_wrap = false;
                        } else {
                            let total_pos = out_col + a.trailing_delta;
                            let row_offset = (total_pos / new_cols).min(extra_blank_rows);
                            let col_in_row = total_pos - row_offset * new_cols;
                            a.mapped_row = final_row_idx + row_offset;
                            a.mapped_col = if a.kind.is_selection_point() {
                                col_in_row
                            } else {
                                col_in_row.min(new_cols - 1)
                            };
                            a.mapped_past_eol = false;
                            a.mapped_pending_wrap = !a.kind.is_selection_point()
                                && a.is_pending_wrap
                                && col_in_row >= new_cols;
                        }
                    }
                }

                new_rows_vec.push(Row::from_parts(
                    cur_cells,
                    extra_blank_rows > 0,
                    epoch,
                    cur_has_extras,
                    cur_trimmed_len,
                ));
                for r_i in 0..extra_blank_rows {
                    self.damage_epoch += 1;
                    epoch = self.damage_epoch;
                    let blank_cells = Self::acquire_cells(&mut cell_vec_pool, new_cols);
                    new_rows_vec.push(Row::from_parts(
                        blank_cells,
                        r_i + 1 < extra_blank_rows,
                        epoch,
                        false,
                        0,
                    ));
                }
            }

            phys_start = phys_end + 1;
        }

        // Return leftover recycled cell buffers to row_pool without touching extras (moved bitwise).
        for mut cells in cell_vec_pool {
            if self.row_pool.len() >= self.row_pool_cap {
                break;
            }
            cells.fill(Cell::BLANK);
            self.row_pool
                .push(Row::from_parts(cells, false, 0, false, 0));
        }

        let total_new = new_rows_vec.len();
        self.cols = new_cols;
        self.viewport_rows = new_rows;
        self.rows.extend(new_rows_vec);

        let evict_count = if total_new < new_rows {
            for _ in total_new..new_rows {
                self.damage_epoch += 1;
                let epoch = self.damage_epoch;
                let blank = self.acquire_row(Cell::BLANK, epoch);
                self.rows.push_back(blank);
            }
            0
        } else {
            let unclamped_hist = total_new - new_rows;
            let evict = unclamped_hist.saturating_sub(self.scrollback_limit);
            for _ in 0..evict {
                if let Some(row) = self.rows.pop_front() {
                    self.total_evicted += 1;
                    self.recycle_row(row);
                }
            }
            evict
        };

        let new_history_len = self.history_len();
        self.dirty.resize(new_rows);
        self.damage_epoch += 1;
        self.content_epoch += 1;
        self.mark_dirty_abs(self.oldest_abs_line());

        let last_kept_buf_row = (total_new - 1) - evict_count;
        let has_blank_tail = total_new < new_rows;
        let oldest_abs = self.oldest_abs_line();
        let last_kept_abs = AbsLine(self.total_evicted + last_kept_buf_row as u64);

        let mut cursor_res_row = 0usize;
        let mut cursor_res_col = 0usize;
        let mut cursor_res_pw = false;
        let mut new_display_offset = 0usize;
        let mut sel_anchor_pt: Option<RemappedPoint> = None;
        let mut sel_head_pt: Option<RemappedPoint> = None;
        let mut origin_start_pt: Option<RemappedPoint> = None;
        let mut origin_end_pt: Option<RemappedPoint> = None;
        let mut extra_res = vec![None; anchors.extra_points.len()];
        let mut extra_pw_out = false;

        for a in tracked {
            if a.blank_offset > 0 {
                let remapped = if has_blank_tail
                    && let Some(final_buf_row) = last_kept_buf_row.checked_add(a.blank_offset)
                    && final_buf_row < self.rows.len()
                {
                    let col = if a.past_eol { new_cols } else { a.col };
                    RemappedPoint::Kept {
                        pt: GridPoint::new(AbsLine(self.total_evicted + final_buf_row as u64), col),
                        past_eol: a.past_eol,
                    }
                } else {
                    let bottom_abs = if has_blank_tail {
                        AbsLine(self.total_evicted + (self.rows.len() - 1) as u64)
                    } else {
                        last_kept_abs
                    };
                    RemappedPoint::BottomCropped {
                        row: bottom_abs,
                        orig_col: a.col,
                    }
                };
                match a.kind {
                    AnchorKind::SelAnchor => sel_anchor_pt = Some(remapped),
                    AnchorKind::SelHead => sel_head_pt = Some(remapped),
                    AnchorKind::OriginStart => origin_start_pt = Some(remapped),
                    AnchorKind::OriginEnd => origin_end_pt = Some(remapped),
                    AnchorKind::Extra(idx) => {
                        let final_buf_row = if has_blank_tail {
                            last_kept_buf_row
                                .saturating_add(a.blank_offset)
                                .min(self.rows.len() - 1)
                        } else {
                            last_kept_buf_row
                        };
                        extra_res[idx] = Some(GridPoint::new(
                            AbsLine(self.total_evicted + final_buf_row as u64),
                            a.col.min(new_cols - 1),
                        ));
                    }
                    AnchorKind::Cursor | AnchorKind::ViewportTop => {}
                }
                continue;
            }
            let evicted = a.already_evicted || a.mapped_row < evict_count;
            let final_buf_row = a.mapped_row.saturating_sub(evict_count);
            let remapped = if evicted {
                RemappedPoint::TopEvicted {
                    orig_col: a.mapped_col,
                }
            } else {
                RemappedPoint::Kept {
                    pt: GridPoint::new(
                        AbsLine(self.total_evicted + final_buf_row as u64),
                        a.mapped_col,
                    ),
                    past_eol: a.mapped_past_eol,
                }
            };
            match a.kind {
                AnchorKind::Cursor => {
                    cursor_res_row = final_buf_row
                        .saturating_sub(new_history_len)
                        .min(new_rows - 1);
                    cursor_res_col = a.mapped_col.min(new_cols - 1);
                    cursor_res_pw = a.mapped_pending_wrap;
                }
                AnchorKind::ViewportTop => {
                    new_display_offset = new_history_len
                        .saturating_sub(final_buf_row)
                        .min(new_history_len);
                }
                AnchorKind::SelAnchor => sel_anchor_pt = Some(remapped),
                AnchorKind::SelHead => sel_head_pt = Some(remapped),
                AnchorKind::OriginStart => origin_start_pt = Some(remapped),
                AnchorKind::OriginEnd => origin_end_pt = Some(remapped),
                AnchorKind::Extra(idx) => {
                    if !evicted {
                        extra_res[idx] = Some(GridPoint::new(
                            AbsLine(self.total_evicted + final_buf_row as u64),
                            a.mapped_col.min(new_cols - 1),
                        ));
                        if extra_pending_wrap == Some(idx) {
                            extra_pw_out = a.mapped_pending_wrap;
                        }
                    }
                }
            }
        }

        self.display_offset = new_display_offset;

        let sel_res = sel_input.and_then(|si| {
            resolve_selection_result(
                sel_anchor_pt?,
                sel_head_pt?,
                origin_start_pt?,
                origin_end_pt?,
                si,
                oldest_abs,
                new_cols,
            )
        });
        let selection = match (sel_anchor_pt, sel_head_pt) {
            (Some(a), Some(h)) if anchors.selection.is_some() => {
                resolve_selection_pair(a, h, oldest_abs, new_cols, EvictionColPolicy::ResetToZero)
                    .map(|(ra, rh)| (ra.pt, rh.pt))
            }
            _ => None,
        };

        (
            ReflowResult {
                cursor_row: cursor_res_row,
                cursor_col: cursor_res_col,
                pending_wrap: cursor_res_pw,
                selection,
                extra_points: extra_res,
            },
            sel_res,
            extra_pw_out,
        )
    }

    fn acquire_cells(pool: &mut Vec<Vec<Cell>>, cols: usize) -> Vec<Cell> {
        if let Some(mut v) = pool.pop() {
            v.resize(cols, Cell::BLANK);
            v.fill(Cell::BLANK);
            v
        } else {
            vec![Cell::BLANK; cols]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::CellExtra;
    use crate::grid::ROW_POOL_CAP;

    fn write_ascii_line(grid: &mut Grid, vp_row: usize, text: &str) {
        grid.write_ascii_slice(
            vp_row,
            0,
            text.as_bytes(),
            PackedColor::Default,
            PackedColor::Default,
            CellFlags::empty(),
        );
    }

    #[test]
    fn grid_row_pool_cap_and_generation_survival_on_scroll() {
        let mut grid = Grid::new(ScreenKind::Primary, 10, 5, 100);
        for r in 0..5 {
            write_ascii_line(&mut grid, r, &format!("line{r}"));
        }
        let gens_before: Vec<u64> = (0..5)
            .map(|r| grid.viewport_row(r).unwrap().generation())
            .collect();

        grid.scroll_up_region(0, 4, 1, PackedColor::Default, true);
        assert_eq!(grid.history_len(), 1);
        // Surviving rows 1..5 keep their exact generation values across scroll!
        for i in 0..4 {
            assert_eq!(
                grid.viewport_row(i).unwrap().generation(),
                gens_before[i + 1]
            );
        }
        assert_ne!(grid.viewport_row(4).unwrap().generation(), gens_before[4]);

        // Scroll past 100 lines and clear history -> row_pool never exceeds ROW_POOL_CAP.
        for _ in 0..150 {
            grid.scroll_up_region(0, 4, 1, PackedColor::Default, true);
        }
        grid.clear_history();
        assert!(grid.pooled_row_count() <= ROW_POOL_CAP);
        grid.debug_verify_extra_invariants();
        grid.debug_verify_wide_invariants();
    }

    #[test]
    fn grid_extra_gc_across_all_destruction_paths() {
        let mut grid = Grid::new(ScreenKind::Primary, 10, 4, 3);
        let extra = CellExtra {
            combining: vec!['\u{0301}'],
            underline_color: Some(PackedColor::rgb(255, 0, 0)),
            hyperlink_id: None,
            text_sizing: None,
        };
        grid.write_cell(
            0,
            0,
            Cell::new(
                'e',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::empty(),
            ),
            Some(extra.clone()),
            PackedColor::Default,
        );
        grid.write_wide_cell(
            0,
            2,
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
            Some(extra.clone()),
            PackedColor::Default,
        );
        assert_eq!(grid.extras().live_count(), 2);
        grid.debug_verify_extra_invariants();
        grid.debug_verify_wide_invariants();

        // Overwrite trailing spacer of wide char -> frees wide lead's extra_id too.
        grid.write_cell(0, 3, Cell::BLANK, None, PackedColor::Default);
        assert_eq!(grid.extras().live_count(), 1);
        grid.debug_verify_extra_invariants();
        grid.debug_verify_wide_invariants();

        // ICH shifts extra off right edge.
        grid.write_cell(
            1,
            8,
            Cell::new(
                'a',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::empty(),
            ),
            Some(extra.clone()),
            PackedColor::Default,
        );
        grid.insert_cells(1, 0, 3, PackedColor::Default);
        assert_eq!(grid.extras().live_count(), 1);
        grid.debug_verify_extra_invariants();

        // DCH deletes cell with extra.
        grid.write_cell(
            2,
            2,
            Cell::new(
                'x',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::empty(),
            ),
            Some(extra.clone()),
            PackedColor::Default,
        );
        grid.delete_cells(2, 1, 2, PackedColor::Default);
        assert_eq!(grid.extras().live_count(), 1);
        grid.debug_verify_extra_invariants();

        // Scrollback eviction reclaims extra.
        for _ in 0..10 {
            grid.scroll_up_region(0, 3, 1, PackedColor::Default, true);
        }
        assert_eq!(grid.extras().live_count(), 0);
        grid.debug_verify_extra_invariants();

        // ECH, EL, ED 2, clear_all.
        grid.write_cell(
            0,
            1,
            Cell::new(
                'e',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::empty(),
            ),
            Some(extra.clone()),
            PackedColor::Default,
        );
        grid.erase_chars(0, 1, 1, PackedColor::Default);
        assert_eq!(grid.extras().live_count(), 0);

        grid.write_cell(
            0,
            2,
            Cell::new(
                'e',
                PackedColor::Default,
                PackedColor::Default,
                CellFlags::empty(),
            ),
            Some(extra),
            PackedColor::Default,
        );
        grid.erase_in_line(0, 0, 2, PackedColor::Default);
        assert_eq!(grid.extras().live_count(), 0);
        grid.debug_verify_extra_invariants();
    }

    #[test]
    fn reflow_wide_char_at_margin_and_roundtrip() {
        let mut grid = Grid::new(ScreenKind::Primary, 5, 4, 100);
        // "abcd" + wide '中' won't fit at col 4 -> WRAP_SPACER at col 4, '中' at row 1 cols 0..2.
        write_ascii_line(&mut grid, 0, "abcd");
        grid.set_wrap_spacer(0, PackedColor::Default);
        grid.write_wide_cell(
            1,
            0,
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
        grid.debug_verify_wide_invariants();

        // Widen to 8 cols -> joins back onto row 0!
        let res = grid.resize(
            8,
            4,
            ReflowAnchors {
                cursor_row: 1,
                cursor_col: 2,
                pending_wrap: false,
                selection: None,
                extra_points: Vec::new(),
            },
        );
        assert_eq!(res.cursor_row, 0);
        assert_eq!(res.cursor_col, 6);
        assert_eq!(grid.viewport_row(0).unwrap().text(grid.extras()), "abcd中");
        assert!(!grid.viewport_row(0).unwrap().soft_wrapped());
        grid.debug_verify_wide_invariants();

        // Shrink back to 5 cols -> atomic wrap restores WRAP_SPACER at col 4 and '中' on row 1.
        let res2 = grid.resize(
            5,
            4,
            ReflowAnchors {
                cursor_row: res.cursor_row,
                cursor_col: res.cursor_col,
                pending_wrap: res.pending_wrap,
                selection: None,
                extra_points: Vec::new(),
            },
        );
        assert_eq!(res2.cursor_row, 1);
        assert_eq!(res2.cursor_col, 2);
        assert!(grid.viewport_row(0).unwrap().soft_wrapped());
        assert!(grid.viewport_row(0).unwrap().cells()[4].is_wrap_spacer());
        assert_eq!(grid.viewport_row(1).unwrap().text(grid.extras()), "中");
        grid.debug_verify_wide_invariants();
    }

    #[test]
    fn reflow_pending_wrap_and_1_col_safe_roundtrip() {
        let mut grid = Grid::new(ScreenKind::Primary, 4, 4, 100);
        write_ascii_line(&mut grid, 0, "ab");
        grid.write_wide_cell(
            0,
            2,
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
        // Cursor is in pending_wrap after '中' at col 3.
        let r1 = grid.resize(
            8,
            4,
            ReflowAnchors {
                cursor_row: 0,
                cursor_col: 3,
                pending_wrap: true,
                selection: None,
                extra_points: Vec::new(),
            },
        );
        assert_eq!(r1.cursor_row, 0);
        assert_eq!(r1.cursor_col, 4);
        assert!(!r1.pending_wrap);

        // Shrink all the way down to 1 column -> must not panic and must satisfy wide invariants.
        let r_one = grid.resize(
            1,
            4,
            ReflowAnchors {
                cursor_row: r1.cursor_row,
                cursor_col: r1.cursor_col,
                pending_wrap: r1.pending_wrap,
                selection: None,
                extra_points: Vec::new(),
            },
        );
        grid.debug_verify_wide_invariants();
        assert_eq!(grid.cols(), 1);

        // Widen back to 8 columns -> '中' recovers its 2-column width!
        let r_back = grid.resize(
            8,
            4,
            ReflowAnchors {
                cursor_row: r_one.cursor_row,
                cursor_col: r_one.cursor_col,
                pending_wrap: r_one.pending_wrap,
                selection: None,
                extra_points: Vec::new(),
            },
        );
        grid.debug_verify_wide_invariants();
        assert_eq!(r_back.cursor_row, 0);
        assert_eq!(r_back.cursor_col, 4);
        assert!(grid.viewport_row(0).unwrap().cells()[2].is_wide_lead());
        assert!(grid.viewport_row(0).unwrap().cells()[3].is_wide_spacer());
    }

    #[test]
    fn reflow_height_shrink_expand_and_selection_eviction() {
        let mut grid = Grid::new(ScreenKind::Primary, 10, 6, 2);
        write_ascii_line(&mut grid, 0, "row0");
        write_ascii_line(&mut grid, 1, "row1");
        write_ascii_line(&mut grid, 2, "row2");
        write_ascii_line(&mut grid, 3, "row3");

        let sel = (
            GridPoint::new(grid.live_viewport_to_abs(0), 1),
            GridPoint::new(grid.live_viewport_to_abs(3), 2),
        );

        // Shrink height to 2 -> row0/row1 move into scrollback (limit 2), row2/row3 visible.
        let r1 = grid.resize(
            10,
            2,
            ReflowAnchors {
                cursor_row: 3,
                cursor_col: 4,
                pending_wrap: false,
                selection: Some(sel),
                extra_points: Vec::new(),
            },
        );
        assert_eq!(grid.history_len(), 2);
        assert_eq!(r1.cursor_row, 1);
        assert_eq!(grid.viewport_row(0).unwrap().text(grid.extras()), "row2");
        assert_eq!(grid.viewport_row(1).unwrap().text(grid.extras()), "row3");
        assert!(r1.selection.is_some());

        // Expand back to 6 -> row0/row1 come back out of scrollback!
        let r2 = grid.resize(
            10,
            6,
            ReflowAnchors {
                cursor_row: r1.cursor_row,
                cursor_col: r1.cursor_col,
                pending_wrap: false,
                selection: r1.selection,
                extra_points: Vec::new(),
            },
        );
        assert_eq!(grid.history_len(), 0);
        assert_eq!(r2.cursor_row, 3);
        assert_eq!(grid.viewport_row(0).unwrap().text(grid.extras()), "row0");
        assert_eq!(grid.viewport_row(3).unwrap().text(grid.extras()), "row3");

        // Shrink cols to 2 with scrollback_limit=2 -> evicts oldest rows cleanly, clamps selection.
        let r3 = grid.resize(
            2,
            2,
            ReflowAnchors {
                cursor_row: 3,
                cursor_col: 4,
                pending_wrap: false,
                selection: r2.selection,
                extra_points: Vec::new(),
            },
        );
        assert!(grid.history_len() <= 2);
        assert!(grid.total_evicted() > 0);
        assert!(r3.selection.is_some());
        grid.debug_verify_extra_invariants();
        grid.debug_verify_wide_invariants();
    }

    #[test]
    fn alt_screen_never_reflows_and_displays_separate_epoch() {
        let mut alt = Grid::new(ScreenKind::Alternate, 6, 3, 100);
        assert_eq!(alt.scrollback_limit(), 0);
        assert!(alt.damage_epoch() > (1_u64 << 63));
        write_ascii_line(&mut alt, 0, "abcdef");
        alt.resize(
            3,
            3,
            ReflowAnchors {
                cursor_row: 0,
                cursor_col: 5,
                pending_wrap: false,
                selection: None,
                extra_points: Vec::new(),
            },
        );
        assert_eq!(alt.history_len(), 0);
        assert_eq!(alt.viewport_row(0).unwrap().text(alt.extras()), "abc");
        alt.debug_verify_extra_invariants();
        alt.debug_verify_wide_invariants();
    }

    #[test]
    fn reflow_sorted_anchors_and_fast_path_preserve_points_and_protected_flags() {
        use crate::cell::TextSizing;

        let mut grid = Grid::new(ScreenKind::Primary, 12, 4, 2000);
        let mut extra_points = Vec::new();
        for i in 0..600 {
            let line_idx = i;
            if i % 3 == 0 {
                // Non-wrapped short line eligible for fast path when widening.
                grid.write_ascii_slice(
                    3,
                    0,
                    b"short",
                    PackedColor::Default,
                    PackedColor::Default,
                    CellFlags::PROTECTED,
                );
            } else {
                // Longer line that wraps when narrowing to 6 cols.
                grid.write_ascii_slice(
                    3,
                    0,
                    b"abcdefghij",
                    PackedColor::Default,
                    PackedColor::Default,
                    CellFlags::empty(),
                );
                grid.write_cell(
                    3,
                    10,
                    Cell::new(
                        'X',
                        PackedColor::Default,
                        PackedColor::Default,
                        CellFlags::PROTECTED,
                    ),
                    Some(CellExtra {
                        text_sizing: Some(TextSizing::default()),
                        ..CellExtra::default()
                    }),
                    PackedColor::Default,
                );
            }
            let abs = grid.live_viewport_to_abs(3);
            extra_points.push(GridPoint::new(abs, 0));
            extra_points.push(GridPoint::new(abs, 7));
            if line_idx + 1 < 600 {
                grid.scroll_up_region(0, 3, 1, PackedColor::Default, true);
            }
        }

        // Reverse order to verify sorted_anchors handles unsorted extra_points cleanly.
        extra_points.reverse();
        let res_narrow = grid.resize(
            6,
            4,
            ReflowAnchors {
                cursor_row: 3,
                cursor_col: 10,
                pending_wrap: false,
                selection: None,
                extra_points,
            },
        );
        grid.debug_verify_extra_invariants();
        grid.debug_verify_wide_invariants();
        assert_eq!(res_narrow.extra_points.len(), 1200);
        // Point at col 7 of the last (i = 599, i % 3 == 2 -> "abcdefghijX") wrapped to next row col 1.
        let p_last_col7 = res_narrow.extra_points[0].unwrap();
        let p_last_col0 = res_narrow.extra_points[1].unwrap();
        assert_eq!(p_last_col0.col, 0);
        assert_eq!(p_last_col7.col, 1);
        assert_eq!(p_last_col7.line.0, p_last_col0.line.0 + 1);

        // Widen to 20 cols -> fast path triggers for non-wrapped lines and unwraps wrapped lines.
        let remapped_in: Vec<GridPoint> = res_narrow.extra_points.into_iter().flatten().collect();
        let res_wide = grid.resize(
            20,
            4,
            ReflowAnchors {
                cursor_row: res_narrow.cursor_row,
                cursor_col: res_narrow.cursor_col,
                pending_wrap: res_narrow.pending_wrap,
                selection: None,
                extra_points: remapped_in,
            },
        );
        grid.debug_verify_extra_invariants();
        grid.debug_verify_wide_invariants();
        let w_col7 = res_wide.extra_points[0].unwrap();
        let w_col0 = res_wide.extra_points[1].unwrap();
        assert_eq!(w_col0.line, w_col7.line);
        assert_eq!(w_col7.col, 7);
        let row_last = grid.row_by_abs(w_col0.line).unwrap();
        assert_eq!(row_last.text(grid.extras()), "abcdefghijX");
        assert!(row_last.cells()[10].is_protected());
        assert!(
            grid.extras()
                .get(row_last.cells()[10].extra_id)
                .unwrap()
                .text_sizing
                .is_some()
        );
    }

    #[test]
    fn reflow_trailing_soft_wrapped_row_skips_fast_path_and_clears_wrap() {
        // Trailing soft-wrapped row with no following row (e.g. after erasing trailing blank rows
        // or snapshot restore) must not enter `resize_cols_in_place_fast`.
        let mut grid = Grid::new(ScreenKind::Primary, 5, 3, 10);
        write_ascii_line(&mut grid, 0, "abcd");
        grid.set_wrap_spacer(0, PackedColor::Default);
        assert!(grid.viewport_row(0).unwrap().soft_wrapped());

        let res = grid.resize(
            10,
            3,
            ReflowAnchors {
                cursor_row: 0,
                cursor_col: 4,
                pending_wrap: false,
                selection: None,
                extra_points: Vec::new(),
            },
        );
        assert_eq!(res.cursor_row, 0);
        assert_eq!(grid.viewport_row(0).unwrap().text(grid.extras()), "abcd");
        assert!(!grid.viewport_row(0).unwrap().soft_wrapped());
        grid.debug_verify_wide_invariants();
        grid.debug_verify_extra_invariants();

        // Also verify trailing soft-wrapped row without WRAP_SPACER when narrowing & widening.
        grid.set_row_soft_wrapped(0, true);
        let res2 = grid.resize(
            6,
            3,
            ReflowAnchors {
                cursor_row: 0,
                cursor_col: 2,
                pending_wrap: false,
                selection: None,
                extra_points: Vec::new(),
            },
        );
        assert_eq!(res2.cursor_row, 0);
        assert_eq!(res2.cursor_col, 2);
        assert_eq!(grid.viewport_row(0).unwrap().text(grid.extras()), "abcd");
        assert!(!grid.viewport_row(0).unwrap().soft_wrapped());
        grid.debug_verify_wide_invariants();
        grid.debug_verify_extra_invariants();
    }
}
