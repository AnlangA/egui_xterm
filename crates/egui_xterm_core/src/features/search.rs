//! Scrollback and viewport literal/regex search (`search` feature).

use std::fmt;

use memchr::memmem;
use regex::RegexBuilder;

use crate::features::{CellSpan, extract_logical_line_with_spans};
use crate::grid::{Grid, ScreenKind};

/// Query parameters for [`Terminal::search`](crate::Terminal::search).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SearchQuery {
    /// Literal substring or regular expression pattern to match.
    pub pattern: String,
    /// When `true`, interpret `pattern` as a regular expression; otherwise literal text.
    pub is_regex: bool,
    /// When `true` (default), match case-sensitively.
    pub case_sensitive: bool,
}

impl SearchQuery {
    /// Creates a case-sensitive literal substring query.
    #[must_use]
    pub fn literal(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            is_regex: false,
            case_sensitive: true,
        }
    }

    /// Creates a case-sensitive regular expression query.
    #[must_use]
    pub fn regex(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            is_regex: true,
            case_sensitive: true,
        }
    }

    /// Sets whether matching is case-sensitive.
    #[must_use]
    pub const fn with_case_sensitive(mut self, case_sensitive: bool) -> Self {
        self.case_sensitive = case_sensitive;
        self
    }

    /// Sets whether `pattern` is interpreted as a regular expression.
    #[must_use]
    pub const fn with_regex(mut self, is_regex: bool) -> Self {
        self.is_regex = is_regex;
        self
    }
}

/// Error returned when compiling a [`SearchQuery`] fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    /// Regular expression pattern failed to compile.
    InvalidRegex(String),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRegex(msg) => write!(f, "invalid search regex: {msg}"),
        }
    }
}

impl std::error::Error for SearchError {}

/// Single search match located in the active terminal grid.
///
/// Line indices (`line`, `end_line`) are 0-based chronological indices over retained rows
/// (`0..total_lines()`), matching [`Terminal::scrollback_line_text`](crate::Terminal::scrollback_line_text).
/// Column indices (`start_col` inclusive, `end_col` exclusive) cover full wide-character spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SearchMatch {
    /// 0-based chronological retained line where the match starts.
    pub line: usize,
    /// Inclusive 0-based start column on `line`.
    pub start_col: u16,
    /// Exclusive 0-based end column on `end_line`.
    pub end_col: u16,
    /// 0-based chronological retained line where the match ends (differs from `line` across soft wraps).
    pub end_line: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct SearchState {
    matches: Vec<SearchMatch>,
    active_index: Option<usize>,
    screen_kind: ScreenKind,
    content_epoch: u64,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            matches: Vec::new(),
            active_index: None,
            screen_kind: ScreenKind::Primary,
            content_epoch: 0,
        }
    }
}

impl SearchState {
    pub(crate) fn clear(&mut self) {
        self.matches.clear();
        self.active_index = None;
    }

    pub(crate) fn validate(&mut self, grid: &Grid) -> bool {
        if self.matches.is_empty()
            || self.screen_kind != grid.kind()
            || self.content_epoch != grid.content_epoch()
        {
            self.clear();
            false
        } else {
            true
        }
    }

    pub(crate) fn matches(&self, grid: &Grid) -> &[SearchMatch] {
        if self.screen_kind != grid.kind() || self.content_epoch != grid.content_epoch() {
            &[]
        } else {
            &self.matches
        }
    }

    pub(crate) fn active_match(&self, grid: &Grid) -> Option<&SearchMatch> {
        if self.screen_kind != grid.kind() || self.content_epoch != grid.content_epoch() {
            return None;
        }
        let idx = self.active_index?;
        self.matches.get(idx)
    }

    pub(crate) fn step_next(&mut self, grid: &Grid) -> Option<&SearchMatch> {
        if !self.validate(grid) {
            return None;
        }
        let len = self.matches.len();
        let next_idx = match self.active_index {
            None => 0,
            Some(i) => (i + 1) % len,
        };
        self.active_index = Some(next_idx);
        self.matches.get(next_idx)
    }

    pub(crate) fn step_prev(&mut self, grid: &Grid) -> Option<&SearchMatch> {
        if !self.validate(grid) {
            return None;
        }
        let len = self.matches.len();
        let prev_idx = match self.active_index {
            None => len - 1,
            Some(0) => len - 1,
            Some(i) => i - 1,
        };
        self.active_index = Some(prev_idx);
        self.matches.get(prev_idx)
    }

