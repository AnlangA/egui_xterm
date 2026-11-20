//! `OSC 133` shell integration zone tracking, prompt navigation, and output extraction
//! (`shell-integration` feature).

use std::collections::{BinaryHeap, VecDeque};

use crate::grid::{AbsLine, Grid, GridPoint, Row};
use crate::term::TerminalEvent;

/// Semantic role of an `OSC 133` tracked region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ZoneKind {
    /// Shell prompt (`OSC 133;A` .. `OSC 133;B`).
    Prompt,
    /// User command input (`OSC 133;B` .. `OSC 133;C`).
    Input,
    /// Executed command output (`OSC 133;C` .. `OSC 133;D`).
    Output,
}

/// Semantic region tracked from `OSC 133` shell integration markers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ShellZone {
    /// Semantic kind of this zone.
    pub kind: ZoneKind,
    /// 0-based retained line index (`0..total_lines()`) where the zone begins.
    pub start_line: usize,
    /// 0-based column where the zone begins on `start_line`.
    pub start_col: usize,
    /// 0-based retained line index (`0..total_lines()`) where the zone ends (inclusive).
    pub end_line: usize,
    /// Exclusive 0-based column on `end_line`.
    pub end_col: usize,
    /// Exit code reported by `OSC 133;D;<exit_code>`, propagated across zones of the same command.
    pub exit_code: Option<i32>,
    /// Monotonic command group identifier linking a command's `Prompt`, `Input`, and `Output`.
    pub command_id: u64,
    start_abs: GridPoint,
    end_abs: GridPoint,
    end_is_eol: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
enum ActivePhase {
    Idle,
    InPrompt {
        start: GridPoint,
        #[cfg_attr(feature = "serde", serde(default))]
        start_pending_wrap: bool,
        #[cfg_attr(feature = "serde", serde(default))]
        next_line_start: Option<GridPoint>,
        command_id: u64,
    },
    InInput {
        start: GridPoint,
        #[cfg_attr(feature = "serde", serde(default))]
        start_pending_wrap: bool,
        #[cfg_attr(feature = "serde", serde(default))]
        next_line_start: Option<GridPoint>,
        command_id: u64,
    },
    InOutput {
        start: GridPoint,
        #[cfg_attr(feature = "serde", serde(default))]
        start_pending_wrap: bool,
        #[cfg_attr(feature = "serde", serde(default))]
        next_line_start: Option<GridPoint>,
        command_id: u64,
    },
}

const ZONES_HEAD_COMPACT_THRESHOLD: usize = 1024;

#[derive(Debug, Clone, Default)]
struct RowEolBucket {
    /// Max-heap of `(start_col, seq)` for same-line `end_is_eol` zones on this row.
    same_line: BinaryHeap<(usize, usize)>,
    /// `seq`s for multi-line `end_is_eol` zones (`start_abs.line < end_abs.line`) ending on this row.
    multi_line: Vec<usize>,
}

impl RowEolBucket {
    fn len(&self) -> usize {
        self.same_line.len() + self.multi_line.len()
    }

    fn is_empty(&self) -> bool {
        self.same_line.is_empty() && self.multi_line.is_empty()
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize))]
pub(crate) struct ShellIntegrationState {
    zones: Vec<ShellZone>,
    #[cfg_attr(feature = "serde", serde(skip))]
    zones_head: usize,
    phase: ActivePhase,
    next_command_id: u64,
    max_zones: usize,
    #[cfg_attr(feature = "serde", serde(skip))]
    last_synced_content_epoch: u64,
    #[cfg_attr(feature = "serde", serde(skip))]
    last_synced_damage_epoch: u64,
    #[cfg_attr(feature = "serde", serde(skip))]
    last_synced_oldest_line: u64,
    #[cfg_attr(feature = "serde", serde(skip))]
    frozen_prefix_len: usize,
    #[cfg_attr(feature = "serde", serde(skip))]
    live_eol_rows: VecDeque<RowEolBucket>,
    #[cfg_attr(feature = "serde", serde(skip))]
    live_eol_base_abs: u64,
    #[cfg_attr(feature = "serde", serde(skip))]
    live_eol_total: usize,
    #[cfg_attr(feature = "serde", serde(skip))]
    base_seq: usize,
}

#[cfg(feature = "serde")]
impl serde::Serialize for ShellIntegrationState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("ShellIntegrationState", 4)?;
        state.serialize_field("zones", self.zones())?;
        state.serialize_field("phase", &self.phase)?;
        state.serialize_field("next_command_id", &self.next_command_id)?;
        state.serialize_field("max_zones", &self.max_zones)?;
        state.end()
    }
}

impl PartialEq for ShellIntegrationState {
    fn eq(&self, other: &Self) -> bool {
        self.zones() == other.zones()
            && self.phase == other.phase
            && self.next_command_id == other.next_command_id
            && self.max_zones == other.max_zones
    }
}

impl Eq for ShellIntegrationState {}

