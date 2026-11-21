//! Explicit (`OSC 8`) and implicit (`http://`, `https://`, `ftp://`) hyperlink tracking
//! (`hyperlinks` feature).
//!
//! # Examples
//!
//! ```
//! use egui_xterm_core::{CoreConfig, HyperlinkSpan, Terminal, TerminalEvent};
//!
//! let mut term = Terminal::new(40, 4, CoreConfig::default());
//! term.advance(b"\x1b]8;id=doc;https://docs.rs\x1b\\docs\x1b]8;;\x1b\\\r\nSee https://example.org/guide.");
//!
//! let (info, spans) = term
//!     .hyperlink_visible_spans_at(0, 0)
//!     .ok_or("expected OSC 8 hyperlink at (0, 0)")?;
//! assert_eq!(info.id.as_deref(), Some("doc"));
//! assert_eq!(info.uri, "https://docs.rs");
//! assert_eq!(
//!     spans,
//!     vec![HyperlinkSpan {
//!         row: 0,
//!         start_col: 0,
//!         end_col: 4,
//!     }]
//! );
//!
//! assert!(term.activate_hyperlink_at(0, 0));
//! assert!(term.drain_events().contains(&TerminalEvent::HyperlinkActivated {
//!     uri: "https://docs.rs".to_owned(),
//!     id: Some("doc".to_owned()),
//! }));
//!
//! assert_eq!(
//!     term.hyperlink_at(6, 1).map(|link| link.uri.as_str()),
//!     Some("https://example.org/guide")
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::collections::VecDeque;

use crate::cell::{HyperlinkId, HyperlinkPool, MAX_HYPERLINK_URI_BYTES};
use crate::features::{CellSpan, extract_logical_line_with_spans};
use crate::grid::{AbsLine, Grid, Row};

/// Hyperlink metadata returned by [`Terminal::hyperlink_at`](crate::Terminal::hyperlink_at)
/// and [`Terminal::hyperlink_visible_spans_at`](crate::Terminal::hyperlink_visible_spans_at).
pub type HyperlinkInfo = crate::cell::Hyperlink;

/// Single visible-viewport horizontal span belonging to an active hyperlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HyperlinkSpan {
    /// Zero-based visible viewport row (`0..rows`).
    pub row: u16,
    /// Inclusive 0-based start column.
    pub start_col: u16,
    /// Exclusive 0-based end column.
    pub end_col: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImplicitLink {
    info: HyperlinkInfo,
    start_line: AbsLine,
    start_col: u16,
    end_line: AbsLine,
    end_col: u16,
}

impl ImplicitLink {
    fn contains(&self, line: AbsLine, col: u16) -> bool {
        if line < self.start_line || line > self.end_line {
            return false;
        }
        if line == self.start_line && col < self.start_col {
            return false;
        }
        if line == self.end_line && col >= self.end_col {
            return false;
        }
        true
    }
}

/// Per-screen incremental store of implicitly detected `http://`, `https://`, and `ftp://` links.
#[derive(Debug, Clone, Default)]
pub(crate) struct ImplicitLinkStore {
    links: VecDeque<ImplicitLink>,
    last_content_epoch: u64,
    last_damage_epoch: u64,
    last_cols: usize,
    last_oldest_line: AbsLine,
    text_buf: String,
    spans: Vec<CellSpan>,
}

impl ImplicitLinkStore {
    pub(crate) fn clear(&mut self) {
        self.links.clear();
        self.text_buf.clear();
        self.spans.clear();
        self.last_content_epoch = 0;
        self.last_damage_epoch = 0;
        self.last_cols = 0;
        self.last_oldest_line = AbsLine(0);
    }