    pub(crate) fn execute(
        &mut self,
        grid: &Grid,
        query: &SearchQuery,
    ) -> Result<Vec<SearchMatch>, SearchError> {
        if query.pattern.is_empty() {
            self.matches.clear();
            self.active_index = None;
            self.screen_kind = grid.kind();
            self.content_epoch = grid.content_epoch();
            return Ok(Vec::new());
        }

        enum Matcher<'a> {
            LiteralSensitive(memmem::Finder<'a>),
            LiteralAsciiInsensitive {
                finder: memmem::Finder<'static>,
                lower_pat: Vec<u8>,
            },
            Regex(regex::Regex),
        }

        let matcher = if query.is_regex {
            let compiled = RegexBuilder::new(&query.pattern)
                .case_insensitive(!query.case_sensitive)
                .build()
                .map_err(|e| SearchError::InvalidRegex(e.to_string()))?;
            Matcher::Regex(compiled)
        } else if query.case_sensitive {
            Matcher::LiteralSensitive(memmem::Finder::new(query.pattern.as_bytes()))
        } else if query.pattern.is_ascii() {
            let lower_pat: Vec<u8> = query
                .pattern
                .bytes()
                .map(|b| b.to_ascii_lowercase())
                .collect();
            let finder = memmem::Finder::new(&lower_pat).into_owned();
            Matcher::LiteralAsciiInsensitive { finder, lower_pat }
        } else {
            let compiled = RegexBuilder::new(&regex::escape(&query.pattern))
                .case_insensitive(true)
                .build()
                .map_err(|e| SearchError::InvalidRegex(e.to_string()))?;
            Matcher::Regex(compiled)
        };

        let mut results = Vec::new();
        if let Some(end_buf_idx) = grid.last_non_empty_buf_idx() {
            let mut text_buf = String::new();
            let mut lower_buf = Vec::new();
            let mut spans: Vec<CellSpan> = Vec::new();
            let mut unicode_ci_fallback: Option<regex::Regex> = None;

            let mut phys_start = 0usize;
            while phys_start <= end_buf_idx {
                let mut phys_end = phys_start;
                while phys_end < end_buf_idx
                    && grid.row_by_buf(phys_end).is_some_and(|r| r.soft_wrapped())
                {
                    phys_end += 1;
                }

                extract_logical_line_with_spans(
                    grid,
                    phys_start,
                    phys_end,
                    &mut text_buf,
                    &mut spans,
                );

                if !text_buf.is_empty() && !spans.is_empty() {
                    match &matcher {
                        Matcher::LiteralSensitive(finder) => {
                            let haystack = text_buf.as_bytes();
                            let pat_len = query.pattern.len();
                            let mut pos = 0usize;
                            while pos + pat_len <= haystack.len() {
                                let Some(rel) = finder.find(&haystack[pos..]) else {
                                    break;
                                };
                                let m_start = pos + rel;
                                let m_end = m_start + pat_len;
                                if let Some(m) = span_range_to_match(&spans, m_start, m_end) {
                                    results.push(m);
                                }
                                pos = m_end;
                            }
                        }
                        Matcher::LiteralAsciiInsensitive { finder, lower_pat } => {
                            if text_buf.is_ascii() {
                                lower_buf.clear();
                                lower_buf.extend(text_buf.bytes().map(|b| b.to_ascii_lowercase()));
                                let pat_len = lower_pat.len();
                                let mut pos = 0usize;
                                while pos + pat_len <= lower_buf.len() {
                                    let Some(rel) = finder.find(&lower_buf[pos..]) else {
                                        break;
                                    };
                                    let m_start = pos + rel;
                                    let m_end = m_start + pat_len;
                                    if let Some(m) = span_range_to_match(&spans, m_start, m_end) {
                                        results.push(m);
                                    }
                                    pos = m_end;
                                }
                            } else {
                                let re = unicode_ci_fallback.get_or_insert_with(|| {
                                    RegexBuilder::new(&regex::escape(&query.pattern))
                                        .case_insensitive(true)
                                        .build()
                                        .expect("escaped literal regex is always valid")
                                });
                                for m in re.find_iter(&text_buf) {
                                    if m.start() < m.end()
                                        && let Some(sm) =
                                            span_range_to_match(&spans, m.start(), m.end())
                                    {
                                        results.push(sm);
                                    }
                                }
                            }
                        }
                        Matcher::Regex(re) => {
                            for m in re.find_iter(&text_buf) {
                                if m.start() < m.end()
                                    && let Some(sm) =
                                        span_range_to_match(&spans, m.start(), m.end())
                                {
                                    results.push(sm);
                                }
                            }
                        }
                    }
                }

                phys_start = phys_end + 1;
            }
        }

        self.matches.clone_from(&results);
        self.active_index = None;
        self.screen_kind = grid.kind();
        self.content_epoch = grid.content_epoch();
        Ok(results)
    }
}