impl ShellIntegrationState {
    pub(crate) fn new(scrollback_limit: usize, viewport_rows: usize) -> Self {
        Self {
            zones: Vec::new(),
            zones_head: 0,
            phase: ActivePhase::Idle,
            next_command_id: 1,
            max_zones: compute_max_zones(scrollback_limit, viewport_rows),
            last_synced_content_epoch: 0,
            last_synced_damage_epoch: 0,
            last_synced_oldest_line: 0,
            frozen_prefix_len: 0,
            live_eol_rows: VecDeque::new(),
            live_eol_base_abs: 0,
            live_eol_total: 0,
            base_seq: 0,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.zones.clear();
        self.zones_head = 0;
        self.phase = ActivePhase::Idle;
        self.next_command_id = 1;
        self.last_synced_content_epoch = 0;
        self.last_synced_damage_epoch = 0;
        self.last_synced_oldest_line = 0;
        self.frozen_prefix_len = 0;
        self.live_eol_rows.clear();
        self.live_eol_base_abs = 0;
        self.live_eol_total = 0;
        self.base_seq = 0;
    }

    #[cfg(feature = "serde")]
    pub(crate) fn snapshot_clone(&self) -> Self {
        Self {
            zones: self.zones().to_vec(),
            zones_head: 0,
            phase: self.phase,
            next_command_id: self.next_command_id,
            max_zones: self.max_zones,
            last_synced_content_epoch: self.last_synced_content_epoch,
            last_synced_damage_epoch: self.last_synced_damage_epoch,
            last_synced_oldest_line: self.last_synced_oldest_line,
            frozen_prefix_len: self.frozen_prefix_len,
            live_eol_rows: VecDeque::new(),
            live_eol_base_abs: 0,
            live_eol_total: 0,
            base_seq: 0,
        }
    }

    pub(crate) fn set_viewport_rows(&mut self, scrollback_limit: usize, viewport_rows: usize) {
        self.max_zones = compute_max_zones(scrollback_limit, viewport_rows);
        self.enforce_cap();
    }

    pub(crate) fn zones(&self) -> &[ShellZone] {
        &self.zones[self.zones_head..]
    }

    fn alloc_command_id(&mut self) -> u64 {
        let id = self.next_command_id;
        self.next_command_id = self.next_command_id.saturating_add(1);
        id
    }

    fn compact_head(&mut self) {
        if self.zones_head > 0 {
            let drain_end = self.zones_head.min(self.zones.len());
            self.zones.drain(..drain_end);
            self.zones_head = 0;
            self.base_seq = self.base_seq.saturating_add(drain_end);
            self.prune_stale_live_eol();
        }
    }

    fn prune_stale_live_eol(&mut self) {
        if self.live_eol_total == 0 {
            return;
        }
        let min_seq = self.base_seq + self.zones_head;
        let mut total = 0usize;
        for bucket in &mut self.live_eol_rows {
            if !bucket.same_line.is_empty() {
                let kept: Vec<(usize, usize)> = bucket
                    .same_line
                    .drain()
                    .filter(|&(_, seq)| seq >= min_seq)
                    .collect();
                bucket.same_line = BinaryHeap::from(kept);
            }
            if !bucket.multi_line.is_empty() {
                bucket.multi_line.retain(|&seq| seq >= min_seq);
            }
            total += bucket.len();
        }
        while self
            .live_eol_rows
            .back()
            .is_some_and(RowEolBucket::is_empty)
        {
            self.live_eol_rows.pop_back();
        }
        self.live_eol_total = total;
        if self.live_eol_total == 0 {
            self.live_eol_rows.clear();
        }
    }

    fn shift_live_eol_to(&mut self, vp_top_abs: AbsLine) {
        if vp_top_abs.0 <= self.live_eol_base_abs {
            return;
        }
        let shift = usize::try_from(vp_top_abs.0 - self.live_eol_base_abs).unwrap_or(usize::MAX);
        if shift >= self.live_eol_rows.len() {
            self.live_eol_rows.clear();
            self.live_eol_total = 0;
            self.live_eol_base_abs = vp_top_abs.0;
            return;
        }
        for _ in 0..shift {
            if let Some(bucket) = self.live_eol_rows.pop_front() {
                self.live_eol_total = self.live_eol_total.saturating_sub(bucket.len());
            }
        }
        self.live_eol_base_abs = vp_top_abs.0;
    }

    fn track_live_eol(
        &mut self,
        end_abs_line: AbsLine,
        start_col_if_same: Option<usize>,
        seq: usize,
        vp_top_abs: AbsLine,
    ) {
        if end_abs_line < vp_top_abs {
            return;
        }
        if self.live_eol_total == 0 {
            self.live_eol_rows.clear();
            self.live_eol_base_abs = vp_top_abs.0;
        } else {
            self.shift_live_eol_to(vp_top_abs);
        }
        let Some(offset) = end_abs_line
            .0
            .checked_sub(self.live_eol_base_abs)
            .and_then(|d| usize::try_from(d).ok())
        else {
            return;
        };
        while self.live_eol_rows.len() <= offset {
            self.live_eol_rows.push_back(RowEolBucket::default());
        }
        let bucket = &mut self.live_eol_rows[offset];
        if let Some(start_col) = start_col_if_same {
            bucket.same_line.push((start_col, seq));
        } else {
            bucket.multi_line.push(seq);
        }
        self.live_eol_total += 1;
    }

    fn clear_live_eol(&mut self) {
        self.live_eol_rows.clear();
        self.live_eol_base_abs = 0;
        self.live_eol_total = 0;
        self.base_seq = 0;
    }

    pub(crate) fn row_has_live_eol(&self, primary: &Grid, vp_row: usize) -> bool {
        if self.live_eol_total == 0 {
            return false;
        }
        let abs_line = primary.live_viewport_to_abs(vp_row);
        let Some(offset) = abs_line
            .0
            .checked_sub(self.live_eol_base_abs)
            .and_then(|d| usize::try_from(d).ok())
        else {
            return false;
        };
        self.live_eol_rows
            .get(offset)
            .is_some_and(|b| !b.is_empty())
    }

    pub(crate) fn on_primary_scroll_up(
        &mut self,
        primary: &Grid,
        top: usize,
        bottom: usize,
        viewport_rows: usize,
    ) {
        if self.live_eol_total == 0 {
            return;
        }
        if top == 0 && bottom + 1 == viewport_rows {
            self.advance_live_eol_base(primary);
        } else {
            self.on_primary_rows_mutated(primary, top, bottom);
        }
    }

    fn advance_live_eol_base(&mut self, primary: &Grid) {
        if self.live_eol_total == 0 {
            return;
        }
        let vp_top_abs = primary.live_viewport_to_abs(0);
        self.shift_live_eol_to(vp_top_abs);
        if self.live_eol_total == 0 {
            self.live_eol_rows.clear();
            return;
        }
        let oldest = primary.oldest_abs_line();
        if oldest == vp_top_abs
            && let Some(bucket) = self.live_eol_rows.front_mut()
            && !bucket.multi_line.is_empty()
            && primary
                .viewport_row(0)
                .is_some_and(|r| r.trimmed_len() == 0)
        {
            let multi = std::mem::take(&mut bucket.multi_line);
            self.live_eol_total = self.live_eol_total.saturating_sub(multi.len());
            for seq in multi {
                if let Some(abs_idx) = seq.checked_sub(self.base_seq)
                    && abs_idx >= self.zones_head
                    && abs_idx < self.zones.len()
                {
                    let z = &mut self.zones[abs_idx];
                    if z.end_is_eol && z.end_abs.line == oldest && z.start_abs.line < oldest {
                        z.start_abs = GridPoint::new(oldest, 0);
                        z.start_col = 0;
                        z.end_col = 0;
                        z.end_abs = z.start_abs;
                        z.end_is_eol = false;
                    }
                }
            }
        }
        if self.live_eol_total == 0 {
            self.live_eol_rows.clear();
        }
    }

    pub(crate) fn on_primary_row_mutated(&mut self, primary: &Grid, vp_row: usize) {
        if self.live_eol_total == 0 {
            return;
        }
        let abs_line = primary.live_viewport_to_abs(vp_row);
        let Some(offset) = abs_line
            .0
            .checked_sub(self.live_eol_base_abs)
            .and_then(|d| usize::try_from(d).ok())
        else {
            return;
        };
        if offset >= self.live_eol_rows.len() || self.live_eol_rows[offset].is_empty() {
            return;
        }
        let Some(row) = primary.viewport_row(vp_row) else {
            return;
        };
        let trimmed = row.trimmed_len().min(primary.cols());
        let oldest = primary.oldest_abs_line();

        {
            let bucket = &mut self.live_eol_rows[offset];
            while let Some(&(start_col, seq)) = bucket.same_line.peek() {
                if trimmed > start_col {
                    break;
                }
                bucket.same_line.pop();
                self.live_eol_total = self.live_eol_total.saturating_sub(1);
                if let Some(abs_idx) = seq.checked_sub(self.base_seq)
                    && abs_idx >= self.zones_head
                    && abs_idx < self.zones.len()
                {
                    let z = &mut self.zones[abs_idx];
                    if z.end_is_eol && z.end_abs.line == abs_line && z.start_abs.line == abs_line {
                        z.end_col = z.start_col;
                        z.end_abs = z.start_abs;
                        z.end_is_eol = false;
                    }
                }
            }
            if trimmed == 0 && abs_line == oldest && !bucket.multi_line.is_empty() {
                let multi = std::mem::take(&mut bucket.multi_line);
                self.live_eol_total = self.live_eol_total.saturating_sub(multi.len());
                for seq in multi {
                    if let Some(abs_idx) = seq.checked_sub(self.base_seq)
                        && abs_idx >= self.zones_head
                        && abs_idx < self.zones.len()
                    {
                        let z = &mut self.zones[abs_idx];
                        if z.end_is_eol && z.end_abs.line == abs_line && z.start_abs.line < oldest {
                            z.start_abs = GridPoint::new(oldest, 0);
                            z.start_col = 0;
                            z.end_col = 0;
                            z.end_abs = z.start_abs;
                            z.end_is_eol = false;
                        }
                    }
                }
            }
        }
        if self.live_eol_total == 0 {
            self.live_eol_rows.clear();
        }
    }

    pub(crate) fn on_primary_rows_mutated(
        &mut self,
        primary: &Grid,
        start_row: usize,
        end_row_inclusive: usize,
    ) {
        if self.live_eol_total == 0
            || start_row > end_row_inclusive
            || self.live_eol_rows.is_empty()
        {
            return;
        }
        let vp_top_abs = primary.live_viewport_to_abs(0);
        let end_abs = primary.live_viewport_to_abs(end_row_inclusive);
        let Some(max_offset) = end_abs
            .0
            .checked_sub(self.live_eol_base_abs)
            .and_then(|d| usize::try_from(d).ok())
        else {
            return;
        };
        let start_abs = primary.live_viewport_to_abs(start_row);
        let min_offset = usize::try_from(start_abs.0.saturating_sub(self.live_eol_base_abs))
            .unwrap_or(usize::MAX);
        let limit_offset = max_offset.min(self.live_eol_rows.len() - 1);
        if min_offset > limit_offset {
            return;
        }
        for offset in min_offset..=limit_offset {
            if self.live_eol_rows[offset].is_empty() {
                continue;
            }
            let abs_row_u64 = self.live_eol_base_abs + offset as u64;
            if let Some(vp_delta) = abs_row_u64.checked_sub(vp_top_abs.0)
                && let Ok(vp_row) = usize::try_from(vp_delta)
            {
                self.on_primary_row_mutated(primary, vp_row);
                if self.live_eol_total == 0 {
                    break;
                }
            }
        }
    }

    fn enforce_cap(&mut self) {
        let active_len = self.zones.len().saturating_sub(self.zones_head);
        if active_len > self.max_zones {
            let excess = active_len - self.max_zones;
            self.zones_head += excess;
            self.frozen_prefix_len = self.frozen_prefix_len.saturating_sub(excess);
            if self.zones_head >= ZONES_HEAD_COMPACT_THRESHOLD {
                self.compact_head();
            }
        }
    }

    fn clamp_phase_to_oldest(&mut self, oldest: AbsLine) {
        match &mut self.phase {
            ActivePhase::Idle => {}
            ActivePhase::InPrompt {
                start,
                start_pending_wrap,
                next_line_start,
                ..
            }
            | ActivePhase::InInput {
                start,
                start_pending_wrap,
                next_line_start,
                ..
            }
            | ActivePhase::InOutput {
                start,
                start_pending_wrap,
                next_line_start,
                ..
            } => {
                if start.line < oldest {
                    *start = GridPoint::new(oldest, 0);
                    *start_pending_wrap = false;
                    *next_line_start = None;
                } else if next_line_start.is_some_and(|nl| nl.line < oldest) {
                    *next_line_start = None;
                }
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "internal state transition helper"
    )]
    fn close_zone(
        &mut self,
        kind: ZoneKind,
        start: GridPoint,
        start_pending_wrap: bool,
        next_line_start: Option<GridPoint>,
        cur_pt: GridPoint,
        command_id: u64,
        exit_code: Option<i32>,
        primary: &Grid,
    ) {
        let oldest = primary.oldest_abs_line();
        let total = primary.total_len();
        let (mut effective_start, empty_from_nl) = if let Some(nl) = next_line_start {
            if cur_pt > nl {
                (nl, false)
            } else {
                (start, true)
            }
        } else if start_pending_wrap {
            if cur_pt > GridPoint::new(AbsLine(start.line.0.saturating_add(1)), 0) {
                (
                    GridPoint::new(AbsLine(start.line.0.saturating_add(1)), 0),
                    false,
                )
            } else {
                (GridPoint::new(start.line, start.col + 1), false)
            }
        } else {
            (start, false)
        };

        if effective_start.line < oldest {
            effective_start = GridPoint::new(oldest, 0);
        } else if !empty_from_nl
            && effective_start.col > 0
            && cur_pt > GridPoint::new(AbsLine(effective_start.line.0.saturating_add(1)), 0)
            && primary
                .row_by_abs(effective_start.line)
                .is_some_and(|r| !r.soft_wrapped() && r.trimmed_len() <= effective_start.col)
        {
            effective_start = GridPoint::new(AbsLine(effective_start.line.0.saturating_add(1)), 0);
        }
        let clamped_start = effective_start;

        let (end_abs, end_is_eol, end_col) = if empty_from_nl || cur_pt <= clamped_start {
            (clamped_start, false, clamped_start.col)
        } else if cur_pt.col == 0 && cur_pt.line > clamped_start.line {
            let prev_abs = AbsLine(cur_pt.line.0 - 1);
            let trimmed = primary.row_by_abs(prev_abs).map_or(0, Row::trimmed_len);
            if prev_abs == clamped_start.line && trimmed <= clamped_start.col {
                (clamped_start, false, clamped_start.col)
            } else {
                (
                    GridPoint::new(prev_abs, trimmed.saturating_sub(1)),
                    true,
                    trimmed,
                )
            }
        } else {
            (
                GridPoint::new(cur_pt.line, cur_pt.col.saturating_sub(1)),
                false,
                cur_pt.col,
            )
        };

        let start_line = usize::try_from(clamped_start.line.0.saturating_sub(oldest.0))
            .unwrap_or(0)
            .min(total.saturating_sub(1));
        let end_line = usize::try_from(end_abs.line.0.saturating_sub(oldest.0))
            .unwrap_or(start_line)
            .clamp(start_line, total.saturating_sub(1));

        let pushed_seq = self.base_seq + self.zones.len();
        let track_info = end_is_eol.then(|| {
            (
                end_abs.line,
                (clamped_start.line == end_abs.line).then_some(clamped_start.col),
            )
        });
        self.zones.push(ShellZone {
            kind,
            start_line,
            start_col: clamped_start.col,
            end_line,
            end_col,
            exit_code,
            command_id,
            start_abs: clamped_start,
            end_abs,
            end_is_eol,
        });
        if let Some((end_abs_line, start_col_if_same)) = track_info {
            let vp_top_abs = primary.live_viewport_to_abs(0);
            self.track_live_eol(end_abs_line, start_col_if_same, pushed_seq, vp_top_abs);
        }
        if self.zones.len().saturating_sub(self.zones_head) > self.max_zones {
            let mut popped = 0usize;
            while self.zones_head < self.zones.len()
                && self.zones[self.zones_head].end_abs.line < oldest
            {
                self.zones_head += 1;
                popped += 1;
            }
            if popped > 0 {
                self.frozen_prefix_len = self.frozen_prefix_len.saturating_sub(popped);
                if self.zones_head >= ZONES_HEAD_COMPACT_THRESHOLD {
                    self.compact_head();
                }
            }
            self.enforce_cap();
        }
    }

    pub(crate) fn handle_osc_133(
        &mut self,
        params: &[&[u8]],
        primary: &Grid,
        cursor_row: usize,
        cursor_col: usize,
        pending_wrap: bool,
        events: &mut Vec<TerminalEvent>,
    ) {
        let Some(&kind_bytes) = params.get(1) else {
            return;
        };
        if matches!(kind_bytes, b"A" | b"B" | b"C" | b"D") {
            self.clamp_phase_to_oldest(primary.oldest_abs_line());
        }
        let abs_row = primary.live_viewport_to_abs(cursor_row);
        let raw_pt = GridPoint::new(abs_row, cursor_col);
        let close_pt = if pending_wrap {
            GridPoint::new(abs_row, cursor_col + 1)
        } else {
            raw_pt
        };

        match kind_bytes {
            b"A" => {
                match self.phase {
                    ActivePhase::Idle => {}
                    ActivePhase::InPrompt {
                        start,
                        start_pending_wrap,
                        next_line_start,
                        command_id,
                    } => {
                        self.close_zone(
                            ZoneKind::Prompt,
                            start,
                            start_pending_wrap,
                            next_line_start,
                            close_pt,
                            command_id,
                            None,
                            primary,
                        );
                    }
                    ActivePhase::InInput {
                        start,
                        start_pending_wrap,
                        next_line_start,
                        command_id,
                    } => {
                        self.close_zone(
                            ZoneKind::Input,
                            start,
                            start_pending_wrap,
                            next_line_start,
                            close_pt,
                            command_id,
                            None,
                            primary,
                        );
                    }
                    ActivePhase::InOutput {
                        start,
                        start_pending_wrap,
                        next_line_start,
                        command_id,
                    } => {
                        self.close_zone(
                            ZoneKind::Output,
                            start,
                            start_pending_wrap,
                            next_line_start,
                            close_pt,
                            command_id,
                            None,
                            primary,
                        );
                    }
                }
                let command_id = self.alloc_command_id();
                self.phase = ActivePhase::InPrompt {
                    start: raw_pt,
                    start_pending_wrap: pending_wrap,
                    next_line_start: None,
                    command_id,
                };
            }
            b"B" => {
                let command_id = match self.phase {
                    ActivePhase::InPrompt {
                        start,
                        start_pending_wrap,
                        next_line_start,
                        command_id,
                    } => {
                        self.close_zone(
                            ZoneKind::Prompt,
                            start,
                            start_pending_wrap,
                            next_line_start,
                            close_pt,
                            command_id,
                            None,
                            primary,
                        );
                        command_id
                    }
                    ActivePhase::InInput {
                        start,
                        start_pending_wrap,
                        next_line_start,
                        command_id,
                    } => {
                        self.close_zone(
                            ZoneKind::Input,
                            start,
                            start_pending_wrap,
                            next_line_start,
                            close_pt,
                            command_id,
                            None,
                            primary,
                        );
                        self.alloc_command_id()
                    }
                    ActivePhase::InOutput {
                        start,
                        start_pending_wrap,
                        next_line_start,
                        command_id,
                    } => {
                        self.close_zone(
                            ZoneKind::Output,
                            start,
                            start_pending_wrap,
                            next_line_start,
                            close_pt,
                            command_id,
                            None,
                            primary,
                        );
                        self.alloc_command_id()
                    }
                    ActivePhase::Idle => self.alloc_command_id(),
                };
                self.phase = ActivePhase::InInput {
                    start: raw_pt,
                    start_pending_wrap: pending_wrap,
                    next_line_start: None,
                    command_id,
                };
            }
            b"C" => {
                let command_id = match self.phase {
                    ActivePhase::InPrompt {
                        start,
                        start_pending_wrap,
                        next_line_start,
                        command_id,
                    } => {
                        self.close_zone(
                            ZoneKind::Prompt,
                            start,
                            start_pending_wrap,
                            next_line_start,
                            close_pt,
                            command_id,
                            None,
                            primary,
                        );
                        command_id
                    }
                    ActivePhase::InInput {
                        start,
                        start_pending_wrap,
                        next_line_start,
                        command_id,
                    } => {
                        self.close_zone(
                            ZoneKind::Input,
                            start,
                            start_pending_wrap,
                            next_line_start,
                            close_pt,
                            command_id,
                            None,
                            primary,
                        );
                        command_id
                    }
                    ActivePhase::InOutput {
                        start,
                        start_pending_wrap,
                        next_line_start,
                        command_id,
                    } => {
                        self.close_zone(
                            ZoneKind::Output,
                            start,
                            start_pending_wrap,
                            next_line_start,
                            close_pt,
                            command_id,
                            None,
                            primary,
                        );
                        self.alloc_command_id()
                    }
                    ActivePhase::Idle => self.alloc_command_id(),
                };
                self.phase = ActivePhase::InOutput {
                    start: raw_pt,
                    start_pending_wrap: pending_wrap,
                    next_line_start: None,
                    command_id,
                };
            }
            b"D" => {
                let exit_code = params
                    .get(2)
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .and_then(|s| s.parse::<i32>().ok());
                let finished_cmd = match self.phase {
                    ActivePhase::Idle => None,
                    ActivePhase::InPrompt {
                        start,
                        start_pending_wrap,
                        next_line_start,
                        command_id,
                    } => {
                        self.close_zone(
                            ZoneKind::Prompt,
                            start,
                            start_pending_wrap,
                            next_line_start,
                            close_pt,
                            command_id,
                            exit_code,
                            primary,
                        );
                        Some(command_id)
                    }
                    ActivePhase::InInput {
                        start,
                        start_pending_wrap,
                        next_line_start,
                        command_id,
                    } => {
                        self.close_zone(
                            ZoneKind::Input,
                            start,
                            start_pending_wrap,
                            next_line_start,
                            close_pt,
                            command_id,
                            exit_code,
                            primary,
                        );
                        Some(command_id)
                    }
                    ActivePhase::InOutput {
                        start,
                        start_pending_wrap,
                        next_line_start,
                        command_id,
                    } => {
                        self.close_zone(
                            ZoneKind::Output,
                            start,
                            start_pending_wrap,
                            next_line_start,
                            close_pt,
                            command_id,
                            exit_code,
                            primary,
                        );
                        Some(command_id)
                    }
                };
                if let Some(cmd_id) = finished_cmd {
                    for z in self.zones[self.zones_head..].iter_mut().rev() {
                        if z.command_id < cmd_id {
                            break;
                        }
                        if z.command_id == cmd_id {
                            z.exit_code = exit_code;
                        }
                    }
                }
                self.phase = ActivePhase::Idle;
                events.push(TerminalEvent::CommandFinished { exit_code });
            }
            _ => {}
        }
    }

    pub(crate) fn sync_after_eviction_if_needed(&mut self, primary: &Grid) {
        let oldest = primary.oldest_abs_line().0;
        if oldest != self.last_synced_oldest_line {
            if self.zones().is_empty() && matches!(self.phase, ActivePhase::Idle) {
                self.zones.clear();
                self.zones_head = 0;
                self.last_synced_oldest_line = oldest;
                self.last_synced_content_epoch = primary.content_epoch();
                self.last_synced_damage_epoch = primary.damage_epoch();
                self.frozen_prefix_len = 0;
                self.clear_live_eol();
            } else {
                self.sync_after_eviction(primary);
            }
        }
    }

    pub(crate) fn sync_with_grid(&mut self, primary: &Grid) {
        let oldest = primary.oldest_abs_line().0;
        let content_epoch = primary.content_epoch();
        let damage_epoch = primary.damage_epoch();
        if self.zones().is_empty() && matches!(self.phase, ActivePhase::Idle) {
            self.zones.clear();
            self.zones_head = 0;
            self.last_synced_content_epoch = content_epoch;
            self.last_synced_damage_epoch = damage_epoch;
            self.last_synced_oldest_line = oldest;
            self.frozen_prefix_len = 0;
            self.clear_live_eol();
            return;
        }
        if oldest != self.last_synced_oldest_line {
            self.sync_after_eviction(primary);
            return;
        }
        if content_epoch == self.last_synced_content_epoch {
            return;
        }
        let vp_top_abs = primary.live_viewport_to_abs(0);
        let active_len = self.zones().len();
        for rel_idx in self.frozen_prefix_len..active_len {
            let abs_idx = self.zones_head + rel_idx;
            let z = &mut self.zones[abs_idx];
            if z.end_is_eol
                && let Some(row) = primary.row_by_abs(z.end_abs.line)
                && row.generation() > self.last_synced_damage_epoch
            {
                normalize_eol_zone(z, primary);
            }
            if rel_idx == self.frozen_prefix_len && z.end_abs.line < vp_top_abs {
                self.frozen_prefix_len += 1;
            }
        }
        self.advance_live_eol_base(primary);
        self.last_synced_content_epoch = content_epoch;
        self.last_synced_damage_epoch = damage_epoch;
        self.last_synced_oldest_line = oldest;
    }

    pub(crate) fn sync_after_eviction(&mut self, primary: &Grid) {
        let oldest = primary.oldest_abs_line();
        let total = primary.total_len();
        let max_line = total.saturating_sub(1);
        let cols = primary.cols();
        let vp_top_abs = primary.live_viewport_to_abs(0);
        let prev_damage_epoch = self.last_synced_damage_epoch;
        let head = self.zones_head.min(self.zones.len());
        let active_len = self.zones.len() - head;
        let old_frozen = self.frozen_prefix_len.min(active_len);

        self.clear_live_eol();
        let mut write_idx = 0usize;
        let mut new_frozen = 0usize;
        for rel_idx in 0..active_len {
            let read_idx = head + rel_idx;
            if self.zones[read_idx].end_abs.line < oldest {
                continue;
            }
            if write_idx != read_idx {
                self.zones.swap(write_idx, read_idx);
            }
            let was_frozen = rel_idx < old_frozen;
            let eol_track = {
                let z = &mut self.zones[write_idx];
                let mut clamped_start = false;
                if z.start_abs.line < oldest {
                    z.start_abs = GridPoint::new(oldest, 0);
                    z.start_col = 0;
                    clamped_start = true;
                }
                let in_frozen_scrollback =
                    was_frozen && !clamped_start && z.end_abs.line < vp_top_abs;
                if z.end_is_eol
                    && !in_frozen_scrollback
                    && (clamped_start
                        || primary
                            .row_by_abs(z.end_abs.line)
                            .is_some_and(|row| row.generation() > prev_damage_epoch))
                {
                    normalize_eol_zone(z, primary);
                }
                if !in_frozen_scrollback
                    && z.start_abs.line == z.end_abs.line
                    && z.end_col < z.start_col
                {
                    z.end_col = z.start_col.min(cols);
                    z.end_abs = z.start_abs;
                    z.end_is_eol = false;
                }
                z.start_line = usize::try_from(z.start_abs.line.0.saturating_sub(oldest.0))
                    .unwrap_or(0)
                    .min(max_line);
                z.end_line = usize::try_from(z.end_abs.line.0.saturating_sub(oldest.0))
                    .unwrap_or(z.start_line)
                    .clamp(z.start_line, max_line);

                if write_idx == new_frozen && z.end_abs.line < vp_top_abs {
                    new_frozen += 1;
                }
                (z.end_is_eol && z.end_abs.line >= vp_top_abs).then(|| {
                    (
                        z.end_abs.line,
                        (z.start_abs.line == z.end_abs.line).then_some(z.start_col),
                    )
                })
            };
            if let Some((end_abs_line, start_col_if_same)) = eol_track {
                self.track_live_eol(end_abs_line, start_col_if_same, write_idx, vp_top_abs);
            }
            write_idx += 1;
        }
        self.zones.truncate(write_idx);
        self.zones_head = 0;
        self.frozen_prefix_len = new_frozen;
        self.last_synced_content_epoch = primary.content_epoch();
        self.last_synced_damage_epoch = primary.damage_epoch();
        self.last_synced_oldest_line = oldest.0;

        self.clamp_phase_to_oldest(oldest);
    }

    #[cfg(feature = "serde")]
    pub(crate) fn sanitize_snapshot(&mut self, primary: &Grid) {
        let oldest = primary.oldest_abs_line();
        let newest = primary.newest_abs_line();
        let total = primary.total_len();
        let max_line = total.saturating_sub(1);
        let cols = primary.cols();

        self.compact_head();
        self.zones.retain(|z| {
            z.end_abs.line >= oldest
                && z.start_abs.line <= newest
                && z.end_abs.line <= newest
                && z.start_abs <= z.end_abs
        });
        let vp_top_abs = primary.live_viewport_to_abs(0);
        self.clear_live_eol();
        let mut new_frozen = 0usize;
        let mut prev_cmd_id = 1u64;
        for idx in 0..self.zones.len() {
            let eol_track = {
                let z = &mut self.zones[idx];
                if z.start_abs.line < oldest {
                    z.start_abs = GridPoint::new(oldest, 0);
                }
                z.start_abs.col = z.start_abs.col.min(cols);
                z.end_abs.col = z.end_abs.col.min(cols);
                if z.end_abs < z.start_abs {
                    z.end_abs = z.start_abs;
                }
                z.start_col = z.start_abs.col;
                if z.end_is_eol {
                    normalize_eol_zone(z, primary);
                } else if z.start_abs == z.end_abs && z.end_col <= z.start_col {
                    z.end_col = z.start_col;
                } else {
                    z.end_col = (z.end_abs.col + 1).min(cols);
                }
                if z.start_abs.line == z.end_abs.line && z.end_col < z.start_col {
                    z.end_col = z.start_col;
                    z.end_abs = z.start_abs;
                    z.end_is_eol = false;
                }
                z.start_line = usize::try_from(z.start_abs.line.0.saturating_sub(oldest.0))
                    .unwrap_or(0)
                    .min(max_line);
                z.end_line = usize::try_from(z.end_abs.line.0.saturating_sub(oldest.0))
                    .unwrap_or(z.start_line)
                    .clamp(z.start_line, max_line);
                z.command_id = z.command_id.max(prev_cmd_id);
                prev_cmd_id = z.command_id;
                if idx == new_frozen && z.end_abs.line < vp_top_abs {
                    new_frozen += 1;
                }
                (z.end_is_eol && z.end_abs.line >= vp_top_abs).then(|| {
                    (
                        z.end_abs.line,
                        (z.start_abs.line == z.end_abs.line).then_some(z.start_col),
                    )
                })
            };
            if let Some((end_abs_line, start_col_if_same)) = eol_track {
                self.track_live_eol(end_abs_line, start_col_if_same, idx, vp_top_abs);
            }
        }
        self.frozen_prefix_len = new_frozen;
        self.last_synced_content_epoch = primary.content_epoch();
        self.last_synced_damage_epoch = primary.damage_epoch();
        self.last_synced_oldest_line = oldest.0;

        let last_zone_cmd = self.zones.last().map_or(0, |z| z.command_id);

        match &mut self.phase {
            ActivePhase::Idle => {}
            ActivePhase::InPrompt {
                start,
                start_pending_wrap,
                next_line_start,
                command_id,
            }
            | ActivePhase::InInput {
                start,
                start_pending_wrap,
                next_line_start,
                command_id,
            }
            | ActivePhase::InOutput {
                start,
                start_pending_wrap,
                next_line_start,
                command_id,
            } => {
                if start.line > newest {
                    self.phase = ActivePhase::Idle;
                } else {
                    if *command_id < last_zone_cmd || *command_id == 0 {
                        *command_id = last_zone_cmd.saturating_add(1).max(1);
                    }
                    if start.line < oldest {
                        *start = GridPoint::new(oldest, 0);
                        *start_pending_wrap = false;
                        *next_line_start = None;
                    } else {
                        start.col = start.col.min(cols.saturating_sub(1));
                        *start_pending_wrap &= start.col == cols.saturating_sub(1);
                        if let Some(nl) = *next_line_start
                            && (nl < *start || nl.line > newest || nl.col > cols)
                        {
                            *next_line_start = None;
                        }
                    }
                }
            }
        }

        let phase_cmd = match &self.phase {
            ActivePhase::Idle => 0,
            ActivePhase::InPrompt { command_id, .. }
            | ActivePhase::InInput { command_id, .. }
            | ActivePhase::InOutput { command_id, .. } => *command_id,
        };
        let max_cmd = last_zone_cmd.max(phase_cmd);
        self.next_command_id = self.next_command_id.max(max_cmd.saturating_add(1)).max(1);
    }

    pub(crate) fn prepare_for_reflow(
        &mut self,
        primary: &Grid,
        pri_cur: Option<(usize, usize, bool)>,
    ) -> bool {
        let pri_cur_pt =
            pri_cur.map(|(r, c, pw)| (GridPoint::new(primary.live_viewport_to_abs(r), c), pw));
        let (start, start_pending_wrap, next_line_start) = match &mut self.phase {
            ActivePhase::Idle => return false,
            ActivePhase::InPrompt {
                start,
                start_pending_wrap,
                next_line_start,
                ..
            }
            | ActivePhase::InInput {
                start,
                start_pending_wrap,
                next_line_start,
                ..
            }
            | ActivePhase::InOutput {
                start,
                start_pending_wrap,
                next_line_start,
                ..
            } => (start, start_pending_wrap, next_line_start),
        };

        if let Some(nl) = *next_line_start {
            if pri_cur_pt.is_some_and(|(pt, _)| pt > nl) {
                *start = nl;
                *start_pending_wrap = false;
                *next_line_start = None;
            }
            return false;
        }

        if *start_pending_wrap {
            let next_line_pt = GridPoint::new(AbsLine(start.line.0.saturating_add(1)), 0);
            if primary
                .row_by_abs(start.line)
                .is_some_and(Row::soft_wrapped)
                && pri_cur_pt.is_none_or(|(pt, _)| pt > next_line_pt)
            {
                *start = next_line_pt;
                *start_pending_wrap = false;
                return false;
            }
            if pri_cur_pt.is_some_and(|(pt, _)| pt > next_line_pt)
                && primary
                    .row_by_abs(start.line)
                    .is_some_and(|r| !r.soft_wrapped() && r.trimmed_len() <= start.col + 1)
            {
                *start = next_line_pt;
                *start_pending_wrap = false;
                return false;
            }
            if pri_cur_pt == Some((*start, true)) {
                return true;
            }
            if pri_cur_pt == Some((next_line_pt, false)) {
                *next_line_start = Some(next_line_pt);
            }
            return false;
        }

        if pri_cur_pt == Some((*start, false)) {
            return true;
        }

        if start.col > 0
            && primary
                .row_by_abs(start.line)
                .is_some_and(|r| !r.soft_wrapped() && r.trimmed_len() <= start.col)
        {
            let next_line_pt = GridPoint::new(AbsLine(start.line.0.saturating_add(1)), 0);
            if pri_cur_pt.is_some_and(|(pt, _)| pt > next_line_pt) {
                *start = next_line_pt;
            } else if pri_cur_pt == Some((next_line_pt, false)) {
                *next_line_start = Some(next_line_pt);
            }
        }
        false
    }

    pub(crate) fn collect_reflow_points(&self, out: &mut Vec<GridPoint>) -> Option<usize> {
        for z in self.zones() {
            out.push(z.start_abs);
            out.push(z.end_abs);
        }
        match self.phase {
            ActivePhase::Idle => None,
            ActivePhase::InPrompt {
                start,
                start_pending_wrap,
                next_line_start,
                ..
            }
            | ActivePhase::InInput {
                start,
                start_pending_wrap,
                next_line_start,
                ..
            }
            | ActivePhase::InOutput {
                start,
                start_pending_wrap,
                next_line_start,
                ..
            } => {
                let start_idx = out.len();
                out.push(start);
                if let Some(nl) = next_line_start {
                    out.push(nl);
                }
                start_pending_wrap.then_some(start_idx)
            }
        }
    }

    pub(crate) fn apply_reflow_points(
        &mut self,
        primary: &Grid,
        mapped: &[Option<GridPoint>],
        phase_pending_wrap: bool,
        post_pri_cur: Option<(usize, usize, bool)>,
        tracking_cursor: bool,
        scrollback_limit: usize,
    ) {
        let oldest = primary.oldest_abs_line();
        let total = primary.total_len();
        let max_line = total.saturating_sub(1);

        let active_len = self.zones().len();
        let expected_zone_pts = active_len * 2;
        if mapped.len() < expected_zone_pts {
            self.sync_after_eviction(primary);
            let cols = primary.cols();
            match &mut self.phase {
                ActivePhase::Idle => {}
                ActivePhase::InPrompt {
                    start,
                    start_pending_wrap,
                    ..
                }
                | ActivePhase::InInput {
                    start,
                    start_pending_wrap,
                    ..
                }
                | ActivePhase::InOutput {
                    start,
                    start_pending_wrap,
                    ..
                } => {
                    *start_pending_wrap &= start.col == cols.saturating_sub(1);
                }
            }
            self.set_viewport_rows(scrollback_limit, primary.viewport_rows());
            return;
        }

        let cols = primary.cols();
        let vp_top_abs = primary.live_viewport_to_abs(0);
        self.clear_live_eol();
        let mut new_frozen = 0usize;
        let mut kept = Vec::with_capacity(active_len);
        let head = self.zones_head.min(self.zones.len());
        let old_zones = std::mem::take(&mut self.zones);
        self.zones_head = 0;
        for (idx, mut z) in old_zones.into_iter().skip(head).enumerate() {
            let s_opt = mapped[idx * 2];
            let e_opt = mapped[idx * 2 + 1];
            let Some(mut e_pt) = e_opt else {
                continue;
            };
            let mut s_pt = s_opt.unwrap_or_else(|| GridPoint::new(oldest, 0));
            let was_empty = !z.end_is_eol && z.start_line == z.end_line && z.start_col >= z.end_col;
            if e_pt < s_pt {
                s_pt = e_pt;
            }
            s_pt.col = s_pt.col.min(cols);
            e_pt.col = e_pt.col.min(cols);
            if e_pt < s_pt {
                e_pt = s_pt;
            }

            z.start_abs = s_pt;
            z.end_abs = e_pt;
            z.start_col = s_pt.col;
            if z.end_is_eol {
                normalize_eol_zone(&mut z, primary);
            } else if was_empty {
                z.end_abs = s_pt;
                z.end_col = z.start_col;
            } else {
                z.end_col = (e_pt.col + 1).min(cols);
            }
            if z.start_abs.line == z.end_abs.line && z.end_col < z.start_col {
                z.end_col = z.start_col;
                z.end_abs = z.start_abs;
                z.end_is_eol = false;
            }
            z.start_line = usize::try_from(z.start_abs.line.0.saturating_sub(oldest.0))
                .unwrap_or(0)
                .min(max_line);
            z.end_line = usize::try_from(z.end_abs.line.0.saturating_sub(oldest.0))
                .unwrap_or(z.start_line)
                .clamp(z.start_line, max_line);
            if kept.len() == new_frozen && z.end_abs.line < vp_top_abs {
                new_frozen += 1;
            }
            if z.end_is_eol && z.end_abs.line >= vp_top_abs {
                let start_col_if_same = (z.start_abs.line == z.end_abs.line).then_some(z.start_col);
                self.track_live_eol(z.end_abs.line, start_col_if_same, kept.len(), vp_top_abs);
            }
            kept.push(z);
        }
        self.zones = kept;
        self.frozen_prefix_len = new_frozen;
        self.last_synced_content_epoch = primary.content_epoch();
        self.last_synced_damage_epoch = primary.damage_epoch();
        self.last_synced_oldest_line = oldest.0;

        if tracking_cursor && let Some((r, c, pw)) = post_pri_cur {
            let cur_pt = GridPoint::new(primary.live_viewport_to_abs(r), c);
            match &mut self.phase {
                ActivePhase::Idle => {}
                ActivePhase::InPrompt {
                    start,
                    start_pending_wrap,
                    next_line_start,
                    ..
                }
                | ActivePhase::InInput {
                    start,
                    start_pending_wrap,
                    next_line_start,
                    ..
                }
                | ActivePhase::InOutput {
                    start,
                    start_pending_wrap,
                    next_line_start,
                    ..
                } => {
                    *start = cur_pt;
                    *start_pending_wrap = pw && cur_pt.col == cols.saturating_sub(1);
                    *next_line_start = None;
                }
            }
        } else if let Some(phase_pt) = mapped.get(expected_zone_pts).copied() {
            let nl_mapped = mapped.get(expected_zone_pts + 1).copied();
            match &mut self.phase {
                ActivePhase::Idle => {}
                ActivePhase::InPrompt {
                    start,
                    start_pending_wrap,
                    next_line_start,
                    ..
                }
                | ActivePhase::InInput {
                    start,
                    start_pending_wrap,
                    next_line_start,
                    ..
                }
                | ActivePhase::InOutput {
                    start,
                    start_pending_wrap,
                    next_line_start,
                    ..
                } => {
                    if let Some(resolved) = phase_pt {
                        *start = resolved;
                        *start_pending_wrap =
                            phase_pending_wrap && resolved.col == cols.saturating_sub(1);
                        if let Some(nl_opt) = nl_mapped {
                            *next_line_start = nl_opt.filter(|&nl| nl >= *start);
                        }
                    } else {
                        *start = GridPoint::new(oldest, 0);
                        *start_pending_wrap = false;
                        *next_line_start = None;
                    }
                }
            }
        }
        self.set_viewport_rows(scrollback_limit, primary.viewport_rows());
    }

    pub(crate) fn jump_to_prompt(&self, primary: &mut Grid, delta: i32) -> bool {
        let zones = self.zones();
        if delta == 0 || zones.is_empty() {
            return false;
        }
        let history_len = primary.history_len();
        let top_line = history_len.saturating_sub(primary.display_offset());

        let mut prompt_lines: Vec<usize> = zones
            .iter()
            .filter(|z| z.kind == ZoneKind::Prompt)
            .map(|z| z.start_line)
            .collect();
        prompt_lines.sort_unstable();
        prompt_lines.dedup();

        let target_line = if delta < 0 {
            let candidates: Vec<usize> =
                prompt_lines.into_iter().filter(|&l| l < top_line).collect();
            if candidates.is_empty() {
                return false;
            }
            let step = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
            candidates[candidates.len().saturating_sub(step)]
        } else {
            let candidates: Vec<usize> =
                prompt_lines.into_iter().filter(|&l| l > top_line).collect();
            if candidates.is_empty() {
                return false;
            }
            let step = usize::try_from(delta).unwrap_or(1).saturating_sub(1);
            candidates[step.min(candidates.len() - 1)]
        };

        let target_offset = history_len.saturating_sub(target_line);
        primary.set_display_offset(target_offset)
    }

    pub(crate) fn command_output_at(&self, primary: &Grid, line: usize) -> Option<String> {
        let zones = self.zones();
        let zone = zones
            .iter()
            .find(|z| z.kind == ZoneKind::Output && line >= z.start_line && line <= z.end_line)
            .or_else(|| {
                let cmd_id = zones
                    .iter()
                    .find(|z| line >= z.start_line && line <= z.end_line)
                    .map(|z| z.command_id)?;
                zones
                    .iter()
                    .find(|z| z.command_id == cmd_id && z.kind == ZoneKind::Output)
            })?;

        Some(extract_zone_text(primary, zone))
    }

    #[cfg(all(test, feature = "serde"))]
    pub(crate) fn tamper_for_test(&mut self) {
        if let Some(first) = self.zones().first().cloned() {
            let mut inverted = first.clone();
            std::mem::swap(&mut inverted.start_abs, &mut inverted.end_abs);
            inverted.start_abs.line = AbsLine(inverted.end_abs.line.0 + 1);
            self.zones.push(inverted);

            let mut oob_line = first.clone();
            oob_line.end_abs = GridPoint::new(AbsLine(999_999), 0);
            self.zones.push(oob_line);

            let mut oob_col = first;
            oob_col.start_abs.col = 999;
            oob_col.end_abs.col = 999;
            self.zones.push(oob_col);
        }
        self.phase = ActivePhase::InOutput {
            start: GridPoint::new(AbsLine(999_999), 999),
            start_pending_wrap: true,
            next_line_start: Some(GridPoint::new(AbsLine(1_000_000), 0)),
            command_id: 99,
        };
    }
}

fn normalize_eol_zone(z: &mut ShellZone, primary: &Grid) {
    if !z.end_is_eol {
        return;
    }
    let cols = primary.cols();
    let trimmed = primary
        .row_by_abs(z.end_abs.line)
        .map_or(z.end_abs.col, Row::trimmed_len)
        .min(cols);
    if z.start_abs.line == z.end_abs.line && trimmed <= z.start_col {
        z.end_col = z.start_col;
        z.end_abs = z.start_abs;
        z.end_is_eol = false;
    } else {
        z.end_col = trimmed;
        z.end_abs.col = trimmed.saturating_sub(1);
    }
}

fn compute_max_zones(scrollback_limit: usize, viewport_rows: usize) -> usize {
    scrollback_limit
        .saturating_add(viewport_rows)
        .saturating_mul(4)
        .max(256)
}

fn extract_zone_text(primary: &Grid, zone: &ShellZone) -> String {
    let mut out = String::new();
    let total = primary.total_len();
    if zone.start_line >= total {
        return out;
    }
    let end_line = zone.end_line.min(total - 1);

    for r in zone.start_line..=end_line {
        let Some(row) = primary.row_by_buf(r) else {
            continue;
        };
        let cells = row.cells();
        let cols = cells.len();
        let col_start = if r == zone.start_line {
            zone.start_col.min(cols)
        } else {
            0
        };
        let col_end = if r == end_line {
            let limit = if zone.end_is_eol {
                row.trimmed_len()
            } else {
                zone.end_col.min(row.trimmed_len())
            };
            limit.min(cols)
        } else if row.soft_wrapped() {
            if cols > 0 && cells[cols - 1].is_wrap_spacer() {
                cols - 1
            } else {
                cols
            }
        } else {
            row.trimmed_len().min(cols)
        };

        let row_was_empty_at_start =
            r == zone.start_line && zone.start_col > 0 && col_start >= col_end && out.is_empty();
        if col_start < col_end {
            for cell in &cells[col_start..col_end] {
                if cell.is_wide_spacer() || cell.is_wrap_spacer() {
                    continue;
                }
                if cell.extra_id != 0
                    && let Some(ext) = primary.cell_extra(cell.extra_id)
                {
                    ext.write_grapheme(cell.codepoint, &mut out);
                } else {
                    out.push(cell.codepoint);
                }
            }
        }

        if r < end_line && !row.soft_wrapped() && !row_was_empty_at_start {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{CoreConfig, Terminal};

    #[test]
    fn osc133_reflow_and_alt_screen_isolation() {
        let mut term = Terminal::new(80, 10, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07abcdefghij12345\r\n\x1b]133;D;0\x07",
        );
        assert_eq!(
            term.command_output_at(1).as_deref(),
            Some("abcdefghij12345")
        );

        // Narrowing wraps output across 2 rows; command_output_at still extracts full logical text.
        term.resize(10, 10);
        let out_zone = term
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Output)
            .unwrap();
        assert_eq!(out_zone.end_line - out_zone.start_line, 1);
        assert_eq!(
            term.command_output_at(out_zone.start_line).as_deref(),
            Some("abcdefghij12345")
        );

        // Alt screen resize and OSC 133 do not corrupt primary shell zones.
        term.advance(b"\x1b[?1049h\x1b]133;A\x07alt\x1b]133;D;9\x07");
        term.resize(40, 10);
        term.advance(b"\x1b[?1049l");
        assert_eq!(
            term.command_output_at(term.shell_zones().last().unwrap().start_line)
                .as_deref(),
            Some("abcdefghij12345")
        );

        // RIS clears all zones.
        term.advance(b"\x1bc");
        assert!(term.shell_zones().is_empty());
    }

    #[test]
    fn jump_to_prompt_edge_cases_and_eviction() {
        let cfg = CoreConfig {
            scrollback_limit: 8,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(40, 4, cfg);
        for i in 0..15 {
            term.advance(
                format!(
                    "\x1b]133;A\x07P{i:02}\x1b]133;B\x07c\r\n\x1b]133;C\x07out{i:02}\r\n\x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }
        assert!(!term.jump_to_prompt(0));
        assert!(!term.jump_to_prompt(1));
        assert!(term.jump_to_prompt(-1));
        assert!(term.jump_to_prompt(1));

        // ED 3 clears scrollback and prunes evicted zones.
        term.advance(b"\x1b[3J");
        assert!(
            term.shell_zones()
                .iter()
                .all(|z| z.end_line < term.total_lines())
        );
    }

    #[test]
    fn osc133_pending_wrap_widening_empty_zones_and_leading_blank_line() {
        // Exact-width output preserves the final column under pending_wrap.
        let mut term = Terminal::new(10, 6, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x071234567890\x1b]133;D;0\x07",
        );
        let out_zone = term
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Output)
            .unwrap();
        assert_eq!((out_zone.start_col, out_zone.end_col), (0, 10));
        assert_eq!(term.command_output_at(1).as_deref(), Some("1234567890"));

        // Exact-width prompt followed by Input wrapping onto row 1 keeps Prompt 0..10 and Input 0..2.
        let mut term = Terminal::new(10, 6, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x071234567890\x1b]133;B\x07ls\r\n\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07",
        );
        let zones = term.shell_zones();
        assert_eq!(
            (
                zones[0].kind,
                zones[0].start_line,
                zones[0].start_col,
                zones[0].end_line,
                zones[0].end_col
            ),
            (ZoneKind::Prompt, 0, 0, 0, 10)
        );
        assert_eq!(
            (
                zones[1].kind,
                zones[1].start_line,
                zones[1].start_col,
                zones[1].end_line,
                zones[1].end_col
            ),
            (ZoneKind::Input, 1, 0, 1, 2)
        );
        assert_eq!(term.command_output_at(2).as_deref(), Some("ok"));

        // Widening wrapped short zones past old end_col preserves their span,
        // and empty same-line zones at col > 0 stay empty without shifting left.
        let mut term = Terminal::new(10, 6, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x071234567890\x1b]133;B\x07x\x1b]133;C\x07y\x1b]133;D;0\x07\r\n12345\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07",
        );
        term.resize(80, 6);
        let z = term.shell_zones();
        assert_eq!((z[0].start_line, z[0].start_col, z[0].end_col), (0, 0, 10));
        assert_eq!((z[1].start_line, z[1].start_col, z[1].end_col), (0, 10, 11));
        assert_eq!((z[2].start_line, z[2].start_col, z[2].end_col), (0, 11, 12));
        assert_eq!(term.command_output_at(0).as_deref(), Some("y"));
        for empty_z in &z[3..6] {
            assert_eq!(
                (empty_z.start_line, empty_z.start_col, empty_z.end_col),
                (1, 5, 5)
            );
        }

        // OSC 133;C starting at col == 0 with a genuine blank first line preserves leading '\n'.
        let mut term = Terminal::new(20, 6, CoreConfig::default());
        term.advance(b"\x1b]133;C\x07\r\nfoo\r\n\x1b]133;D;0\x07");
        assert_eq!(term.command_output_at(0).as_deref(), Some("\nfoo"));
    }