    /// Incrementally updates implicit hyperlinks for grid mutations starting at `dirty_from`.
    pub(crate) fn refresh_dirty(&mut self, grid: &Grid, dirty_from: Option<AbsLine>) {
        let content_epoch = grid.content_epoch();
        let damage_epoch = grid.damage_epoch();
        let cols = grid.cols();

        if content_epoch == self.last_content_epoch && cols == self.last_cols {
            return;
        }

        let oldest = grid.oldest_abs_line();
        let Some(end_buf_idx) = grid.last_non_empty_buf_idx() else {
            self.links.clear();
            self.last_content_epoch = content_epoch;
            self.last_damage_epoch = damage_epoch;
            self.last_cols = cols;
            self.last_oldest_line = oldest;
            return;
        };

        let force_rescan = self.last_cols != cols
            || self.last_content_epoch == 0
            || oldest < self.last_oldest_line;

        if force_rescan {
            self.links.clear();
            scan_buf_range(
                grid,
                0,
                end_buf_idx,
                &mut self.text_buf,
                &mut self.spans,
                &mut self.links,
            );
        } else {
            let head_evicted = oldest > self.last_oldest_line;
            if head_evicted {
                while self.links.front().is_some_and(|l| l.end_line < oldest) {
                    self.links.pop_front();
                }
                while self.links.front().is_some_and(|l| l.start_line < oldest) {
                    self.links.pop_front();
                }
            }

            let max_abs_non_empty = grid.abs_line_of_buf(end_buf_idx);
            while self
                .links
                .back()
                .is_some_and(|l| l.end_line > max_abs_non_empty)
            {
                self.links.pop_back();
            }

            let scan_start_buf = if let Some(d) = dirty_from {
                let raw = if d <= oldest {
                    0
                } else if let Some(idx) = grid.buf_idx_of_abs(d) {
                    idx
                } else {
                    end_buf_idx + 1
                };

                if raw > end_buf_idx {
                    while self.links.back().is_some_and(|l| l.end_line >= d) {
                        self.links.pop_back();
                    }
                    if grid.row_by_buf(end_buf_idx).is_some_and(Row::soft_wrapped) {
                        let start = logical_line_start(grid, end_buf_idx);
                        let abs_start = grid.abs_line_of_buf(start);
                        while self.links.back().is_some_and(|l| l.end_line >= abs_start) {
                            self.links.pop_back();
                        }
                        start
                    } else {
                        end_buf_idx + 1
                    }
                } else {
                    let start = logical_line_start(grid, raw);
                    let abs_start = grid.abs_line_of_buf(start);
                    while self.links.back().is_some_and(|l| l.end_line >= abs_start) {
                        self.links.pop_back();
                    }
                    start
                }
            } else {
                end_buf_idx + 1
            };

            if head_evicted && scan_start_buf > 0 {
                let mut phys_end_0 = 0usize;
                while phys_end_0 < end_buf_idx
                    && grid.row_by_buf(phys_end_0).is_some_and(Row::soft_wrapped)
                {
                    phys_end_0 += 1;
                }
                let abs_end_0 = grid.abs_line_of_buf(phys_end_0);
                while self
                    .links
                    .front()
                    .is_some_and(|l| l.start_line <= abs_end_0)
                {
                    self.links.pop_front();
                }
                if logical_line_has_scheme_sep(grid, 0, phys_end_0) {
                    extract_logical_line_with_spans(
                        grid,
                        0,
                        phys_end_0,
                        &mut self.text_buf,
                        &mut self.spans,
                    );
                    let mut front_links = VecDeque::new();
                    scan_implicit_urls(grid, &self.text_buf, &self.spans, &mut front_links);
                    for link in front_links.into_iter().rev() {
                        self.links.push_front(link);
                    }
                }
            }

            if scan_start_buf <= end_buf_idx {
                scan_buf_range(
                    grid,
                    scan_start_buf,
                    end_buf_idx,
                    &mut self.text_buf,
                    &mut self.spans,
                    &mut self.links,
                );
            }
        }

        self.last_content_epoch = content_epoch;
        self.last_damage_epoch = damage_epoch;
        self.last_cols = cols;
        self.last_oldest_line = oldest;
    }

    pub(crate) fn find_at(&self, line: AbsLine, col: u16) -> Option<&HyperlinkInfo> {
        self.find_link(line, col).map(|link| &link.info)
    }

    fn find_link(&self, line: AbsLine, col: u16) -> Option<&ImplicitLink> {
        let (a, b) = self.links.as_slices();
        let slice = if let Some(b_first) = b.first()
            && (line, col) >= (b_first.start_line, b_first.start_col)
        {
            b
        } else {
            a
        };
        let idx = slice.partition_point(|l| l.end_line < line);
        slice[idx..]
            .iter()
            .take_while(|l| l.start_line <= line)
            .find(|l| l.contains(line, col))
    }

    pub(crate) fn visible_spans_at(
        &self,
        grid: &Grid,
        line: AbsLine,
        col: u16,
    ) -> Option<(&HyperlinkInfo, Vec<HyperlinkSpan>)> {
        let link = self.find_link(line, col)?;
        let mut out = Vec::new();
        let cols_u16 = u16::try_from(grid.cols()).unwrap_or(u16::MAX);
        for vp_row in 0..grid.viewport_rows() {
            let row_abs = grid.screen_to_abs(vp_row);
            if row_abs < link.start_line || row_abs > link.end_line {
                continue;
            }
            let start_col = if row_abs == link.start_line {
                link.start_col
            } else {
                0
            };
            let end_col = if row_abs == link.end_line {
                link.end_col
            } else {
                let row_trimmed = grid.visible_row(vp_row).map_or(cols_u16, |r| {
                    let mut len = r.trimmed_len();
                    if len > 0 && r.cells()[len - 1].is_wrap_spacer() {
                        len -= 1;
                    }
                    u16::try_from(len).unwrap_or(cols_u16)
                });
                row_trimmed.min(cols_u16)
            };
            if start_col < end_col {
                out.push(HyperlinkSpan {
                    row: u16::try_from(vp_row).unwrap_or(u16::MAX),
                    start_col,
                    end_col,
                });
            }
        }
        Some((&link.info, out))
    }
}

