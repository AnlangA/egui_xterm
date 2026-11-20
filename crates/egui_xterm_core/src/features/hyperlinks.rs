//! Explicit (`OSC 8`) and implicit (`http://`, `https://`, `ftp://`) hyperlink tracking
//! (`hyperlinks` feature).

use std::collections::HashSet;

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
    links: Vec<ImplicitLink>,
    last_content_epoch: u64,
    last_damage_epoch: u64,
    last_cols: usize,
    last_oldest_line: AbsLine,
}

impl ImplicitLinkStore {
    pub(crate) fn clear(&mut self) {
        self.links.clear();
        self.last_content_epoch = 0;
        self.last_damage_epoch = 0;
        self.last_cols = 0;
        self.last_oldest_line = AbsLine(0);
    }

    pub(crate) fn refresh(&mut self, grid: &Grid) {
        let content_epoch = grid.content_epoch();
        let damage_epoch = grid.damage_epoch();
        let cols = grid.cols();

        if content_epoch == self.last_content_epoch && cols == self.last_cols {
            return;
        }

        let oldest = grid.oldest_abs_line();
        let force_rescan = self.last_cols != cols || self.last_content_epoch == 0;
        let head_evicted = !force_rescan && oldest > self.last_oldest_line;
        let mut dropped_ranges: Vec<(AbsLine, AbsLine)> = Vec::new();
        if force_rescan {
            self.links.clear();
        } else {
            let prev_damage = self.last_damage_epoch;
            self.links.retain(|link| {
                if link.start_line < oldest {
                    if link.end_line >= oldest {
                        dropped_ranges.push((oldest, link.end_line));
                    }
                    return false;
                }
                let mut line = link.start_line.0;
                while line <= link.end_line.0 {
                    let Some(row) = grid.row_by_abs(AbsLine(line)) else {
                        dropped_ranges.push((link.start_line, link.end_line));
                        return false;
                    };
                    if row.generation() > prev_damage {
                        dropped_ranges.push((link.start_line, link.end_line));
                        return false;
                    }
                    line += 1;
                }
                true
            });
        }

        if let Some(end_buf_idx) = grid.last_non_empty_buf_idx() {
            let mut text_buf = String::new();
            let mut spans: Vec<CellSpan> = Vec::new();
            let mut phys_start = 0usize;

            while phys_start <= end_buf_idx {
                let mut phys_end = phys_start;
                while phys_end < end_buf_idx
                    && grid.row_by_buf(phys_end).is_some_and(Row::soft_wrapped)
                {
                    phys_end += 1;
                }

                let abs_start = grid.abs_line_of_buf(phys_start);
                let abs_end = grid.abs_line_of_buf(phys_end);
                let trailing_erased_continuation = phys_end == end_buf_idx
                    && grid.row_by_buf(phys_end).is_some_and(Row::soft_wrapped)
                    && ((end_buf_idx + 1)..grid.total_len()).any(|idx| {
                        grid.row_by_buf(idx)
                            .is_some_and(|r| r.generation() > self.last_damage_epoch)
                    });
                let had_dropped_link = !dropped_ranges.is_empty()
                    && dropped_ranges
                        .iter()
                        .any(|&(s, e)| !(e < abs_start || s > abs_end));
                let evicted_head_line = head_evicted && phys_start == 0;
                let prev_row_dirty = phys_start > 0
                    && grid
                        .row_by_buf(phys_start - 1)
                        .is_some_and(|r| r.generation() > self.last_damage_epoch);

                let any_dirty = force_rescan
                    || evicted_head_line
                    || prev_row_dirty
                    || trailing_erased_continuation
                    || had_dropped_link
                    || (phys_start..=phys_end).any(|idx| {
                        grid.row_by_buf(idx)
                            .is_some_and(|r| r.generation() > self.last_damage_epoch)
                    });

                if any_dirty {
                    if !force_rescan && !self.links.is_empty() {
                        self.links
                            .retain(|l| l.end_line < abs_start || l.start_line > abs_end);
                    }

                    let has_colon = (phys_start..=phys_end).any(|idx| {
                        grid.row_by_buf(idx).is_some_and(|row| {
                            let trimmed = row.trimmed_len();
                            row.cells()[..trimmed].iter().any(|c| c.codepoint == ':')
                        })
                    });

                    if has_colon {
                        extract_logical_line_with_spans(
                            grid,
                            phys_start,
                            phys_end,
                            &mut text_buf,
                            &mut spans,
                        );
                        scan_implicit_urls(grid, &text_buf, &spans, &mut self.links);
                    }
                }

                phys_start = phys_end + 1;
            }
        } else {
            self.links.clear();
        }

        self.links
            .sort_unstable_by_key(|a| (a.start_line, a.start_col));
        self.last_content_epoch = content_epoch;
        self.last_damage_epoch = damage_epoch;
        self.last_cols = cols;
        self.last_oldest_line = oldest;
    }

    pub(crate) fn find_at(&self, line: AbsLine, col: u16) -> Option<&HyperlinkInfo> {
        self.links
            .iter()
            .find(|link| link.contains(line, col))
            .map(|link| &link.info)
    }

    pub(crate) fn visible_spans_at(
        &self,
        grid: &Grid,
        line: AbsLine,
        col: u16,
    ) -> Option<(&HyperlinkInfo, Vec<HyperlinkSpan>)> {
        let link = self.links.iter().find(|link| link.contains(line, col))?;
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

fn scan_implicit_urls(grid: &Grid, text: &str, spans: &[CellSpan], out: &mut Vec<ImplicitLink>) {
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
                    out.push(ImplicitLink {
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

/// Helper to collect all live `HyperlinkId`s referenced by a [`Grid`].
pub(crate) fn collect_grid_hyperlink_ids(grid: &Grid, out: &mut HashSet<HyperlinkId>) {
    for row in grid.rows_deque() {
        if !row.may_have_extras() {
            continue;
        }
        for cell in row.cells() {
            if cell.extra_id != 0
                && let Some(ext) = grid.cell_extra(cell.extra_id)
                && let Some(hid) = ext.hyperlink_id
            {
                out.insert(hid);
            }
        }
    }
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
}