    #[test]
    fn empty_pending_wrap_zones_do_not_spill_across_newline() {
        // Bug 2 Output: 13x10, Prompt(10)+cmd(3) -> pending_wrap -> C\r\nD;0 -> Cmd 2 on line 1.
        let mut term = Terminal::new(13, 10, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x071234567890\x1b]133;B\x07cmd\x1b]133;C\x07\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07\r\n\x1b]133;C\x07hello\r\n\x1b]133;D;0\x07",
        );
        let cmd1_out = term
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Output && z.command_id == 1)
            .unwrap();
        assert_eq!(cmd1_out.start_line, 0);
        assert_eq!(cmd1_out.end_line, 0);
        assert_eq!(term.command_output_at(1).as_deref(), Some("hello"));
        term.resize(5, 10);
        let cmd2_line = term
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Prompt && z.command_id == 2)
            .unwrap()
            .start_line;
        assert_eq!(term.command_output_at(cmd2_line).as_deref(), Some("hello"));
        term.resize(20, 10);
        assert_eq!(term.command_output_at(1).as_deref(), Some("hello"));

        // Bug 2 Input: 10-col prompt -> pending_wrap -> B\r\nC keeps Input on line 0 and Output on line 1.
        let mut term_in = Terminal::new(10, 6, CoreConfig::default());
        term_in.advance(
            b"\x1b]133;A\x071234567890\x1b]133;B\x07\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07",
        );
        let in_z = term_in
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Input)
            .unwrap();
        let out_z = term_in
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Output)
            .unwrap();
        assert_eq!(in_z.start_line, 0);
        assert_eq!(in_z.end_line, 0);
        assert_eq!(out_z.start_line, 1);
        assert_eq!(term_in.command_output_at(1).as_deref(), Some("out"));

        // Bug 2 Prompt: empty prompt in pending_wrap before \r\nB stays on line 0.
        let mut term_pr = Terminal::new(5, 6, CoreConfig::default());
        term_pr.advance(
            b"12345\x1b]133;A\x07\r\n\x1b]133;B\x07cmd\r\n\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07",
        );
        let pr_z = term_pr
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Prompt)
            .unwrap();
        assert_eq!(pr_z.start_line, 0);
        assert_eq!(pr_z.end_line, 0);

        // cols = 1 pending_wrap edge case.
        let mut term_1c = Terminal::new(1, 6, CoreConfig::default());
        term_1c.advance(
            b"\x1b]133;A\x07a\x1b]133;B\x07\x1b]133;C\x07\r\n\x1b]133;D;0\x07\x1b]133;A\x07b\x1b]133;B\x07\r\n\x1b]133;C\x07z\r\n\x1b]133;D;0\x07",
        );
        let cmd2_pr = term_1c
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Prompt && z.command_id == 2)
            .unwrap();
        assert_eq!(
            term_1c.command_output_at(cmd2_pr.start_line).as_deref(),
            Some("z")
        );
    }

    #[test]
    fn zones_at_trimmed_eol_across_newline_and_resize() {
        // Bug 3 Output: "$ cmd" + C\r\noutput_line\r\nD;0 -> resize(5, 6).
        let mut term = Terminal::new(10, 6, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07\r\noutput_line\r\n\x1b]133;D;0\x07",
        );
        term.resize(5, 6);
        let out_z = term
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Output)
            .unwrap();
        assert_eq!(
            term.command_output_at(out_z.start_line).as_deref(),
            Some("output_line")
        );
        assert_eq!(term.command_output_at(0).as_deref(), Some("output_line"));

        // Trailing space on Input ("$ ls " -> trimmed_len = 4, C at col 5) -> resize(4, 6).
        let mut term_sp = Terminal::new(10, 6, CoreConfig::default());
        term_sp.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07ls \x1b]133;C\x07\r\noutput\r\n\x1b]133;D;0\x07",
        );
        term_sp.resize(4, 6);
        assert_eq!(term_sp.command_output_at(0).as_deref(), Some("output"));

        // Leading blank line after C at trimmed_len: "$ cmd" + C\r\n\r\noutput\r\nD;0 -> resize(5, 6).
        let mut term_nl = Terminal::new(10, 6, CoreConfig::default());
        term_nl.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07\r\n\r\noutput\r\n\x1b]133;D;0\x07",
        );
        term_nl.resize(5, 6);
        assert_eq!(term_nl.command_output_at(0).as_deref(), Some("\noutput"));

        // Bug 3 Input: "$>" + B\r\ncmd\r\nC -> resize(2, 6).
        let mut term_in = Terminal::new(10, 6, CoreConfig::default());
        term_in.advance(
            b"\x1b]133;A\x07$>\x1b]133;B\x07\r\ncmd\r\n\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07",
        );
        term_in.resize(2, 6);
        let in_z = term_in
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Input)
            .unwrap();
        assert_eq!((in_z.start_line, in_z.start_col), (1, 0));
        assert_eq!(term_in.command_output_at(0).as_deref(), Some("ok"));

        // Bug 3 Prompt: "foo" + A\r\n$ \r\nB -> resize(3, 6).
        let mut term_pr = Terminal::new(10, 6, CoreConfig::default());
        term_pr.advance(
            b"foo\x1b]133;A\x07\r\n$ \r\n\x1b]133;B\x07ls\r\n\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07",
        );
        term_pr.resize(3, 6);
        let pr_z = term_pr
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Prompt)
            .unwrap();
        assert_eq!((pr_z.start_line, pr_z.start_col), (1, 0));
    }

    #[test]
    fn mid_command_active_phase_resize_and_backward_cursor() {
        // 1. InOutput at (0, 5) on "$ cmd", "\r\noutput_line" printed, resize(5, 6), then "\r\nD;0".
        let mut t1 = Terminal::new(10, 6, CoreConfig::default());
        t1.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07\r\noutput_line");
        t1.resize(5, 6);
        t1.advance(b"\r\n\x1b]133;D;0\x07");
        assert_eq!(t1.command_output_at(0).as_deref(), Some("output_line"));

        // 2a. InOutput paused right after "\r\n", resize(5, 6), then "output_line\r\nD;0".
        let mut t2a = Terminal::new(10, 6, CoreConfig::default());
        t2a.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07\r\n");
        t2a.resize(5, 6);
        t2a.advance(b"output_line\r\n\x1b]133;D;0\x07");
        assert_eq!(t2a.command_output_at(0).as_deref(), Some("output_line"));

        // 2b. InOutput paused right after "\r\n", resize(5, 6), resize(10, 6), then empty D;0 + Cmd 2.
        let mut t2b = Terminal::new(10, 6, CoreConfig::default());
        t2b.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07\r\n");
        t2b.resize(5, 6);
        t2b.resize(10, 6);
        t2b.advance(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07x\r\n\x1b]133;C\x07ok2\r\n\x1b]133;D;0\x07");
        let cmd1_out = t2b
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Output && z.command_id == 1)
            .unwrap();
        assert_eq!(cmd1_out.start_line, 0);
        assert_eq!(cmd1_out.end_line, 0);
        assert_eq!(t2b.command_output_at(1).as_deref(), Some("ok2"));

        // 2c. InOutput right at (0, 5) before "\r\n", resize(5, 6), then "\r\noutput_line\r\nD;0".
        let mut t2c = Terminal::new(10, 6, CoreConfig::default());
        t2c.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07");
        t2c.resize(5, 6);
        t2c.advance(b"\r\noutput_line\r\n\x1b]133;D;0\x07");
        assert_eq!(t2c.command_output_at(0).as_deref(), Some("output_line"));

        // 3a. InInput at pending_wrap after 10-col prompt -> resize(20, 6) -> wrapped typing -> C.
        let mut t3a = Terminal::new(10, 6, CoreConfig::default());
        t3a.advance(b"\x1b]133;A\x071234567890\x1b]133;B\x07");
        t3a.resize(20, 6);
        t3a.advance(b"1234567890more\r\n\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07");
        let in_3a = t3a
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Input)
            .unwrap();
        assert_eq!(
            (
                in_3a.start_line,
                in_3a.start_col,
                in_3a.end_line,
                in_3a.end_col
            ),
            (0, 10, 1, 4)
        );
        assert_eq!(extract_zone_text(t3a.grid(), in_3a), "1234567890more");

        // 3b. Same with "ab" already typed before resize(20, 6).
        let mut t3b = Terminal::new(10, 6, CoreConfig::default());
        t3b.advance(b"\x1b]133;A\x071234567890\x1b]133;B\x07ab");
        t3b.resize(20, 6);
        t3b.advance(b"34567890more\r\n\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07");
        let in_3b = t3b
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Input)
            .unwrap();
        assert_eq!(
            (
                in_3b.start_line,
                in_3b.start_col,
                in_3b.end_line,
                in_3b.end_col
            ),
            (0, 10, 1, 4)
        );
        assert_eq!(extract_zone_text(t3b.grid(), in_3b), "ab34567890more");

        // 4. Mid-command resize while in alternate screen (?1049h) with alt_cursor_backup active.
        let mut t4 = Terminal::new(10, 6, CoreConfig::default());
        t4.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07\r\n\x1b[?1049h");
        t4.resize(5, 6);
        t4.advance(b"\x1b[?1049loutput_line\r\n\x1b]133;D;0\x07");
        assert_eq!(t4.command_output_at(0).as_deref(), Some("output_line"));

        // 5. Backward cursor (CSI H) before OSC 133;D keeps end >= start.
        let mut t5 = Terminal::new(20, 6, CoreConfig::default());
        t5.advance(b"\r\n\r\n\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out\x1b[1;1H\x1b]133;D;0\x07");
        for z in t5.shell_zones() {
            assert!(z.start_line <= z.end_line);
            assert!(z.start_abs <= z.end_abs);
            if z.start_line == z.end_line {
                assert!(z.start_col <= z.end_col);
            }
        }

        // 6. Scrollback eviction with next_line_start clears cleanly.
        let mut t6 = Terminal::new(
            10,
            4,
            CoreConfig {
                scrollback_limit: 1,
                ..CoreConfig::default()
            },
        );
        t6.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07\r\n");
        t6.resize(5, 4);
        t6.advance(b"l1\r\nl2\r\nl3\r\nl4\r\nl5\r\n\x1b]133;D;0\x07");
        assert!(t6.command_output_at(0).is_some());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn sanitize_snapshot_clamps_same_line_end_is_eol_when_row_trimmed_before_start() {
        let mut term = Terminal::new(10, 4, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out\r\nxy\x1b]133;D;0\x07",
        );

        let mut snap = term.export_snapshot();
        {
            let si = snap.shell_integration_mut().unwrap();
            let template = si.zones[0].clone();
            si.zones.clear();

            // Case 1: start_abs == end_abs on blank row 3 (trimmed_len() == 0 < 8) with end_is_eol: true.
            let mut same_col = template.clone();
            same_col.kind = ZoneKind::Prompt;
            same_col.start_line = 3;
            same_col.start_col = 8;
            same_col.end_line = 3;
            same_col.end_col = 8;
            same_col.start_abs = GridPoint::new(AbsLine(3), 8);
            same_col.end_abs = GridPoint::new(AbsLine(3), 8);
            same_col.end_is_eol = true;
            si.zones.push(same_col);

            // Case 2: start_abs.col < end_abs.col on row 2 ("xy", trimmed_len() == 2 < 3) with end_is_eol: true.
            let mut distinct_cols = template;
            distinct_cols.kind = ZoneKind::Output;
            distinct_cols.start_line = 2;
            distinct_cols.start_col = 3;
            distinct_cols.end_line = 2;
            distinct_cols.end_col = 6;
            distinct_cols.start_abs = GridPoint::new(AbsLine(2), 3);
            distinct_cols.end_abs = GridPoint::new(AbsLine(2), 6);
            distinct_cols.end_is_eol = true;
            si.zones.push(distinct_cols);
        }

        let mut restored = Terminal::from_snapshot(snap, CoreConfig::default());
        let zones = restored.shell_zones();
        assert_eq!(zones.len(), 2);

        assert_eq!(zones[0].start_line, 3);
        assert_eq!(zones[0].end_line, 3);
        assert_eq!(zones[0].start_col, 8);
        assert_eq!(zones[0].end_col, 8);
        assert_eq!(zones[0].end_abs, GridPoint::new(AbsLine(3), 8));
        assert!(!zones[0].end_is_eol);

        assert_eq!(zones[1].start_line, 2);
        assert_eq!(zones[1].end_line, 2);
        assert_eq!(zones[1].start_col, 3);
        assert_eq!(zones[1].end_col, 3);
        assert_eq!(zones[1].end_abs, GridPoint::new(AbsLine(2), 3));
        assert!(!zones[1].end_is_eol);

        for z in zones {
            assert!(z.start_col <= z.end_col);
        }

        restored.advance(b"more\r\n");
        restored.resize(5, 4);
    }

    fn assert_zone_invariants(term: &Terminal) {
        let cols = usize::from(term.size().0);
        let total = term.total_lines();
        let oldest = term.grid().oldest_abs_line();
        let newest = term.grid().newest_abs_line();
        for z in term.shell_zones() {
            assert!(z.start_line <= z.end_line, "inverted line range: {z:?}");
            assert!(z.end_line < total, "end_line OOB ({total}): {z:?}");
            assert!(z.start_abs <= z.end_abs, "inverted abs range: {z:?}");
            assert!(z.start_col <= cols, "start_col OOB ({cols}): {z:?}");
            assert!(z.end_col <= cols, "end_col OOB ({cols}): {z:?}");
            assert!(z.start_abs.line >= oldest, "start_abs < oldest: {z:?}");
            assert!(z.end_abs.line <= newest, "end_abs > newest: {z:?}");
            if z.start_line == z.end_line {
                assert!(z.start_col <= z.end_col, "inverted same-line cols: {z:?}");
            }
        }
    }

    #[test]
    fn live_end_is_eol_erase_and_resize_never_inverts_zone() {
        // Minimal 2-call live reproducer from M3 Iteration 4 Challenger.
        let mut term = Terminal::new(12, 4, CoreConfig::default());
        term.advance(b"0123456789\x1b]133;C\x07ab\r\n\x1b]133;D;0\x07\x1b[1;9H\x1b[0K");
        assert_zone_invariants(&term);
        assert_eq!(term.shell_zones().len(), 1);
        assert_eq!(
            (
                term.shell_zones()[0].start_line,
                term.shell_zones()[0].start_col,
                term.shell_zones()[0].end_line,
                term.shell_zones()[0].end_col,
                term.shell_zones()[0].end_is_eol,
            ),
            (0, 10, 0, 10, false)
        );

        term.resize(14, 4);
        assert_zone_invariants(&term);
        assert_eq!(
            (
                term.shell_zones()[0].start_line,
                term.shell_zones()[0].start_col,
                term.shell_zones()[0].end_line,
                term.shell_zones()[0].end_col,
                term.shell_zones()[0].end_is_eol,
            ),
            (0, 10, 0, 10, false)
        );

        // Partial trim remaining strictly past start_col keeps end_is_eol: true and updates end_col.
        let mut partial = Terminal::new(12, 4, CoreConfig::default());
        partial.advance(b"0123456789\x1b]133;C\x07ab\r\n\x1b]133;D;0\x07\x1b[1;12H\x1b[0K");
        assert_zone_invariants(&partial);
        assert_eq!(
            (
                partial.shell_zones()[0].start_col,
                partial.shell_zones()[0].end_col,
                partial.shell_zones()[0].end_is_eol,
            ),
            (10, 11, true)
        );
        partial.resize(14, 4);
        assert_zone_invariants(&partial);
        assert_eq!(
            (
                partial.shell_zones()[0].start_col,
                partial.shell_zones()[0].end_col,
                partial.shell_zones()[0].end_is_eol,
            ),
            (10, 11, true)
        );
        partial.resize(6, 4);
        assert_zone_invariants(&partial);

        // Exact-to-start trim (trimmed_len == start_col) normalizes cleanly to empty non-eol zone.
        let mut exact_trim = Terminal::new(12, 4, CoreConfig::default());
        exact_trim.advance(b"0123456789\x1b]133;C\x07ab\r\n\x1b]133;D;0\x07\x1b[1;11H\x1b[0K");
        assert_zone_invariants(&exact_trim);
        assert_eq!(
            (
                exact_trim.shell_zones()[0].start_col,
                exact_trim.shell_zones()[0].end_col,
                exact_trim.shell_zones()[0].end_is_eol,
            ),
            (10, 10, false)
        );
        exact_trim.resize(14, 4);
        assert_zone_invariants(&exact_trim);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn end_is_eol_erase_snapshot_round_trip_parity_pre_and_post_resize() {
        let cases: &[(&str, u16, u16, &[u8])] = &[
            (
                "el0_past_start",
                12,
                4,
                b"0123456789\x1b]133;C\x07ab\r\n\x1b]133;D;0\x07\x1b[1;9H\x1b[0K",
            ),
            (
                "el2_to_zero_rand_rt_49_style",
                20,
                4,
                b"0123456789abcd\x1b]133;C\x07xyz\r\n\x1b]133;D;0\x07\x1b[1;1H\x1b[2K",
            ),
            (
                "partial_trim_above_start",
                16,
                4,
                b"0123456789\x1b]133;C\x07abcdef\r\n\x1b]133;D;0\x07\x1b[1;13H\x1b[0K",
            ),
            (
                "ech_and_dch_erase",
                16,
                4,
                b"0123456789\x1b]133;C\x07abcd\r\n\x1b]133;D;0\x07\x1b[1;8H\x1b[6X",
            ),
            (
                "space_overwrite_trim",
                16,
                4,
                b"0123456789\x1b]133;C\x07abcd\r\n\x1b]133;D;0\x07\x1b[1;9H      ",
            ),
            (
                "ed2_clear_multiline_end_is_eol",
                16,
                4,
                b"012345\x1b]133;C\x07abc\r\ndefg\r\n\x1b]133;D;0\x07\x1b[2J",
            ),
            (
                "decstbm_scroll_up_mutates_end_is_eol",
                16,
                4,
                b"0123456789\x1b]133;C\x07abc\r\n\x1b]133;D;0\x07\x1b[1;2r\x1b[1S",
            ),
            (
                "append_extends_end_is_eol",
                20,
                4,
                b"0123456789\x1b]133;C\x07ab\r\n\x1b]133;D;0\x07\x1b[1;13Hxyz",
            ),
        ];

        for &(name, cols, rows, seq) in cases {
            let mut live = Terminal::new(cols, rows, CoreConfig::default());
            live.advance(seq);
            assert_zone_invariants(&live);

            let snap = live.export_snapshot();
            let mut restored = Terminal::from_snapshot(snap, CoreConfig::default());
            assert_zone_invariants(&restored);
            assert_eq!(
                live.shell_zones(),
                restored.shell_zones(),
                "pre-resize parity mismatch for {name}"
            );

            for &(rw, rh) in &[(cols + 4, rows), (5, rows + 1), (1, rows), (24, 6)] {
                live.resize(rw, rh);
                restored.resize(rw, rh);
                assert_zone_invariants(&live);
                assert_zone_invariants(&restored);
                assert_eq!(
                    live.shell_zones(),
                    restored.shell_zones(),
                    "post-resize ({rw}x{rh}) parity mismatch for {name}"
                );
            }
        }
    }

    #[cfg(feature = "serde")]
    #[test]
    fn randomized_shell_zone_invariants_and_snapshot_parity() {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next_u32 = || -> u32 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 32) as u32
        };

        for trial in 0..100u32 {
            let init_cols = ((next_u32() % 20) + 2) as u16;
            let init_rows = ((next_u32() % 6) + 2) as u16;
            let sb = [0usize, 2, 8, 32][(next_u32() % 4) as usize];
            let cfg = CoreConfig {
                scrollback_limit: sb,
                ..CoreConfig::default()
            };
            let mut term = Terminal::new(init_cols, init_rows, cfg.clone());

            for step in 0..24u32 {
                let mut buf = Vec::new();
                for _ in 0..4 {
                    match next_u32() % 14 {
                        0 => buf.extend_from_slice(b"\x1b]133;A\x07$ "),
                        1 => buf.extend_from_slice(b"\x1b]133;B\x07cmd"),
                        2 => buf.extend_from_slice(b"\x1b]133;C\x07"),
                        3 => {
                            let code = next_u32() % 3;
                            buf.extend_from_slice(format!("\x1b]133;D;{code}\x07").as_bytes());
                        }
                        4 => buf.extend_from_slice(b"0123456789abcdef"),
                        5 => buf.extend_from_slice(b"\r\n"),
                        6 => {
                            let (c, r) = term.size();
                            let row = (next_u32() % u32::from(r)) + 1;
                            let col = (next_u32() % u32::from(c)) + 1;
                            let mode = next_u32() % 3;
                            buf.extend_from_slice(
                                format!("\x1b[{row};{col}H\x1b[{mode}K").as_bytes(),
                            );
                        }
                        7 => {
                            let mode = next_u32() % 4;
                            buf.extend_from_slice(format!("\x1b[{mode}J").as_bytes());
                        }
                        8 => {
                            let n = (next_u32() % 6) + 1;
                            match next_u32() % 3 {
                                0 => buf.extend_from_slice(format!("\x1b[{n}P").as_bytes()),
                                1 => buf.extend_from_slice(format!("\x1b[{n}X").as_bytes()),
                                _ => buf.extend_from_slice(format!("\x1b[{n}@").as_bytes()),
                            }
                        }
                        9 => buf.extend_from_slice(b"\x1b[1;2r\x1b[1S\x1b[r"),
                        10 => buf.extend_from_slice(b"   "),
                        11 => buf.extend_from_slice("中文🚀".as_bytes()),
                        12 => buf.extend_from_slice(b"\x1b[?1049hhi\x1b[?1049l"),
                        _ => buf.extend_from_slice(b"\x1b[1;1H"),
                    }
                }
                term.advance(&buf);
                assert_zone_invariants(&term);

                let mut restored = Terminal::from_snapshot(term.export_snapshot(), cfg.clone());
                assert_zone_invariants(&restored);
                assert_eq!(
                    term.shell_zones(),
                    restored.shell_zones(),
                    "pre-resize mismatch at trial {trial} step {step}"
                );

                if step % 4 == 3 {
                    let nw = ((next_u32() % 24) + 1) as u16;
                    let nh = ((next_u32() % 8) + 1) as u16;
                    term.resize(nw, nh);
                    restored.resize(nw, nh);
                    assert_zone_invariants(&term);
                    assert_zone_invariants(&restored);
                    assert_eq!(
                        term.shell_zones(),
                        restored.shell_zones(),
                        "post-resize ({nw}x{nh}) mismatch at trial {trial} step {step}"
                    );
                }
            }
        }
    }

    #[test]
    fn mid_advance_eviction_osc133_ed3_decstbm_and_frozen_prefix_preservation() {
        use crate::color::PackedColor;
        use crate::grid::ScreenKind;

        // 1. Direct ShellIntegrationState + Grid check: steady-state eviction preserves
        // frozen_prefix_len for retained scrollback zones and tracks last_synced_oldest_line.
        let mut grid = Grid::new(ScreenKind::Primary, 20, 4, 8);
        let mut si = ShellIntegrationState::new(8, 4);
        let mut events = Vec::new();
        for i in 0..5 {
            si.handle_osc_133(&[b"133", b"A"], &grid, 3, 0, false, &mut events);
            si.handle_osc_133(&[b"133", b"B"], &grid, 3, 2, false, &mut events);
            si.handle_osc_133(&[b"133", b"C"], &grid, 3, 4, false, &mut events);
            grid.scroll_up_region(0, 3, 1, PackedColor::Default, true);
            si.handle_osc_133(&[b"133", b"D", b"0"], &grid, 3, 0, false, &mut events);
            let _ = i;
        }
        si.sync_with_grid(&grid);
        assert!(si.frozen_prefix_len > 0);
        assert_eq!(si.last_synced_oldest_line, grid.oldest_abs_line().0);

        // Scroll more lines so eviction occurs while keeping newer zones in scrollback.
        for _ in 0..5 {
            grid.scroll_up_region(0, 3, 1, PackedColor::Default, true);
        }
        si.sync_with_grid(&grid);
        assert!(
            si.frozen_prefix_len > 0,
            "frozen_prefix_len should remain > 0 across steady-state eviction"
        );
        assert_eq!(si.last_synced_oldest_line, grid.oldest_abs_line().0);

        // 2. Empty zones + ActivePhase evicted mid-advance before OSC 133;D.
        let cfg = CoreConfig {
            scrollback_limit: 4,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(20, 4, cfg);
        term.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07l0\r\nl1\r\nl2\r\nl3\r\nl4\r\nl5\r\nl6\r\nl7\r\n\x1b]133;D;0\x07");
        assert_zone_invariants(&term);
        assert_eq!(term.shell_zones().len(), 1);
        assert_eq!(term.shell_zones()[0].kind, ZoneKind::Output);
        assert_eq!(term.shell_zones()[0].start_line, 0);

        // 3. Mid-advance eviction with retained zones followed by new OSC 133;A/B/C/D and ED 3.
        let mut stream = Vec::new();
        for i in 0..12 {
            stream.extend_from_slice(
                format!(
                    "\x1b]133;A\x07P{i:02}\x1b]133;B\x07c{i:02}\r\n\x1b]133;C\x07out{i:02}\r\n\x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }
        stream.extend_from_slice(b"tail1\r\ntail2\r\ntail3\r\n\x1b]133;A\x07last\x1b]133;B\x07\r\n\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07");
        term.advance(&stream);
        assert_zone_invariants(&term);
        assert_eq!(
            term.command_output_at(term.shell_zones().last().unwrap().start_line)
                .as_deref(),
            Some("ok")
        );

        // ED 3 mid-advance followed by another command.
        term.advance(
            b"\x1b[3J\x1b]133;A\x07$ \x1b]133;B\x07pwd\r\n\x1b]133;C\x07/tmp\r\n\x1b]133;D;0\x07",
        );
        assert_zone_invariants(&term);
        assert_eq!(
            term.command_output_at(term.shell_zones().last().unwrap().start_line)
                .as_deref(),
            Some("/tmp")
        );

        // 4. DECSTBM partial scroll region mutating an end_is_eol zone without eviction.
        let mut decstbm = Terminal::new(20, 5, CoreConfig::default());
        decstbm.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07row1\r\nrow2\r\n\x1b]133;D;0\x07",
        );
        decstbm.advance(b"\x1b[2;3r\x1b[1S\x1b[r");
        assert_zone_invariants(&decstbm);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn steady_state_full_scrollback_streaming_and_snapshot_parity() {
        let cfg = CoreConfig {
            scrollback_limit: 16,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(24, 5, cfg.clone());

        // Populate scrollback beyond scrollback_limit with OSC 133 zones, including a
        // backward-cursor zone and a partially-evicted end_is_eol zone at the oldest boundary.
        for i in 0..10 {
            term.advance(
                format!(
                    "\x1b]133;A\x07P{i:02}\x1b]133;B\x07run\r\n\x1b]133;C\x07line{i:02}\r\n\x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }
        // Backward cursor creates an out-of-order zone before scrolling further.
        term.advance(
            b"\x1b[1;1H\x1b]133;A\x07BW\x1b]133;B\x07cmd\x1b]133;C\x07val\x1b]133;D;0\x07\x1b[5;1H",
        );

        // Stream steady-state full-scrollback chunks.
        for step in 0..8 {
            let chunk = format!("s{step}_a\r\ns{step}_b\r\ns{step}_c\r\n");
            term.advance(chunk.as_bytes());
            assert_zone_invariants(&term);

            let mut restored = Terminal::from_snapshot(term.export_snapshot(), cfg.clone());
            assert_zone_invariants(&restored);
            assert_eq!(
                term.shell_zones(),
                restored.shell_zones(),
                "steady-state pre-resize parity mismatch at step {step}"
            );

            if step % 3 == 2 {
                term.resize(12, 5);
                restored.resize(12, 5);
                assert_zone_invariants(&term);
                assert_zone_invariants(&restored);
                assert_eq!(
                    term.shell_zones(),
                    restored.shell_zones(),
                    "steady-state post-resize parity mismatch at step {step}"
                );
                term.resize(24, 5);
                restored.resize(24, 5);
            }
        }

        // ED 3 mid-advance after steady-state streaming preserves snapshot parity.
        term.advance(
            b"\x1b[3J\x1b]133;A\x07$ \x1b]133;B\x07echo\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07",
        );
        assert_zone_invariants(&term);
        let restored = Terminal::from_snapshot(term.export_snapshot(), cfg);
        assert_zone_invariants(&restored);
        assert_eq!(term.shell_zones(), restored.shell_zones());
    }

    #[test]
    fn dense_osc133_max_zones_head_offset_compaction_and_reflow() {
        use crate::color::PackedColor;
        use crate::grid::ScreenKind;

        // 1. Direct ShellIntegrationState: zones_head advances past max_zones (256),
        // compacts at ZONES_HEAD_COMPACT_THRESHOLD (1024), and advances again cleanly.
        let grid = Grid::new(ScreenKind::Primary, 40, 4, 0);
        let mut si = ShellIntegrationState::new(0, 4);
        assert_eq!(si.max_zones, 256);
        let mut events = Vec::new();

        for i in 0..100u32 {
            // 3 zones per command => 300 zones => zones_head == 44 (< 1024).
            si.handle_osc_133(&[b"133", b"A"], &grid, 0, 0, false, &mut events);
            si.handle_osc_133(&[b"133", b"B"], &grid, 0, 2, false, &mut events);
            si.handle_osc_133(&[b"133", b"C"], &grid, 0, 4, false, &mut events);
            let code = format!("{i}");
            si.handle_osc_133(
                &[b"133", b"D", code.as_bytes()],
                &grid,
                0,
                8,
                false,
                &mut events,
            );
        }
        assert_eq!(si.zones().len(), 256);
        assert_eq!(si.zones_head, 44);
        let tail_zones: Vec<_> = si.zones().iter().rev().take(3).collect();
        assert!(tail_zones.iter().all(|z| z.exit_code == Some(99)));
        assert_eq!(
            si.zones()[si.zones().len() - 4].exit_code,
            Some(98),
            "O(1) backward D scan must not touch earlier commands"
        );

        // Push enough commands to cross ZONES_HEAD_COMPACT_THRESHOLD (1024) and then advance again.
        for i in 100..470u32 {
            si.handle_osc_133(&[b"133", b"A"], &grid, 0, 0, false, &mut events);
            si.handle_osc_133(&[b"133", b"B"], &grid, 0, 2, false, &mut events);
            si.handle_osc_133(&[b"133", b"C"], &grid, 0, 4, false, &mut events);
            let code = format!("{i}");
            si.handle_osc_133(
                &[b"133", b"D", code.as_bytes()],
                &grid,
                0,
                8,
                false,
                &mut events,
            );
        }
        assert_eq!(si.zones().len(), 256);
        assert!(
            si.zones_head > 0 && si.zones_head < ZONES_HEAD_COMPACT_THRESHOLD,
            "expected post-compaction non-zero zones_head, got {}",
            si.zones_head
        );

        // 2. When max_zones is reached while evicted front zones sit at the front,
        // close_zone pops evicted front zones via zones_head before dropping live zones.
        let mut grid2 = Grid::new(ScreenKind::Primary, 20, 4, 0);
        let mut si2 = ShellIntegrationState::new(0, 4);
        for _ in 0..80 {
            si2.handle_osc_133(&[b"133", b"A"], &grid2, 3, 0, false, &mut events);
            si2.handle_osc_133(&[b"133", b"B"], &grid2, 3, 2, false, &mut events);
            si2.handle_osc_133(&[b"133", b"C"], &grid2, 3, 4, false, &mut events);
            si2.handle_osc_133(&[b"133", b"D", b"0"], &grid2, 3, 8, false, &mut events);
        }
        // 240 zones on line 3; now scroll 4 lines so all 240 front zones have end_abs.line < oldest.
        for _ in 0..4 {
            grid2.scroll_up_region(0, 3, 1, PackedColor::Default, true);
        }
        let oldest_after_scroll = grid2.oldest_abs_line();
        // Push 10 live commands (30 zones) on the new viewport -> crosses 256;
        // all 240 evicted front zones should be popped cleanly while all 30 live zones stay!
        for _ in 0..10 {
            si2.handle_osc_133(&[b"133", b"A"], &grid2, 3, 0, false, &mut events);
            si2.handle_osc_133(&[b"133", b"B"], &grid2, 3, 2, false, &mut events);
            si2.handle_osc_133(&[b"133", b"C"], &grid2, 3, 4, false, &mut events);
            si2.handle_osc_133(&[b"133", b"D", b"7"], &grid2, 3, 8, false, &mut events);
        }
        assert_eq!(si2.zones().len(), 30);
        assert!(
            si2.zones()
                .iter()
                .all(|z| z.end_abs.line >= oldest_after_scroll && z.exit_code == Some(7))
        );

        // 3. Terminal-level dense OSC 133 without eviction triggers zones_head > 0 and reflows cleanly.
        let cfg = CoreConfig {
            scrollback_limit: 0,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(30, 4, cfg);
        let mut batch = Vec::new();
        for i in 0..120u32 {
            batch.extend_from_slice(
                format!("\r\x1b]133;A\x07$ \x1b]133;B\x07c\x1b]133;C\x07o{i}\x1b]133;D;{i}\x07")
                    .as_bytes(),
            );
        }
        term.advance(&batch);
        assert_zone_invariants(&term);
        assert_eq!(term.shell_zones().len(), 256);
        let last_zone = term.shell_zones().last().unwrap();
        assert_eq!(last_zone.exit_code, Some(119));
        assert_eq!(extract_zone_text(term.grid(), last_zone), "o119");
        assert!(term.command_output_at(0).is_some());
        term.resize(12, 4);
        assert_zone_invariants(&term);
        assert_eq!(term.shell_zones().len(), 256);
        term.resize(30, 4);
        assert_zone_invariants(&term);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn zones_head_nonzero_snapshot_parity_and_partial_eq() {
        use crate::grid::ScreenKind;
        use serde::Serialize;

        #[derive(Debug)]
        struct DummyErr;
        impl std::fmt::Display for DummyErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("dummy")
            }
        }
        impl std::error::Error for DummyErr {}
        impl serde::ser::Error for DummyErr {
            fn custom<T: std::fmt::Display>(_msg: T) -> Self {
                Self
            }
        }
        struct CountSeq(usize);
        impl serde::ser::SerializeSeq for CountSeq {
            type Ok = usize;
            type Error = DummyErr;
            fn serialize_element<T: ?Sized + Serialize>(&mut self, _: &T) -> Result<(), DummyErr> {
                self.0 += 1;
                Ok(())
            }
            fn end(self) -> Result<usize, DummyErr> {
                Ok(self.0)
            }
        }
        struct CountSeqSer;
        impl serde::Serializer for CountSeqSer {
            type Ok = usize;
            type Error = DummyErr;
            type SerializeSeq = CountSeq;
            type SerializeTuple = serde::ser::Impossible<usize, DummyErr>;
            type SerializeTupleStruct = serde::ser::Impossible<usize, DummyErr>;
            type SerializeTupleVariant = serde::ser::Impossible<usize, DummyErr>;
            type SerializeMap = serde::ser::Impossible<usize, DummyErr>;
            type SerializeStruct = serde::ser::Impossible<usize, DummyErr>;
            type SerializeStructVariant = serde::ser::Impossible<usize, DummyErr>;
            fn serialize_bool(self, _: bool) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_i8(self, _: i8) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_i16(self, _: i16) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_i32(self, _: i32) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_i64(self, _: i64) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_u8(self, _: u8) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_u16(self, _: u16) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_u32(self, _: u32) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_u64(self, _: u64) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_f32(self, _: f32) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_f64(self, _: f64) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_char(self, _: char) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_str(self, _: &str) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_bytes(self, _: &[u8]) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_none(self) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_some<T: ?Sized + Serialize>(self, _: &T) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_unit(self) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_unit_struct(self, _: &'static str) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_unit_variant(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
            ) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_newtype_struct<T: ?Sized + Serialize>(
                self,
                _: &'static str,
                _: &T,
            ) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_newtype_variant<T: ?Sized + Serialize>(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
                _: &T,
            ) -> Result<usize, DummyErr> {
                Ok(0)
            }
            fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, DummyErr> {
                Ok(CountSeq(0))
            }
            fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_tuple_struct(
                self,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeTupleStruct, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_tuple_variant(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeTupleVariant, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_struct(
                self,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeStruct, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_struct_variant(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeStructVariant, DummyErr> {
                Err(DummyErr)
            }
        }
        struct OuterStruct {
            zones_count: usize,
        }
        impl serde::ser::SerializeStruct for OuterStruct {
            type Ok = usize;
            type Error = DummyErr;
            fn serialize_field<T: ?Sized + Serialize>(
                &mut self,
                key: &'static str,
                value: &T,
            ) -> Result<(), DummyErr> {
                if key == "zones" {
                    self.zones_count = value.serialize(CountSeqSer)?;
                }
                Ok(())
            }
            fn end(self) -> Result<usize, DummyErr> {
                Ok(self.zones_count)
            }
        }
        struct OuterSer;
        impl serde::Serializer for OuterSer {
            type Ok = usize;
            type Error = DummyErr;
            type SerializeSeq = serde::ser::Impossible<usize, DummyErr>;
            type SerializeTuple = serde::ser::Impossible<usize, DummyErr>;
            type SerializeTupleStruct = serde::ser::Impossible<usize, DummyErr>;
            type SerializeTupleVariant = serde::ser::Impossible<usize, DummyErr>;
            type SerializeMap = serde::ser::Impossible<usize, DummyErr>;
            type SerializeStruct = OuterStruct;
            type SerializeStructVariant = serde::ser::Impossible<usize, DummyErr>;
            fn serialize_bool(self, _: bool) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_i8(self, _: i8) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_i16(self, _: i16) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_i32(self, _: i32) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_i64(self, _: i64) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_u8(self, _: u8) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_u16(self, _: u16) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_u32(self, _: u32) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_u64(self, _: u64) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_f32(self, _: f32) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_f64(self, _: f64) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_char(self, _: char) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_str(self, _: &str) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_bytes(self, _: &[u8]) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_none(self) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_some<T: ?Sized + Serialize>(self, _: &T) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_unit(self) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_unit_struct(self, _: &'static str) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_unit_variant(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
            ) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_newtype_struct<T: ?Sized + Serialize>(
                self,
                _: &'static str,
                _: &T,
            ) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_newtype_variant<T: ?Sized + Serialize>(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
                _: &T,
            ) -> Result<usize, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_tuple_struct(
                self,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeTupleStruct, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_tuple_variant(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeTupleVariant, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, DummyErr> {
                Err(DummyErr)
            }
            fn serialize_struct(
                self,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeStruct, DummyErr> {
                Ok(OuterStruct { zones_count: 0 })
            }
            fn serialize_struct_variant(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeStructVariant, DummyErr> {
                Err(DummyErr)
            }
        }

        let grid = Grid::new(ScreenKind::Primary, 30, 4, 0);
        let mut si = ShellIntegrationState::new(0, 4);
        let mut events = Vec::new();
        for _ in 0..100 {
            si.handle_osc_133(&[b"133", b"A"], &grid, 0, 0, false, &mut events);
            si.handle_osc_133(&[b"133", b"B"], &grid, 0, 2, false, &mut events);
            si.handle_osc_133(&[b"133", b"C"], &grid, 0, 4, false, &mut events);
            si.handle_osc_133(&[b"133", b"D", b"0"], &grid, 0, 8, false, &mut events);
        }
        assert_eq!(si.zones_head, 44);
        assert_eq!(si.zones.len(), 300);
        assert_eq!(si.zones().len(), 256);

        // Verify custom Serialize on uncompacted state emits active slice length (256), not 300.
        let emitted = si.serialize(OuterSer).unwrap();
        assert_eq!(emitted, 256);

        // PartialEq ignores evicted pre-zones_head elements.
        let mut compacted = si.clone();
        compacted.compact_head();
        assert_eq!(compacted.zones_head, 0);
        assert_eq!(compacted.zones.len(), 256);
        assert_eq!(si, compacted);

        // snapshot_clone strips pre-zones_head elements.
        let snapped = si.snapshot_clone();
        assert_eq!(snapped.zones_head, 0);
        assert_eq!(snapped.zones.len(), 256);
        assert_eq!(si, snapped);

        // Terminal export_snapshot / from_snapshot parity when zones_head > 0.
        let cfg = CoreConfig {
            scrollback_limit: 0,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(30, 4, cfg.clone());
        let mut batch = Vec::new();
        for i in 0..100u32 {
            batch.extend_from_slice(
                format!("\r\x1b]133;A\x07$ \x1b]133;B\x07c\x1b]133;C\x07o{i}\x1b]133;D;{i}\x07")
                    .as_bytes(),
            );
        }
        term.advance(&batch);
        let snap_direct = term.export_snapshot();
        let restored_direct = Terminal::from_snapshot(snap_direct, cfg.clone());
        assert_zone_invariants(&restored_direct);
        assert_eq!(restored_direct.shell_zones(), term.shell_zones());

        let mut snap = term.export_snapshot();
        {
            let snap_si = snap.shell_integration_mut().unwrap();
            assert_eq!(snap_si.zones_head, 0);
            assert_eq!(snap_si.zones.len(), 256);
            // Also verify sanitize_snapshot compacts an uncompacted state with zones_head > 0.
            *snap_si = si;
        }
        let mut restored = Terminal::from_snapshot(snap, cfg);
        assert_zone_invariants(&restored);
        assert_eq!(restored.shell_zones().len(), 256);

        restored.resize(15, 4);
        assert_zone_invariants(&restored);
        assert_eq!(restored.shell_zones().len(), 256);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn sanitize_snapshot_normalizes_non_monotonic_command_ids() {
        let mut term = Terminal::new(20, 6, CoreConfig::default());
        term.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07\x1b]133;C\x07run",
        );
        let mut snap = term.export_snapshot();
        {
            let si = snap.shell_integration_mut().unwrap();
            assert_eq!(si.zones.len(), 3);
            si.zones[0].command_id = 500;
            si.zones[1].command_id = 10;
            si.zones[2].command_id = 20;
            si.next_command_id = 1;
            if let ActivePhase::InOutput { command_id, .. } = &mut si.phase {
                *command_id = 5;
            }
        }

        let mut restored = Terminal::from_snapshot(snap, CoreConfig::default());
        assert_zone_invariants(&restored);
        let z = restored.shell_zones();
        assert_eq!(z.len(), 3);
        assert!(z.windows(2).all(|w| w[0].command_id <= w[1].command_id));
        assert_eq!(z[0].command_id, 500);
        assert_eq!(z[1].command_id, 500);
        assert_eq!(z[2].command_id, 500);

        // Finishing the tampered active phase assigns exit code 77 only to the active zone (501).
        restored.advance(b"\r\n\x1b]133;D;77\x07");
        let z_after_d = restored.shell_zones();
        assert_eq!(z_after_d.len(), 4);
        assert_eq!(z_after_d[0].exit_code, Some(0));
        assert_eq!(z_after_d[3].command_id, 501);
        assert_eq!(z_after_d[3].exit_code, Some(77));

        // Subsequent command gets strictly higher command_id (502) and isolated exit code (99).
        restored.advance(
            b"\x1b]133;A\x07$ \x1b]133;B\x07pwd\r\n\x1b]133;C\x07/tmp\r\n\x1b]133;D;99\x07",
        );
        let z_final = restored.shell_zones();
        assert_eq!(z_final.len(), 7);
        assert!(
            z_final[4..7]
                .iter()
                .all(|zone| zone.command_id == 502 && zone.exit_code == Some(99))
        );
        assert_eq!(z_final[3].exit_code, Some(77));
    }

    #[test]
    fn start_pending_wrap_off_cursor_reflow_positions() {
        // Widen 10 -> 20 after OSC 133;A at pending_wrap + cursor move away:
        // writing at col 10 starts the prompt at col 10 (after '0'), not col 9.
        let mut t = Terminal::new(10, 4, CoreConfig::default());
        t.advance(b"1234567890\x1b]133;A\x07\x1b[1;1H");
        t.resize(20, 4);
        t.advance(b"\x1b[1;10HXYZ\x1b]133;B\x07");
        let zones = t.shell_zones();
        assert_eq!(zones.len(), 1);
        assert_eq!((zones[0].start_line, zones[0].start_col), (0, 10));
        assert_eq!((zones[0].end_line, zones[0].end_col), (0, 12));

        // Shrink 10 -> 5 (even wrap keeps pending_wrap at col 4) and soft-wrapped continuation.
        let mut t_even = Terminal::new(10, 4, CoreConfig::default());
        t_even.advance(b"1234567890\x1b]133;A\x07\x1b[1;1H");
        t_even.resize(5, 4);
        t_even.advance(b"\x1b[2;5HZ\x1b]133;B\x07");
        // On 5 cols, row 1 has "67890", writing at (1, 4) overwrites '0' and moves cursor to
        // pending_wrap after col 4, while start had pending_wrap at (1, 4) -> close_zone starts at (1, 5).
        assert_eq!(t_even.shell_zones().len(), 1);
        assert_eq!(t_even.shell_zones()[0].start_col, 5);

        let mut t_wrap = Terminal::new(10, 4, CoreConfig::default());
        t_wrap.advance(b"1234567890\x1b]133;A\x07AB\x1b[1;1H");
        t_wrap.resize(20, 4);
        t_wrap.advance(b"\x1b[1;13H\x1b]133;B\x07");
        assert_eq!(t_wrap.shell_zones().len(), 1);
        assert_eq!(
            (
                t_wrap.shell_zones()[0].start_col,
                t_wrap.shell_zones()[0].end_col
            ),
            (10, 12)
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn start_pending_wrap_off_cursor_resize_snapshot_and_close_zone_parity() {
        struct ParityCase<'a> {
            name: &'a str,
            c0: u16,
            r0: u16,
            init: &'a [u8],
            c1: u16,
            r1: u16,
            follow: &'a [u8],
        }
        let cases = [
            ParityCase {
                name: "challenger_bug1_widen_10_to_20",
                c0: 10,
                r0: 4,
                init: b"1234567890\x1b]133;A\x07\x1b[1;1H",
                c1: 20,
                r1: 4,
                follow: b"\x1b[1;10HXYZ\x1b]133;B\x07",
            },
            ParityCase {
                name: "widen_with_next_line_start_at_row1_col0",
                c0: 10,
                r0: 4,
                init: b"1234567890\x1b]133;A\x07\r\n",
                c1: 20,
                r1: 4,
                follow: b"cmd\x1b]133;B\x07",
            },
            ParityCase {
                name: "shrink_uneven_10_to_7",
                c0: 10,
                r0: 4,
                init: b"1234567890\x1b]133;A\x07\x1b[1;1H",
                c1: 7,
                r1: 4,
                follow: b"\x1b[2;4HXY\x1b]133;B\x07",
            },
            ParityCase {
                name: "shrink_even_10_to_5_keeps_pending_wrap",
                c0: 10,
                r0: 4,
                init: b"1234567890\x1b]133;A\x07\x1b[1;1H",
                c1: 5,
                r1: 4,
                follow: b"\x1b[3;1Hout\x1b]133;B\x07",
            },
            ParityCase {
                name: "soft_wrapped_continuation_widen_10_to_20",
                c0: 10,
                r0: 4,
                init: b"1234567890\x1b]133;A\x07AB\x1b[1;1H",
                c1: 20,
                r1: 4,
                follow: b"\x1b[1;13H\x1b]133;B\x07",
            },
            ParityCase {
                name: "wide_char_at_margin_widen_10_to_20",
                c0: 10,
                r0: 4,
                init: "12345678中\x1b]133;A\x07\x1b[1;1H".as_bytes(),
                c1: 20,
                r1: 4,
                follow: b"\x1b[1;11HXY\x1b]133;B\x07",
            },
            ParityCase {
                name: "alt_screen_resize_while_primary_start_pending_wrap",
                c0: 10,
                r0: 4,
                init: b"1234567890\x1b]133;A\x07\x1b[1;1H\x1b[?1049h",
                c1: 20,
                r1: 4,
                follow: b"\x1b[?1049l\x1b[1;10HXYZ\x1b]133;B\x07",
            },
        ];

        let cfg = CoreConfig::default();
        for case in cases {
            let mut live = Terminal::new(case.c0, case.r0, cfg.clone());
            live.advance(case.init);
            live.resize(case.c1, case.r1);
            let snap = live.export_snapshot();
            let mut rest = Terminal::from_snapshot(snap.clone(), cfg.clone());
            assert_eq!(
                rest.export_snapshot(),
                snap,
                "snapshot roundtrip mismatch for {}",
                case.name
            );
            live.advance(case.follow);
            rest.advance(case.follow);
            assert_zone_invariants(&live);
            assert_zone_invariants(&rest);
            assert_eq!(
                live.shell_zones(),
                rest.shell_zones(),
                "post-follow shell_zones mismatch for {}",
                case.name
            );
        }
    }

    #[test]
    fn end_is_eol_chunk_parity_minimal_reproducers_input_and_output() {
        // Challenger R8-2 minimal Input zone reproducer: whole vs split after EL 2 (\x1b[2K) & 1-byte chunks.
        let cfg = CoreConfig::default();
        let setup_in = b"\x1b]133;A\x07$ \x1b]133;B\x07hello\r\n\x1b]133;C\x07";
        let tail_in = b"\x1b[1;1H\x1b[2K\x1b[1;5HXYZ";

        let mut whole_in = Terminal::new(20, 4, cfg.clone());
        whole_in.advance(setup_in);
        whole_in.advance(tail_in);

        let mut split_in = Terminal::new(20, 4, cfg.clone());
        split_in.advance(setup_in);
        split_in.advance(b"\x1b[1;1H\x1b[2K");
        split_in.advance(b"\x1b[1;5HXYZ");

        let mut byte_in = Terminal::new(20, 4, cfg.clone());
        for &b in setup_in.iter().chain(tail_in.iter()) {
            byte_in.advance(&[b]);
        }

        assert_zone_invariants(&whole_in);
        assert_eq!(whole_in.shell_zones(), split_in.shell_zones());
        assert_eq!(whole_in.shell_zones(), byte_in.shell_zones());
        let input_z = whole_in
            .shell_zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Input)
            .unwrap();
        assert_eq!(
            (input_z.start_col, input_z.end_col, input_z.end_is_eol),
            (2, 2, false)
        );

        // Challenger R8-2 minimal Output zone reproducer: whole vs split after EL 2 (\x1b[2K) & 1-byte chunks.
        let setup_out =
            b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07old\r\n\x1b]133;D;0\x07";
        let tail_out = b"\x1b[2;1H\x1b[2KNEW";

        let mut whole_out = Terminal::new(20, 4, cfg.clone());
        whole_out.advance(setup_out);
        whole_out.advance(tail_out);

        let mut split_out = Terminal::new(20, 4, cfg.clone());
        split_out.advance(setup_out);
        split_out.advance(b"\x1b[2;1H\x1b[2K");
        split_out.advance(b"NEW");

        let mut byte_out = Terminal::new(20, 4, cfg);
        for &b in setup_out.iter().chain(tail_out.iter()) {
            byte_out.advance(&[b]);
        }

        assert_zone_invariants(&whole_out);
        assert_eq!(whole_out.shell_zones(), split_out.shell_zones());
        assert_eq!(whole_out.shell_zones(), byte_out.shell_zones());
        assert_eq!(whole_out.command_output_at(1).as_deref(), Some(""));
        assert_eq!(split_out.command_output_at(1).as_deref(), Some(""));
        assert_eq!(byte_out.command_output_at(1).as_deref(), Some(""));
    }

    #[test]
    fn end_is_eol_chunk_parity_erase_shift_space_and_seeds() {
        struct ParitySeq<'a> {
            name: &'a str,
            cols: u16,
            rows: u16,
            scrollback: usize,
            stream: &'a [u8],
        }
        let cases = [
            ParitySeq {
                name: "seed126_csi_1k_after_osc133_c",
                cols: 20,
                rows: 4,
                scrollback: 16,
                stream: b"\r\n\x1b]133;A\x07$\x1b]133;B\x07cmd\r\n\x1b]133;C\x07\x1b[2;9H\x1b[1K\x1b]133;A\x07$ ",
            },
            ParitySeq {
                name: "seed179_csi_1j_decrc_and_csi_0k",
                cols: 20,
                rows: 5,
                scrollback: 16,
                stream: b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07\x1b[2;2H\x1b7\x1b]133;A\x07$ \x1b[1J\x1b8\x1b]133;A\x07$ \x1b[s\x1b[u\x1b[4;4H\x1b[0K\x1b[?1049l",
            },
            ParitySeq {
                name: "ech_erase_and_redraw",
                cols: 20,
                rows: 4,
                scrollback: 16,
                stream: b"\x1b]133;A\x07$ \x1b]133;B\x07abcde\r\n\x1b]133;C\x07\x1b[1;3H\x1b[5X\x1b[1;5HXY",
            },
            ParitySeq {
                name: "dch_delete_and_redraw",
                cols: 20,
                rows: 4,
                scrollback: 16,
                stream: b"\x1b]133;A\x07$ \x1b]133;B\x07abc\r\n\x1b]133;C\x07\x1b[1;3H\x1b[3P\x1b[1;5HXY",
            },
            ParitySeq {
                name: "dl_and_il_shift_and_redraw",
                cols: 20,
                rows: 4,
                scrollback: 16,
                stream: b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07\x1b[2;1H\x1b[1MNEW\x1b[2;1H\x1b[1L\x1b[2;1HAGAIN",
            },
            ParitySeq {
                name: "decstbm_su_sd_and_redraw",
                cols: 20,
                rows: 5,
                scrollback: 16,
                stream: b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07\x1b[2;3r\x1b[1S\x1b[r\x1b[2;1HNEW",
            },
            ParitySeq {
                name: "space_overwrite_then_write_on_eol_row",
                cols: 20,
                rows: 4,
                scrollback: 16,
                stream: b"\x1b]133;A\x07$ \x1b]133;B\x07ab\r\n\x1b]133;C\x07\x1b[1;3H  X",
            },
            ParitySeq {
                name: "zero_scrollback_multiline_blank_tail_eviction_and_write",
                cols: 20,
                rows: 4,
                scrollback: 0,
                stream: b"\x1b]133;C\x07line0\r\n\r\n\x1b]133;D;0\x07\r\n\r\n\x1b[1;1HNEW",
            },
        ];

        for case in cases {
            let cfg = CoreConfig {
                scrollback_limit: case.scrollback,
                ..CoreConfig::default()
            };
            let mut whole = Terminal::new(case.cols, case.rows, cfg.clone());
            whole.advance(case.stream);
            assert_zone_invariants(&whole);

            for chunk_len in [1usize, 2, 3, 5, 7] {
                let mut chunked = Terminal::new(case.cols, case.rows, cfg.clone());
                for chunk in case.stream.chunks(chunk_len) {
                    chunked.advance(chunk);
                    assert_zone_invariants(&chunked);
                }
                assert_eq!(
                    whole.shell_zones(),
                    chunked.shell_zones(),
                    "chunk_len={chunk_len} zones mismatch for {}",
                    case.name
                );
                assert_eq!(
                    whole.command_output_at(0),
                    chunked.command_output_at(0),
                    "chunk_len={chunk_len} output(0) mismatch for {}",
                    case.name
                );
                assert_eq!(
                    whole.command_output_at(1),
                    chunked.command_output_at(1),
                    "chunk_len={chunk_len} output(1) mismatch for {}",
                    case.name
                );
            }
        }
    }

    #[cfg(feature = "serde")]
    #[test]
    fn end_is_eol_mid_advance_erase_redraw_snapshot_parity() {
        let cfg = CoreConfig::default();
        let stream = b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07old\r\n\x1b]133;D;0\x07\x1b[2;1H\x1b[2KNEW\x1b[1;1H\x1b[2K\x1b[1;5HXYZ";

        let mut whole = Terminal::new(20, 4, cfg.clone());
        whole.advance(stream);
        let snap_whole = whole.export_snapshot();
        let mut restored_whole = Terminal::from_snapshot(snap_whole.clone(), cfg.clone());
        assert_eq!(restored_whole.export_snapshot(), snap_whole);

        let mut chunked = Terminal::new(20, 4, cfg.clone());
        for chunk in stream.chunks(3) {
            chunked.advance(chunk);
        }
        let snap_chunked = chunked.export_snapshot();
        let mut restored_chunked = Terminal::from_snapshot(snap_chunked.clone(), cfg);
        assert_eq!(restored_chunked.export_snapshot(), snap_chunked);

        assert_eq!(whole.shell_zones(), restored_whole.shell_zones());
        assert_eq!(whole.shell_zones(), chunked.shell_zones());
        assert_eq!(whole.shell_zones(), restored_chunked.shell_zones());
        assert_eq!(whole.visible_lines(), chunked.visible_lines());
        assert_eq!(whole.visible_lines(), restored_chunked.visible_lines());
        assert_eq!(whole.cursor_pos(), chunked.cursor_pos());
        assert_eq!(whole.command_output_at(1).as_deref(), Some(""));
        assert_eq!(restored_chunked.command_output_at(1).as_deref(), Some(""));

        for &(rw, rh) in &[(8, 4), (30, 4)] {
            whole.resize(rw, rh);
            chunked.resize(rw, rh);
            restored_whole.resize(rw, rh);
            restored_chunked.resize(rw, rh);
            assert_eq!(whole.shell_zones(), chunked.shell_zones());
            assert_eq!(whole.shell_zones(), restored_whole.shell_zones());
            assert_eq!(whole.shell_zones(), restored_chunked.shell_zones());
            assert_eq!(
                whole.command_output_at(1),
                restored_chunked.command_output_at(1)
            );
        }
    }

    #[test]
    fn vs16_promote_cell_to_wide_chunk_parity() {
        let cfg = CoreConfig::default();

        // Reviewer R9-2 reproducer: promote_cell_to_wide clears adjacent WIDE_SPACER at col+2.
        let setup_a = "a\u{4e2d}\x1b[1;3H\x1b]133;C\x07\r\n\x1b]133;D;0\x07".as_bytes();
        let part_a1 = "\x1b[1;1H\u{2764}\u{fe0f}".as_bytes();
        let part_a2 = b"Z";
        let mut tail_a = Vec::new();
        tail_a.extend_from_slice(part_a1);
        tail_a.extend_from_slice(part_a2);

        let mut whole_a = Terminal::new(10, 4, cfg.clone());
        whole_a.advance(setup_a);
        whole_a.advance(&tail_a);

        let mut split_a = Terminal::new(10, 4, cfg.clone());
        split_a.advance(setup_a);
        split_a.advance(part_a1);
        split_a.advance(part_a2);

        let mut byte_a = Terminal::new(10, 4, cfg.clone());
        for &b in setup_a.iter().chain(tail_a.iter()) {
            byte_a.advance(&[b]);
        }

        assert_zone_invariants(&whole_a);
        assert_zone_invariants(&split_a);
        assert_zone_invariants(&byte_a);
        assert_eq!(whole_a.shell_zones(), split_a.shell_zones());
        assert_eq!(whole_a.shell_zones(), byte_a.shell_zones());
        assert_eq!(whole_a.command_output_at(0).as_deref(), Some(""));
        assert_eq!(split_a.command_output_at(0).as_deref(), Some(""));
        assert_eq!(byte_a.command_output_at(0).as_deref(), Some(""));

        // Challenger R9-1 reproducer: VS16 promotion followed by CUP + ASCII write on the same row.
        let setup_b = "A\u{4E2D}\x08\x1b]133;C\x07\r\n\x1b]133;D;0\x07".as_bytes();
        let part_b1 = "\x1b[1A\r\u{2764}\u{FE0F}".as_bytes();
        let part_b2 = b"\x1b[1;4HXYZ";
        let mut tail_b = Vec::new();
        tail_b.extend_from_slice(part_b1);
        tail_b.extend_from_slice(part_b2);

        let mut whole_b = Terminal::new(10, 4, cfg.clone());
        whole_b.advance(setup_b);
        whole_b.advance(&tail_b);

        let mut split_b = Terminal::new(10, 4, cfg.clone());
        split_b.advance(setup_b);
        split_b.advance(part_b1);
        split_b.advance(part_b2);

        let mut byte_b = Terminal::new(10, 4, cfg);
        for &b in setup_b.iter().chain(tail_b.iter()) {
            byte_b.advance(&[b]);
        }

        assert_zone_invariants(&whole_b);
        assert_zone_invariants(&split_b);
        assert_zone_invariants(&byte_b);
        assert_eq!(whole_b.shell_zones(), split_b.shell_zones());
        assert_eq!(whole_b.shell_zones(), byte_b.shell_zones());
        assert_eq!(whole_b.command_output_at(0).as_deref(), Some(""));
        assert_eq!(split_b.command_output_at(0).as_deref(), Some(""));
        assert_eq!(byte_b.command_output_at(0).as_deref(), Some(""));
    }
}