fn logical_line_start(grid: &Grid, mut buf_idx: usize) -> usize {
    while buf_idx > 0 && grid.row_by_buf(buf_idx - 1).is_some_and(Row::soft_wrapped) {
        buf_idx -= 1;
    }
    buf_idx
}

fn logical_line_has_scheme_sep(grid: &Grid, phys_start: usize, phys_end: usize) -> bool {
    let mut stage = 0u8;
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
        for &cell in &cells[..take_end] {
            if cell.is_wide_spacer() || cell.is_wrap_spacer() || cell.is_sized_spacer() {
                continue;
            }
            stage = match (stage, cell.codepoint) {
                (_, ':') => 1,
                (1, '/') => 2,
                (2, '/') => return true,
                _ => 0,
            };
        }
    }
    false
}

fn scan_buf_range(
    grid: &Grid,
    start_buf: usize,
    end_buf_idx: usize,
    text_buf: &mut String,
    spans: &mut Vec<CellSpan>,
    out: &mut VecDeque<ImplicitLink>,
) {
    let mut phys_start = start_buf;
    while phys_start <= end_buf_idx {
        let mut phys_end = phys_start;
        while phys_end < end_buf_idx && grid.row_by_buf(phys_end).is_some_and(Row::soft_wrapped) {
            phys_end += 1;
        }

        if logical_line_has_scheme_sep(grid, phys_start, phys_end) {
            extract_logical_line_with_spans(grid, phys_start, phys_end, text_buf, spans);
            scan_implicit_urls(grid, text_buf, spans, out);
        }

        phys_start = phys_end + 1;
    }
}

fn cell_has_explicit_link(grid: &Grid, span: &CellSpan) -> bool {
    let Some(row) = grid.row_by_buf(span.buf_row) else {
        return false;
    };
    let Some(cell) = row.cells().get(usize::from(span.start_col)) else {
        return false;
    };
    cell.extra_id != 0
        && grid
            .cell_extra(cell.extra_id)
            .is_some_and(|e| e.hyperlink_id.is_some())
}