fn span_range_to_match(spans: &[CellSpan], m_start: usize, m_end: usize) -> Option<SearchMatch> {
    if m_start >= m_end || spans.is_empty() {
        return None;
    }
    let first_idx = spans.partition_point(|s| s.byte_end <= m_start);
    let last_idx = spans
        .partition_point(|s| s.byte_start < m_end)
        .saturating_sub(1);
    let first = spans.get(first_idx)?;
    let last = spans.get(last_idx)?;
    Some(SearchMatch {
        line: first.buf_row,
        start_col: first.start_col,
        end_col: last.end_col,
        end_line: last.buf_row,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{CoreConfig, Terminal};

    #[test]
    fn literal_regex_case_and_error_isolation() {
        let mut term = Terminal::new(40, 6, CoreConfig::default());
        term.advance("Café CAFÉ cafe\r\nerr-404 ok\r\n".as_bytes());

        let exact = term.search(&SearchQuery::literal("cafe")).unwrap();
        assert_eq!(exact.len(), 1);
        assert_eq!(
            (exact[0].line, exact[0].start_col, exact[0].end_col),
            (0, 10, 14)
        );

        // Unicode case-insensitive literal match.
        let unicode_ci = term
            .search(&SearchQuery::literal("café").with_case_sensitive(false))
            .unwrap();
        assert_eq!(unicode_ci.len(), 2);

        // Invalid regex preserves previous matches and terminal contents.
        assert!(term.search(&SearchQuery::regex(r"[unclosed(")).is_err());
        assert_eq!(term.search_matches().len(), 2);
        assert_eq!(term.visible_line_text(0).as_deref(), Some("Café CAFÉ cafe"));

        // Empty query clears state cleanly.
        assert!(term.search(&SearchQuery::literal("")).unwrap().is_empty());
        assert!(term.next_search_match().is_none());
        assert!(term.prev_search_match().is_none());
        assert!(term.active_search_match().is_none());
    }

    #[test]
    fn zero_width_wide_wrap_and_scroll_invariant_navigation() {
        let cfg = CoreConfig {
            scrollback_limit: 50,
            ..CoreConfig::default()
        };
        let mut term = Terminal::new(8, 4, cfg);
        term.advance("中文测试a\r\n".as_bytes());
        let wide_wrap = term.search(&SearchQuery::literal("测试a")).unwrap();
        assert_eq!(
            wide_wrap,
            vec![SearchMatch {
                line: 0,
                start_col: 4,
                end_col: 1,
                end_line: 1,
            }]
        );

        // Zero-width regexes skip empty matches and terminate bounded.
        let zw = term.search(&SearchQuery::regex(r"a*")).unwrap();
        assert_eq!(zw.len(), 1);
        assert!(term.search(&SearchQuery::regex(r"\b")).unwrap().is_empty());

        // Scroll invariance + wrap-around + content_epoch invalidation.
        for i in 0..20 {
            term.advance(format!("row{i:02}\r\n").as_bytes());
        }
        let matches = term.search(&SearchQuery::regex(r"row(02|18)")).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(term.prev_search_match().unwrap().line, matches[1].line);
        assert_eq!(term.prev_search_match().unwrap().line, matches[0].line);
        // Scrolling into view during navigation must NOT invalidate cached search state.
        assert!(term.display_offset() > 0);
        assert_eq!(term.next_search_match().unwrap().line, matches[1].line);
        assert_eq!(term.next_search_match().unwrap().line, matches[0].line);

        // Content mutation invalidates stale matches.
        term.advance(b"mutated");
        assert!(term.active_search_match().is_none());
        assert!(term.next_search_match().is_none());
        assert!(term.search_matches().is_empty());

        // Switching to alternate screen also invalidates search_matches().
        term.advance(b"findme");
        assert_eq!(
            term.search(&SearchQuery::literal("findme")).unwrap().len(),
            1
        );
        assert_eq!(term.search_matches().len(), 1);
        term.advance(b"\x1b[?1049h");
        assert!(term.search_matches().is_empty());

        term.clear_search();
        assert!(term.search_matches().is_empty());
    }
}