fn scan_implicit_urls(
    grid: &Grid,
    text: &str,
    spans: &[CellSpan],
    out: &mut VecDeque<ImplicitLink>,
) {
    const SCHEMES: [&str; 3] = ["https://", "http://", "ftp://"];
    let bytes = text.as_bytes();
    let mut cursor = 0usize;

    while cursor < text.len() {
        let mut next_match: Option<(usize, &str)> = None;
        for scheme in SCHEMES {
            if let Some(rel) = memchr::memmem::find(&bytes[cursor..], scheme.as_bytes()) {
                let abs_pos = cursor + rel;
                if next_match.is_none_or(|(best, _)| abs_pos < best) {
                    next_match = Some((abs_pos, scheme));
                }
            }
        }
        let Some((url_start, scheme)) = next_match else {
            break;
        };

        // Ensure scheme is not glued directly onto a preceding identifier character.
        if url_start > 0
            && let Some(prev_ch) = text[..url_start].chars().next_back()
            && prev_ch.is_ascii_alphanumeric()
        {
            cursor = url_start + scheme.len();
            continue;
        }

        let body_start = url_start + scheme.len();
        let mut url_end = url_start;
        for (offset, ch) in text[url_start..].char_indices() {
            if ch.is_whitespace()
                || ch.is_control()
                || matches!(
                    ch,
                    '<' | '>' | '"' | '\'' | '`' | '{' | '}' | '|' | '^' | '\\'
                )
            {
                break;
            }
            let char_byte = url_start + offset;
            let span_idx = spans
                .partition_point(|s| s.byte_start <= char_byte)
                .saturating_sub(1);
            if spans
                .get(span_idx)
                .is_some_and(|s| cell_has_explicit_link(grid, s))
            {
                break;
            }
            url_end = char_byte + ch.len_utf8();
        }

        // Trim trailing sentence punctuation and unmatched closing brackets.
        while url_end > body_start {
            let slice = &text[url_start..url_end];
            let Some(last_ch) = slice.chars().next_back() else {
                break;
            };
            let strip = match last_ch {
                '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '`' | '*' => true,
                ')' => {
                    slice.bytes().filter(|&b| b == b')').count()
                        > slice.bytes().filter(|&b| b == b'(').count()
                }
                ']' => {
                    slice.bytes().filter(|&b| b == b']').count()
                        > slice.bytes().filter(|&b| b == b'[').count()
                }
                _ => false,
            };
            if strip {
                url_end -= last_ch.len_utf8();
            } else {
                break;
            }
        }

        if url_end > body_start && url_end - url_start <= MAX_HYPERLINK_URI_BYTES {
            let url_str = &text[url_start..url_end];
            let after_scheme = &url_str[scheme.len()..];
            if !after_scheme.is_empty() && !after_scheme.starts_with(['/', '?', '#', ':', '.']) {
                let first_idx = spans.partition_point(|s| s.byte_end <= url_start);
                let last_idx = spans
                    .partition_point(|s| s.byte_start < url_end)
                    .saturating_sub(1);
                if let (Some(first), Some(last)) = (spans.get(first_idx), spans.get(last_idx)) {
                    out.push_back(ImplicitLink {
                        info: HyperlinkInfo {
                            id: None,
                            uri: url_str.to_owned(),
                        },
                        start_line: grid.abs_line_of_buf(first.buf_row),
                        start_col: first.start_col,
                        end_line: grid.abs_line_of_buf(last.buf_row),
                        end_col: last.end_col,
                    });
                }
            }
        }

        cursor = url_end.max(url_start + scheme.len());
    }
}

/// Parses an `OSC 8` parameter list (`id=...` separated by `:`) and interns the URI.
pub(crate) fn parse_osc8_hyperlink(
    params_part: &[u8],
    uri: &str,
    pool: &mut HyperlinkPool,
) -> Option<HyperlinkId> {
    let mut id: Option<&str> = None;
    if let Ok(params_str) = std::str::from_utf8(params_part) {
        for kv in params_str.split(':') {
            if let Some(val) = kv.strip_prefix("id=")
                && !val.is_empty()
            {
                id = Some(val);
            }
        }
    }
    pool.try_intern(id, uri)
}

/// Collects visible viewport spans for an explicit `OSC 8` [`HyperlinkId`].
pub(crate) fn explicit_visible_spans(
    grid: &Grid,
    target_hid: HyperlinkId,
    group_by_id: bool,
    target_row: u16,
    target_col: u16,
) -> Vec<HyperlinkSpan> {
    let mut spans = Vec::new();
    let vrows = grid.viewport_rows();
    let cols = grid.cols();

    let cell_has_target = |r: usize, c: usize| -> bool {
        let Some(row) = grid.visible_row(r) else {
            return false;
        };
        let Some(cell) = row.cells().get(c) else {
            return false;
        };
        if cell.is_wrap_spacer() {
            return false;
        }
        if cell.is_wide_spacer() {
            return c > 0
                && row.cells()[c - 1].extra_id != 0
                && grid
                    .cell_extra(row.cells()[c - 1].extra_id)
                    .is_some_and(|e| e.hyperlink_id == Some(target_hid));
        }
        cell.extra_id != 0
            && grid
                .cell_extra(cell.extra_id)
                .is_some_and(|e| e.hyperlink_id == Some(target_hid))
    };

    if group_by_id {
        for r in 0..vrows {
            let mut c = 0usize;
            while c < cols {
                if cell_has_target(r, c) {
                    let start = c;
                    while c < cols && cell_has_target(r, c) {
                        c += 1;
                    }
                    spans.push(HyperlinkSpan {
                        row: u16::try_from(r).unwrap_or(u16::MAX),
                        start_col: u16::try_from(start).unwrap_or(u16::MAX),
                        end_col: u16::try_from(c).unwrap_or(u16::MAX),
                    });
                } else {
                    c += 1;
                }
            }
        }
        return spans;
    }

    // Without an `id=`, find the contiguous run connected to (target_row, target_col),
    // including across soft-wrapped row boundaries.
    let tr = usize::from(target_row);
    let tc = usize::from(target_col);
    if tr >= vrows || tc >= cols || !cell_has_target(tr, tc) {
        return spans;
    }

    let mut cur_start = tc;
    while cur_start > 0 && cell_has_target(tr, cur_start - 1) {
        cur_start -= 1;
    }
    let mut cur_end = tc + 1;
    while cur_end < cols && cell_has_target(tr, cur_end) {
        cur_end += 1;
    }

    let mut collected = vec![(tr, cur_start, cur_end)];

    // Walk backward across soft-wrapped rows if run touches column 0.
    let mut b_row = tr;
    let mut b_start = cur_start;
    while b_start == 0 && b_row > 0 {
        let prev_r = b_row - 1;
        let Some(prev) = grid.visible_row(prev_r) else {
            break;
        };
        if !prev.soft_wrapped() {
            break;
        }
        let mut last_c = cols;
        if last_c > 0 && prev.cells()[last_c - 1].is_wrap_spacer() {
            last_c -= 1;
        }
        if last_c == 0 || !cell_has_target(prev_r, last_c - 1) {
            break;
        }
        let mut s = last_c - 1;
        while s > 0 && cell_has_target(prev_r, s - 1) {
            s -= 1;
        }
        collected.push((prev_r, s, last_c));
        b_row = prev_r;
        b_start = s;
    }

    // Walk forward across soft-wrapped rows if run reaches end of row.
    let mut f_row = tr;
    let mut f_end = cur_end;
    while f_row + 1 < vrows {
        let Some(cur) = grid.visible_row(f_row) else {
            break;
        };
        if !cur.soft_wrapped() {
            break;
        }
        let row_limit = if cols > 0 && cur.cells()[cols - 1].is_wrap_spacer() {
            cols - 1
        } else {
            cols
        };
        if f_end < row_limit {
            break;
        }
        let next_r = f_row + 1;
        if !cell_has_target(next_r, 0) {
            break;
        }
        let mut e = 1usize;
        while e < cols && cell_has_target(next_r, e) {
            e += 1;
        }
        collected.push((next_r, 0, e));
        f_row = next_r;
        f_end = e;
    }

    collected.sort_unstable_by_key(|&(r, s, _)| (r, s));
    for (r, s, e) in collected {
        spans.push(HyperlinkSpan {
            row: u16::try_from(r).unwrap_or(u16::MAX),
            start_col: u16::try_from(s).unwrap_or(u16::MAX),
            end_col: u16::try_from(e).unwrap_or(u16::MAX),
        });
    }
    spans
}

#[cfg(test)]
mod tests {
    use crate::term::{CoreConfig, Terminal, TerminalEvent};

    #[test]
    fn explicit_osc8_grouping_wide_resolution_and_activation() {
        let mut term = Terminal::new(10, 4, CoreConfig::default());
        term.advance(
            "\x1b]8;id=g1;https://example.org/p\x1b\\中ClickHereLong\x1b]8;;\x1b\\".as_bytes(),
        );

        // Clicking either half of wide '中' resolves the link.
        assert_eq!(
            term.hyperlink_at(0, 0).map(|h| h.uri.as_str()),
            Some("https://example.org/p")
        );
        assert_eq!(
            term.hyperlink_at(1, 0).map(|h| h.uri.as_str()),
            Some("https://example.org/p")
        );
        // Wrapped continuation on row 1.
        assert_eq!(
            term.hyperlink_at(2, 1).map(|h| h.uri.as_str()),
            Some("https://example.org/p")
        );
        assert!(term.hyperlink_at(6, 1).is_none());

        let (info, spans) = term.hyperlink_visible_spans_at(1, 0).unwrap();
        assert_eq!(info.id.as_deref(), Some("g1"));
        assert_eq!(spans.len(), 2);
        assert_eq!(
            (spans[0].row, spans[0].start_col, spans[0].end_col),
            (0, 0, 10)
        );
        assert_eq!(
            (spans[1].row, spans[1].start_col, spans[1].end_col),
            (1, 0, 5)
        );

        assert!(term.activate_hyperlink_at(2, 1));
        assert!(!term.activate_hyperlink_at(7, 1));
        let events = term.drain_events();
        assert!(events.contains(&TerminalEvent::HyperlinkActivated {
            uri: "https://example.org/p".to_owned(),
            id: Some("g1".to_owned()),
        }));
    }

    #[test]
    fn implicit_url_balancing_soft_wrap_reflow_and_pool_gc() {
        let cfg = CoreConfig {
            scrollback_limit: 5,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(20, 4, cfg);
        term.advance(
            b"See (https://en.wikipedia.org/wiki/Rust_(lang)). ftp://files.test/a.tar.gz!",
        );

        // Balanced inner parens kept, outer ')' and '.' stripped.
        assert_eq!(
            term.hyperlink_at(6, 0).map(|h| h.uri.as_str()),
            Some("https://en.wikipedia.org/wiki/Rust_(lang)")
        );
        // Wrapped continuation onto row 1.
        assert_eq!(
            term.hyperlink_at(2, 2).map(|h| h.uri.as_str()),
            Some("https://en.wikipedia.org/wiki/Rust_(lang)")
        );
        // Second implicit URL with trailing '!' stripped.
        let (_, ftp_spans) = term.hyperlink_visible_spans_at(10, 2).unwrap();
        assert!(!ftp_spans.is_empty());
        assert_eq!(
            term.hyperlink_at(10, 2).map(|h| h.uri.as_str()),
            Some("ftp://files.test/a.tar.gz")
        );

        // Reflow wider keeps implicit links accurate.
        term.resize(80, 4);
        assert_eq!(
            term.hyperlink_at(10, 0).map(|h| h.uri.as_str()),
            Some("https://en.wikipedia.org/wiki/Rust_(lang)")
        );

        // Pool GC bounds memory after churn of 1000 unique evicted links.
        for i in 0..1000 {
            term.advance(format!("\x1b]8;;https://churn.test/{i}\x07L\x1b]8;;\x07\r\n").as_bytes());
        }
        assert!(
            term.hyperlink_pool().len() <= 320,
            "pool len should stay bounded, got {}",
            term.hyperlink_pool().len()
        );
    }

    #[test]
    fn implicit_links_survive_row_shifting_scrolls_and_erased_wrapped_tails() {
        // CSI 1 T (scroll down) across separate advance calls.
        let mut term = Terminal::new(40, 4, CoreConfig::default());
        term.advance(b"\r\nhttps://example.com");
        term.advance(b"\x1b[1T");
        assert!(term.hyperlink_at(0, 1).is_none());
        assert_eq!(
            term.hyperlink_at(0, 2).map(|h| h.uri.as_str()),
            Some("https://example.com")
        );

        // CSI 1 L (insert line) across separate advance calls.
        let mut term = Terminal::new(40, 4, CoreConfig::default());
        term.advance(b"https://example.com\x1b[1;1H");
        term.advance(b"\x1b[1L");
        assert!(term.hyperlink_at(0, 0).is_none());
        assert_eq!(
            term.hyperlink_at(0, 1).map(|h| h.uri.as_str()),
            Some("https://example.com")
        );

        // CSI 1 M (delete line) across separate advance calls.
        let mut term = Terminal::new(40, 4, CoreConfig::default());
        term.advance(b"first\r\nhttps://example.com\x1b[1;1H");
        term.advance(b"\x1b[1M");
        assert_eq!(
            term.hyperlink_at(0, 0).map(|h| h.uri.as_str()),
            Some("https://example.com")
        );
        assert!(term.hyperlink_at(0, 1).is_none());

        // DECSTBM + CSI 1 S across separate advance calls.
        let mut term = Terminal::new(40, 4, CoreConfig::default());
        term.advance(b"\x1b[2;4r\x1b[3;1Hhttps://example.com");
        term.advance(b"\x1b[1S");
        assert_eq!(
            term.hyperlink_at(0, 1).map(|h| h.uri.as_str()),
            Some("https://example.com")
        );
        assert!(term.hyperlink_at(0, 2).is_none());

        // Alt-screen scroll across separate advance calls.
        let mut term = Terminal::new(40, 3, CoreConfig::default());
        term.advance(b"\x1b[?1049h");
        term.advance(b"\r\nhttps://alt.example.com\r\n");
        term.advance(b"\r\n");
        assert_eq!(
            term.hyperlink_at(0, 0).map(|h| h.uri.as_str()),
            Some("https://alt.example.com")
        );
        assert!(term.hyperlink_at(0, 1).is_none());

        // EL 2 on soft-wrapped continuation row keeps surviving URL prefix on row 0.
        let mut term = Terminal::new(20, 4, CoreConfig::default());
        term.advance(b"https://example.com/abc");
        term.advance(b"\x1b[2;1H\x1b[2K");
        assert_eq!(
            term.hyperlink_at(0, 0).map(|h| h.uri.as_str()),
            Some("https://example.com/")
        );

        // ED 0 on soft-wrapped continuation row also keeps surviving prefix on row 0.
        let mut term = Terminal::new(20, 4, CoreConfig::default());
        term.advance(b"https://example.com/abc");
        term.advance(b"\x1b[2;1H\x1b[J");
        assert_eq!(
            term.hyperlink_at(0, 0).map(|h| h.uri.as_str()),
            Some("https://example.com/")
        );
    }

    #[test]
    fn implicit_visible_spans_exclude_wrap_spacer() {
        let mut term = Terminal::new(10, 4, CoreConfig::default());
        // "https://a" (9 cols) + wide '中' wraps atomically, placing WRAP_SPACER at (0, 9).
        term.advance("https://a中/ok".as_bytes());
        assert!(term.hyperlink_at(9, 0).is_none());
        let (info, spans) = term.hyperlink_visible_spans_at(0, 0).unwrap();
        assert_eq!(info.uri, "https://a中/ok");
        assert_eq!(spans.len(), 2);
        assert_eq!(
            (spans[0].row, spans[0].start_col, spans[0].end_col),
            (0, 0, 9)
        );
        assert_eq!(
            (spans[1].row, spans[1].start_col, spans[1].end_col),
            (1, 0, 5)
        );
    }

    #[test]
    fn implicit_links_partial_eviction_and_dual_screen_refresh() {
        // 1. Partial eviction via scrollback limit (probe_7 repro).
        let mut t1 = Terminal::new(
            20,
            2,
            CoreConfig {
                scrollback_limit: 1,
                ..CoreConfig::default()
            },
        );
        t1.advance(b"https://example.com/abc\r\nl2\r\nl3");
        t1.scroll_display(1);
        assert!(t1.hyperlink_at(0, 0).is_none());
        assert!(t1.hyperlink_visible_spans_at(0, 0).is_none());

        // 2. Partial eviction via scrollback_limit = 0.
        let mut t2 = Terminal::new(
            20,
            2,
            CoreConfig {
                scrollback_limit: 0,
                ..CoreConfig::default()
            },
        );
        t2.advance(b"https://example.com/abc");
        assert_eq!(
            t2.hyperlink_at(0, 1).map(|h| h.uri.as_str()),
            Some("https://example.com/abc")
        );
        t2.advance(b"\r\nl2");
        assert!(t2.hyperlink_at(0, 0).is_none());

        // 3. Partial eviction via ED 3 (\x1b[3J).
        let mut t3 = Terminal::new(
            20,
            2,
            CoreConfig {
                scrollback_limit: 10,
                ..CoreConfig::default()
            },
        );
        t3.advance(b"https://example.com/abc\r\nl2");
        assert_eq!(
            t3.hyperlink_at(0, 0).map(|h| h.uri.as_str()),
            Some("https://example.com/abc")
        );
        t3.advance(b"\x1b[3J");
        assert!(t3.hyperlink_at(0, 0).is_none());

        // 4. Partial eviction via same-width height-shrinking resize.
        let mut t4 = Terminal::new(
            20,
            4,
            CoreConfig {
                scrollback_limit: 0,
                ..CoreConfig::default()
            },
        );
        t4.advance(b"https://example.com/abc");
        t4.resize(20, 1);
        assert!(t4.hyperlink_at(0, 0).is_none());

        // 5a. Surviving tail row with space-separated second URL.
        let mut t5a = Terminal::new(
            20,
            2,
            CoreConfig {
                scrollback_limit: 1,
                ..CoreConfig::default()
            },
        );
        t5a.advance(b"https://alpha.test/1234 http://beta.test\r\nl2\r\nl3");
        t5a.scroll_display(1);
        assert!(t5a.hyperlink_at(0, 0).is_none());
        assert_eq!(
            t5a.hyperlink_at(4, 0).map(|h| h.uri.as_str()),
            Some("http://beta.test")
        );
        let (_, spans_5a) = t5a.hyperlink_visible_spans_at(4, 0).unwrap();
        assert_eq!(spans_5a.len(), 1);
        assert_eq!(
            (spans_5a[0].row, spans_5a[0].start_col, spans_5a[0].end_col),
            (0, 4, 20)
        );

        // 5b. Surviving tail row with glued second URL.
        let mut t5b = Terminal::new(
            20,
            2,
            CoreConfig {
                scrollback_limit: 1,
                ..CoreConfig::default()
            },
        );
        t5b.advance(b"https://alpha.test/1https://beta.test/xy\r\nl2\r\nl3");
        t5b.scroll_display(1);
        assert_eq!(
            t5b.hyperlink_at(0, 0).map(|h| h.uri.as_str()),
            Some("https://beta.test/xy")
        );

        // 5c. Newly exposed URL after alphanumeric prefix eviction.
        let mut t5c = Terminal::new(
            20,
            2,
            CoreConfig {
                scrollback_limit: 0,
                ..CoreConfig::default()
            },
        );
        t5c.advance(b"prefixprefixprefix01https://beta.test/xy");
        assert!(t5c.hyperlink_at(0, 1).is_none());
        t5c.advance(b"\r\nl2");
        assert_eq!(
            t5c.hyperlink_at(0, 0).map(|h| h.uri.as_str()),
            Some("https://beta.test/xy")
        );

        // 6. 3-row wrapped URL partial eviction + OSC 8 coexistence on tail.
        let mut t6 = Terminal::new(
            20,
            3,
            CoreConfig {
                scrollback_limit: 1,
                ..CoreConfig::default()
            },
        );
        t6.advance(
            b"https://long.example.org/part1/part2/part3 \x1b]8;;https://osc8.test\x07click\x1b]8;;\x07\r\nl2\r\nl3",
        );
        t6.scroll_display(1);
        assert!(t6.hyperlink_at(0, 0).is_none());
        assert!(t6.hyperlink_at(0, 1).is_none());
        assert_eq!(
            t6.hyperlink_at(3, 1).map(|h| h.uri.as_str()),
            Some("https://osc8.test")
        );

        // 7. Single-advance alt-screen transition + RIS (\x1bc).
        let mut t7 = Terminal::new(
            20,
            2,
            CoreConfig {
                scrollback_limit: 0,
                ..CoreConfig::default()
            },
        );
        t7.advance(b"https://p.test\x1b[?1049hhttps://a.test");
        assert_eq!(
            t7.hyperlink_at(0, 0).map(|h| h.uri.as_str()),
            Some("https://a.test")
        );
        t7.advance(b"\x1b[?1049l");
        assert_eq!(
            t7.hyperlink_at(0, 0).map(|h| h.uri.as_str()),
            Some("https://p.test")
        );
        t7.advance(b"\x1bchttps://example.com/abc\r\nl2");
        assert!(t7.hyperlink_at(0, 0).is_none());
    }

    #[test]
    fn implicit_links_unwrapped_tail_detected_when_prev_row_dirty() {
        // 1. Minimal reproducer: alphanumeric suffix on row 0 suppresses URL on row 1,
        //    then EL 2 clears row 0 (soft_wrapped = false) without touching row 1.
        let mut live = Terminal::new(10, 3, CoreConfig::default());
        live.advance(b"abcdefghi0https://b.test/ok");
        assert!(live.hyperlink_at(0, 1).is_none());
        live.advance(b"\x1b[1;1H\x1b[2K");
        assert_eq!(
            live.hyperlink_at(0, 1).map(|h| h.uri.as_str()),
            Some("https://b.test/ok")
        );

        // 2. OSC 8 suppression on row 0 cleared via EL 0 (\x1b[0K).
        let mut t_osc8_el0 = Terminal::new(10, 3, CoreConfig::default());
        t_osc8_el0.advance(b"\x1b]8;;https://osc8.test\x070123456789\x1b]8;;\x07https://b.test/ok");
        assert_eq!(
            t_osc8_el0.hyperlink_at(0, 0).map(|h| h.uri.as_str()),
            Some("https://osc8.test")
        );
        assert!(t_osc8_el0.hyperlink_at(0, 1).is_none());
        t_osc8_el0.advance(b"\x1b[1;1H\x1b[0K");
        assert!(t_osc8_el0.hyperlink_at(0, 0).is_none());
        assert_eq!(
            t_osc8_el0.hyperlink_at(0, 1).map(|h| h.uri.as_str()),
            Some("https://b.test/ok")
        );

        // 3. Invalid-scheme suppression ("https://.x") on phys_start - 1 cleared via DECSTBM scroll-up.
        let mut t_decstbm = Terminal::new(10, 4, CoreConfig::default());
        t_decstbm.advance(b"row0\r\nhttps://.xhttps://b.test/ok");
        assert!(t_decstbm.hyperlink_at(0, 1).is_none());
        assert!(t_decstbm.hyperlink_at(0, 2).is_none());
        t_decstbm.advance(b"\x1b[1;2r\x1b[1S");
        assert_eq!(
            t_decstbm.hyperlink_at(0, 2).map(|h| h.uri.as_str()),
            Some("https://b.test/ok")
        );

        // 4. Invalid-scheme suppression on row 0 cleared via EL 2 with scrollback present.
        let mut t_sb = Terminal::new(
            10,
            3,
            CoreConfig {
                scrollback_limit: 4,
                ..CoreConfig::default()
            },
        );
        t_sb.advance(b"hist0\r\nhist1\r\nhttps://.xhttps://b.test/ok");
        assert!(t_sb.hyperlink_at(0, 1).is_none());
        t_sb.advance(b"\x1b[1;1H\x1b[2K");
        assert_eq!(
            t_sb.hyperlink_at(0, 1).map(|h| h.uri.as_str()),
            Some("https://b.test/ok")
        );
    }

    #[test]
    fn scheme_sep_prefilter_dirty_range_and_vecdeque_lookup() {
        use super::{ImplicitLinkStore, logical_line_has_scheme_sep};
        use crate::cell::{Cell, CellFlags};
        use crate::color::PackedColor;
        use crate::grid::{Grid, ScreenKind};

        let mut grid = Grid::new(ScreenKind::Primary, 6, 4, 4);
        let write_str = |g: &mut Grid, vp_row: usize, s: &str, soft_wrap: bool| {
            for (c, ch) in s.chars().enumerate() {
                g.write_cell(
                    vp_row,
                    c,
                    Cell::new(
                        ch,
                        PackedColor::Default,
                        PackedColor::Default,
                        CellFlags::empty(),
                    ),
                    None,
                    PackedColor::Default,
                );
            }
            if soft_wrap {
                g.set_row_soft_wrapped(vp_row, true);
            }
        };

        // Timestamps with ':' never trigger scheme separator prefilter.
        write_str(&mut grid, 0, "12:00:", true);
        write_str(&mut grid, 1, "00Z ok", false);
        assert!(!logical_line_has_scheme_sep(&grid, 0, 1));

        // "http:" split right at soft-wrap boundary before "//a.io" is detected by prefilter.
        write_str(&mut grid, 2, " http:", true);
        write_str(&mut grid, 3, "//a.io", false);
        assert!(logical_line_has_scheme_sep(&grid, 2, 3));

        let mut store = ImplicitLinkStore::default();
        store.refresh_dirty(&grid, Some(grid.oldest_abs_line()));
        assert_eq!(
            store
                .find_at(grid.abs_line_of_buf(2), 1)
                .map(|h| h.uri.as_str()),
            Some("http://a.io")
        );
        assert_eq!(
            store
                .find_at(grid.abs_line_of_buf(3), 5)
                .map(|h| h.uri.as_str()),
            Some("http://a.io")
        );

        // Scroll & evict while adding many links so VecDeque wraps around (both as_slices() non-empty).
        let mut big = Grid::new(ScreenKind::Primary, 24, 4, 16);
        let mut big_store = ImplicitLinkStore::default();
        for i in 0..40 {
            big.scroll_up_region(0, 3, 1, PackedColor::Default, true);
            let dirty = big.live_viewport_to_abs(3);
            let s = format!("https://u{i:02}.test/p");
            write_str(&mut big, 3, &s, false);
            big_store.refresh_dirty(&big, Some(dirty));
        }
        // Verify every retained line resolves via binary search on VecDeque slices.
        for buf_idx in 0..big.total_len() {
            let abs = big.abs_line_of_buf(buf_idx);
            let row = big.row_by_buf(buf_idx).unwrap();
            if row.trimmed_len() > 0 {
                assert!(
                    big_store.find_at(abs, 0).is_some(),
                    "missing link at buf_idx={buf_idx} abs={abs:?}"
                );
            }
        }
        assert!(big_store.find_at(crate::grid::AbsLine(0), 0).is_none());
    }
}
