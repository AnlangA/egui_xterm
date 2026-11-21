//! Scrollback and viewport literal/regex search (`search` feature).

use std::fmt;

use memchr::memmem;
use regex::RegexBuilder;

use crate::features::{CellSpan, extract_logical_line_with_spans};
use crate::grid::{AbsLine, Grid, Row, ScreenKind};

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
    /// When `true`, require matches to align with whole-word boundaries.
    #[cfg_attr(feature = "serde", serde(default))]
    pub whole_word: bool,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            is_regex: false,
            case_sensitive: true,
            whole_word: false,
        }
    }
}

impl SearchQuery {
    /// Creates a case-sensitive literal substring query.
    #[must_use]
    pub fn literal(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            is_regex: false,
            case_sensitive: true,
            whole_word: false,
        }
    }

    /// Creates a case-sensitive regular expression query.
    #[must_use]
    pub fn regex(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            is_regex: true,
            case_sensitive: true,
            whole_word: false,
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

    /// Sets whether matches must align with whole-word boundaries.
    #[must_use]
    pub const fn with_whole_word(mut self, whole_word: bool) -> Self {
        self.whole_word = whole_word;
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

/// Progress snapshot returned by incremental search operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchStepProgress {
    /// Number of physical buffer lines scanned so far.
    pub scanned_lines: usize,
    /// Total physical lines targeted by the incremental search.
    pub total_lines: usize,
    /// Number of matches discovered so far.
    pub matches_found: usize,
    /// `true` once all target lines have been scanned.
    pub complete: bool,
}

#[derive(Debug, Clone)]
enum CompiledMatcher {
    LiteralSensitive {
        finder: memmem::Finder<'static>,
        pat_len: usize,
        whole_word: bool,
    },
    LiteralAsciiInsensitive {
        finder: memmem::Finder<'static>,
        pat_len: usize,
        pattern: String,
        whole_word: bool,
    },
    Regex {
        regex: regex::Regex,
        whole_word_filter: bool,
    },
}

impl CompiledMatcher {
    fn compile(query: &SearchQuery) -> Result<Self, SearchError> {
        if query.is_regex {
            let pattern_buf;
            let pat_str = if query.whole_word {
                pattern_buf = format!(r"\b(?:{})\b", query.pattern);
                pattern_buf.as_str()
            } else {
                query.pattern.as_str()
            };
            let compiled = RegexBuilder::new(pat_str)
                .case_insensitive(!query.case_sensitive)
                .build()
                .map_err(|e| SearchError::InvalidRegex(e.to_string()))?;
            Ok(Self::Regex {
                regex: compiled,
                whole_word_filter: false,
            })
        } else if query.case_sensitive {
            Ok(Self::LiteralSensitive {
                finder: memmem::Finder::new(query.pattern.as_bytes()).into_owned(),
                pat_len: query.pattern.len(),
                whole_word: query.whole_word,
            })
        } else if query.pattern.is_ascii() {
            let lower_pat: Vec<u8> = query
                .pattern
                .bytes()
                .map(|b| b.to_ascii_lowercase())
                .collect();
            let finder = memmem::Finder::new(&lower_pat).into_owned();
            Ok(Self::LiteralAsciiInsensitive {
                finder,
                pat_len: lower_pat.len(),
                pattern: query.pattern.clone(),
                whole_word: query.whole_word,
            })
        } else {
            let compiled = RegexBuilder::new(&regex::escape(&query.pattern))
                .case_insensitive(true)
                .build()
                .map_err(|e| SearchError::InvalidRegex(e.to_string()))?;
            Ok(Self::Regex {
                regex: compiled,
                whole_word_filter: query.whole_word,
            })
        }
    }
}

#[derive(Debug, Clone)]
struct SearchCursor {
    query: SearchQuery,
    matcher: CompiledMatcher,
    screen_kind: ScreenKind,
    content_epoch: u64,
    cols: usize,
    next_buf_idx: usize,
    end_buf_idx: usize,
    total_lines: usize,
    results: Vec<SearchMatch>,
}

#[derive(Debug, Clone)]
pub(crate) struct SearchState {
    matches: Vec<SearchMatch>,
    active_index: Option<usize>,
    screen_kind: ScreenKind,
    content_epoch: u64,
    last_query: Option<SearchQuery>,
    last_oldest_abs: AbsLine,
    last_cols: usize,
    last_damage_epoch: u64,
    text_buf: String,
    lower_buf: Vec<u8>,
    spans: Vec<CellSpan>,
    cursor: Option<SearchCursor>,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            matches: Vec::new(),
            active_index: None,
            screen_kind: ScreenKind::Primary,
            content_epoch: 0,
            last_query: None,
            last_oldest_abs: AbsLine(0),
            last_cols: 0,
            last_damage_epoch: 0,
            text_buf: String::new(),
            lower_buf: Vec::new(),
            spans: Vec::new(),
            cursor: None,
        }
    }
}

impl SearchState {
    pub(crate) fn clear(&mut self) {
        self.matches.clear();
        self.active_index = None;
        self.last_query = None;
        self.cursor = None;
    }

    pub(crate) fn validate(&mut self, grid: &Grid) -> bool {
        if self.matches.is_empty()
            || self.screen_kind != grid.kind()
            || self.content_epoch != grid.content_epoch()
        {
            self.active_index = None;
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

    /// Returns cached matches intersecting 0-based chronological retained `line` in `O(log M)`.
    pub(crate) fn matches_on_line(&self, grid: &Grid, line: usize) -> &[SearchMatch] {
        let all = self.matches(grid);
        if all.is_empty() {
            return &[];
        }
        let lo = all.partition_point(|m| m.end_line < line);
        let hi = lo + all[lo..].partition_point(|m| m.line <= line);
        &all[lo..hi]
    }

    /// Returns cached matches intersecting `abs_line` in `O(log M)`.
    pub(crate) fn matches_on_abs_line(&self, grid: &Grid, abs_line: AbsLine) -> &[SearchMatch] {
        let Some(line) = grid.buf_idx_of_abs(abs_line) else {
            return &[];
        };
        self.matches_on_line(grid, line)
    }

    /// Returns cached matches intersecting `[start_line, end_line_excl)` in `O(log M)`.
    pub(crate) fn matches_in_range(
        &self,
        grid: &Grid,
        start_line: usize,
        end_line_excl: usize,
    ) -> &[SearchMatch] {
        let all = self.matches(grid);
        if all.is_empty() || start_line >= end_line_excl {
            return &[];
        }
        let lo = all.partition_point(|m| m.end_line < start_line);
        let hi = lo + all[lo..].partition_point(|m| m.line < end_line_excl);
        &all[lo..hi]
    }

    pub(crate) fn active_index(&self, grid: &Grid) -> Option<usize> {
        if self.screen_kind != grid.kind() || self.content_epoch != grid.content_epoch() {
            return None;
        }
        self.active_index.filter(|&idx| idx < self.matches.len())
    }

    pub(crate) fn active_match(&self, grid: &Grid) -> Option<&SearchMatch> {
        let idx = self.active_index(grid)?;
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
            self.cursor = None;
            self.matches.clear();
            self.active_index = None;
            self.record_sync_meta(grid, query);
            return Ok(Vec::new());
        }

        // Exact repeat query on unchanged grid returns cached matches immediately.
        if self.screen_kind == grid.kind()
            && self.content_epoch == grid.content_epoch()
            && self.last_cols == grid.cols()
            && self.last_query.as_ref() == Some(query)
        {
            self.cursor = None;
            self.active_index = None;
            return Ok(self.matches.clone());
        }

        let matcher = CompiledMatcher::compile(query)?;
        let mut results = Vec::new();
        let mut unicode_ci_fallback: Option<regex::Regex> = None;

        // Literal query-refinement fast path: if the new literal query extends the previous literal
        // query on the exact same grid state, only rescan logical lines that had prior matches.
        let can_refine = self.screen_kind == grid.kind()
            && self.content_epoch == grid.content_epoch()
            && self.last_cols == grid.cols()
            && self
                .last_query
                .as_ref()
                .is_some_and(|prev| is_literal_refinement(prev, query));

        if can_refine {
            if !self.matches.is_empty() {
                let total_rows = grid.total_len();
                let mut prev_phys_end: Option<usize> = None;

                for m in &self.matches {
                    if prev_phys_end.is_some_and(|end| m.line <= end) {
                        continue;
                    }
                    if m.line >= total_rows {
                        continue;
                    }
                    let (phys_start, phys_end) = logical_line_bounds(grid, m.line, total_rows - 1);
                    prev_phys_end = Some(phys_end);
                    scan_logical_line(
                        grid,
                        phys_start,
                        phys_end,
                        &matcher,
                        &mut self.text_buf,
                        &mut self.lower_buf,
                        &mut self.spans,
                        &mut unicode_ci_fallback,
                        &mut results,
                    )?;
                }
            }
        } else if let Some(end_buf_idx) = grid.last_non_empty_buf_idx() {
            let mut phys_start = 0usize;
            while phys_start <= end_buf_idx {
                let mut phys_end = phys_start;
                while phys_end < end_buf_idx
                    && grid.row_by_buf(phys_end).is_some_and(Row::soft_wrapped)
                {
                    phys_end += 1;
                }
                scan_logical_line(
                    grid,
                    phys_start,
                    phys_end,
                    &matcher,
                    &mut self.text_buf,
                    &mut self.lower_buf,
                    &mut self.spans,
                    &mut unicode_ci_fallback,
                    &mut results,
                )?;
                phys_start = phys_end + 1;
            }
        }

        self.cursor = None;
        self.matches.clone_from(&results);
        self.active_index = None;
        self.record_sync_meta(grid, query);
        Ok(results)
    }

    /// Incrementally updates cached search matches after grid mutations starting around `dirty_from`.
    ///
    /// Returns `Ok(true)` if cached matches were updated (or re-evaluated), or `Ok(false)` when
    /// no active query exists, the grid switched screens, or `content_epoch` is already current.
    pub(crate) fn refresh(
        &mut self,
        grid: &Grid,
        dirty_from: Option<AbsLine>,
    ) -> Result<bool, SearchError> {
        let Some(query) = self.last_query.clone() else {
            return Ok(false);
        };
        if self.screen_kind != grid.kind() {
            return Ok(false);
        }
        if self.content_epoch == grid.content_epoch() && self.last_cols == grid.cols() {
            return Ok(false);
        }
        self.cursor = None;
        if query.pattern.is_empty() {
            self.matches.clear();
            self.active_index = None;
            self.record_sync_meta(grid, &query);
            return Ok(true);
        }

        let oldest = grid.oldest_abs_line();
        if self.last_cols != grid.cols() || oldest < self.last_oldest_abs {
            self.execute(grid, &query)?;
            return Ok(true);
        }

        let evicted_delta =
            usize::try_from(oldest.0.saturating_sub(self.last_oldest_abs.0)).unwrap_or(usize::MAX);
        if evicted_delta >= grid.total_len() {
            self.execute(grid, &query)?;
            return Ok(true);
        }

        let saved_active = self
            .active_index
            .and_then(|idx| self.matches.get(idx).copied())
            .filter(|m| m.line >= evicted_delta)
            .map(|m| (m.line - evicted_delta, m.start_col));

        if evicted_delta > 0 && !self.matches.is_empty() {
            let first_keep = self.matches.partition_point(|m| m.end_line < evicted_delta);
            self.matches.drain(..first_keep);
            let drop_straddle = self.matches.partition_point(|m| m.line < evicted_delta);
            self.matches.drain(..drop_straddle);
            for m in &mut self.matches {
                m.line -= evicted_delta;
                m.end_line -= evicted_delta;
            }
        }

        let Some(end_buf_idx) = grid.last_non_empty_buf_idx() else {
            self.matches.clear();
            self.active_index = None;
            self.record_sync_meta(grid, &query);
            return Ok(true);
        };

        let scan_start_buf = if let Some(d) = dirty_from {
            let raw = if d <= oldest {
                0
            } else if let Some(idx) = grid.buf_idx_of_abs(d) {
                idx
            } else {
                end_buf_idx + 1
            };
            if raw <= end_buf_idx {
                logical_line_start(grid, raw)
            } else if grid.row_by_buf(end_buf_idx).is_some_and(Row::soft_wrapped) {
                logical_line_start(grid, end_buf_idx)
            } else {
                end_buf_idx + 1
            }
        } else {
            end_buf_idx + 1
        };

        let keep_len = self
            .matches
            .partition_point(|m| m.end_line < scan_start_buf);
        self.matches.truncate(keep_len);

        let need_front_rescan =
            evicted_delta > 0 && scan_start_buf > 0 && grid.row_by_buf(0).is_some();
        let need_tail_rescan = scan_start_buf <= end_buf_idx;

        if need_front_rescan || need_tail_rescan {
            let matcher = CompiledMatcher::compile(&query)?;
            let mut unicode_ci_fallback: Option<regex::Regex> = None;

            if need_front_rescan {
                let mut phys_end_0 = 0usize;
                while phys_end_0 < end_buf_idx
                    && grid.row_by_buf(phys_end_0).is_some_and(Row::soft_wrapped)
                {
                    phys_end_0 += 1;
                }
                let after_front = self.matches.partition_point(|m| m.line <= phys_end_0);
                self.matches.drain(..after_front);

                let mut front_matches = Vec::new();
                scan_logical_line(
                    grid,
                    0,
                    phys_end_0,
                    &matcher,
                    &mut self.text_buf,
                    &mut self.lower_buf,
                    &mut self.spans,
                    &mut unicode_ci_fallback,
                    &mut front_matches,
                )?;
                if !front_matches.is_empty() {
                    self.matches.splice(0..0, front_matches);
                }
            }

            if need_tail_rescan {
                let mut phys_start = scan_start_buf;
                while phys_start <= end_buf_idx {
                    let mut phys_end = phys_start;
                    while phys_end < end_buf_idx
                        && grid.row_by_buf(phys_end).is_some_and(Row::soft_wrapped)
                    {
                        phys_end += 1;
                    }
                    scan_logical_line(
                        grid,
                        phys_start,
                        phys_end,
                        &matcher,
                        &mut self.text_buf,
                        &mut self.lower_buf,
                        &mut self.spans,
                        &mut unicode_ci_fallback,
                        &mut self.matches,
                    )?;
                    phys_start = phys_end + 1;
                }
            }
        }

        self.active_index = saved_active.and_then(|(line, start_col)| {
            let slice = self.matches_on_line_raw(line);
            let base = self.matches.partition_point(|m| m.end_line < line);
            slice
                .iter()
                .position(|m| m.line == line && m.start_col == start_col)
                .map(|rel| base + rel)
        });
        self.record_sync_meta(grid, &query);
        Ok(true)
    }

    /// Starts a chunked incremental search over `grid`, compiling `query` upfront so errors leave
    /// existing search state intact.
    pub(crate) fn begin_incremental(
        &mut self,
        grid: &Grid,
        query: &SearchQuery,
    ) -> Result<SearchStepProgress, SearchError> {
        let matcher = if query.pattern.is_empty() {
            None
        } else {
            Some(CompiledMatcher::compile(query)?)
        };

        let Some(end_buf_idx) = matcher.as_ref().and_then(|_| grid.last_non_empty_buf_idx()) else {
            self.cursor = None;
            self.matches.clear();
            self.active_index = None;
            self.record_sync_meta(grid, query);
            let total_lines = grid.total_len();
            return Ok(SearchStepProgress {
                scanned_lines: total_lines,
                total_lines,
                matches_found: 0,
                complete: true,
            });
        };

        let total_lines = end_buf_idx + 1;
        if let Some(compiled) = matcher {
            self.cursor = Some(SearchCursor {
                query: query.clone(),
                matcher: compiled,
                screen_kind: grid.kind(),
                content_epoch: grid.content_epoch(),
                cols: grid.cols(),
                next_buf_idx: 0,
                end_buf_idx,
                total_lines,
                results: Vec::new(),
            });
        }
        Ok(SearchStepProgress {
            scanned_lines: 0,
            total_lines,
            matches_found: 0,
            complete: false,
        })
    }

    /// Advances an active chunked incremental search by up to `max_lines` physical lines.
    pub(crate) fn step_incremental(&mut self, grid: &Grid, max_lines: usize) -> SearchStepProgress {
        let Some(mut cursor) = self.cursor.take() else {
            let total = grid.total_len();
            return SearchStepProgress {
                scanned_lines: total,
                total_lines: total,
                matches_found: self.matches(grid).len(),
                complete: true,
            };
        };

        if cursor.screen_kind != grid.kind()
            || cursor.content_epoch != grid.content_epoch()
            || cursor.cols != grid.cols()
        {
            cursor.screen_kind = grid.kind();
            cursor.content_epoch = grid.content_epoch();
            cursor.cols = grid.cols();
            cursor.results.clear();
            cursor.next_buf_idx = 0;
            if let Some(end_idx) = grid.last_non_empty_buf_idx() {
                cursor.end_buf_idx = end_idx;
                cursor.total_lines = end_idx + 1;
            } else {
                self.matches.clear();
                self.active_index = None;
                self.record_sync_meta(grid, &cursor.query);
                return SearchStepProgress {
                    scanned_lines: 0,
                    total_lines: 0,
                    matches_found: 0,
                    complete: true,
                };
            }
        }

        let budget = max_lines.max(1);
        let mut lines_done = 0usize;
        let mut unicode_ci_fallback: Option<regex::Regex> = None;

        while cursor.next_buf_idx <= cursor.end_buf_idx && lines_done < budget {
            let phys_start = cursor.next_buf_idx;
            let mut phys_end = phys_start;
            while phys_end < cursor.end_buf_idx
                && grid.row_by_buf(phys_end).is_some_and(Row::soft_wrapped)
            {
                phys_end += 1;
            }
            if scan_logical_line(
                grid,
                phys_start,
                phys_end,
                &cursor.matcher,
                &mut self.text_buf,
                &mut self.lower_buf,
                &mut self.spans,
                &mut unicode_ci_fallback,
                &mut cursor.results,
            )
            .is_err()
            {
                self.matches.clear();
                self.active_index = None;
                return SearchStepProgress {
                    scanned_lines: cursor.total_lines,
                    total_lines: cursor.total_lines,
                    matches_found: 0,
                    complete: true,
                };
            }
            lines_done += phys_end - phys_start + 1;
            cursor.next_buf_idx = phys_end + 1;
        }

        let complete = cursor.next_buf_idx > cursor.end_buf_idx;
        let matches_found = cursor.results.len();
        let scanned_lines = cursor.next_buf_idx.min(cursor.total_lines);
        let total_lines = cursor.total_lines;

        if complete {
            self.matches = cursor.results;
            self.active_index = None;
            self.record_sync_meta(grid, &cursor.query);
        } else {
            self.cursor = Some(cursor);
        }

        SearchStepProgress {
            scanned_lines,
            total_lines,
            matches_found,
            complete,
        }
    }

    fn record_sync_meta(&mut self, grid: &Grid, query: &SearchQuery) {
        self.screen_kind = grid.kind();
        self.content_epoch = grid.content_epoch();
        self.last_oldest_abs = grid.oldest_abs_line();
        self.last_cols = grid.cols();
        self.last_damage_epoch = grid.damage_epoch();
        self.last_query = Some(query.clone());
    }

    fn matches_on_line_raw(&self, line: usize) -> &[SearchMatch] {
        if self.matches.is_empty() {
            return &[];
        }
        let lo = self.matches.partition_point(|m| m.end_line < line);
        let hi = lo + self.matches[lo..].partition_point(|m| m.line <= line);
        &self.matches[lo..hi]
    }
}

#[inline]
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

const fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch,
        '\u{0300}'..='\u{036F}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FE20}'..='\u{FE2F}'
    )
}

fn is_whole_word_match(text: &str, m_start: usize, m_end: usize) -> bool {
    let Some(matched) = text.get(m_start..m_end) else {
        return false;
    };
    let Some(first_ch) = matched.chars().next() else {
        return false;
    };
    let Some(last_base_ch) = matched.chars().rev().find(|&c| !is_combining_mark(c)) else {
        return false;
    };
    if is_word_char(first_ch)
        && let Some(prev_ch) = text[..m_start].chars().next_back()
        && is_word_char(prev_ch)
    {
        return false;
    }
    if is_word_char(last_base_ch)
        && let Some(next_ch) = text[m_end..].chars().next()
        && is_word_char(next_ch)
    {
        return false;
    }
    true
}

#[inline]
fn next_char_boundary(s: &str, idx: usize) -> usize {
    s.get(idx..)
        .and_then(|tail| tail.chars().next())
        .map_or(s.len(), |ch| idx + ch.len_utf8())
}

fn is_literal_refinement(prev: &SearchQuery, next: &SearchQuery) -> bool {
    if prev.is_regex
        || next.is_regex
        || prev.whole_word
        || prev.case_sensitive != next.case_sensitive
    {
        return false;
    }
    if prev.pattern.is_empty() || next.pattern.len() < prev.pattern.len() {
        return false;
    }
    if prev.case_sensitive {
        next.pattern.contains(&prev.pattern)
    } else {
        prev.pattern.is_ascii()
            && next.pattern.is_ascii()
            && next
                .pattern
                .to_ascii_lowercase()
                .contains(&prev.pattern.to_ascii_lowercase())
    }
}

fn logical_line_start(grid: &Grid, mut buf_idx: usize) -> usize {
    while buf_idx > 0 && grid.row_by_buf(buf_idx - 1).is_some_and(Row::soft_wrapped) {
        buf_idx -= 1;
    }
    buf_idx
}

fn logical_line_bounds(grid: &Grid, buf_idx: usize, max_buf_idx: usize) -> (usize, usize) {
    let phys_start = logical_line_start(grid, buf_idx);
    let mut phys_end = phys_start;
    while phys_end < max_buf_idx && grid.row_by_buf(phys_end).is_some_and(Row::soft_wrapped) {
        phys_end += 1;
    }
    (phys_start, phys_end)
}

#[expect(
    clippy::too_many_arguments,
    reason = "reusable scratch buffers for zero-churn logical line search"
)]
fn scan_logical_line(
    grid: &Grid,
    phys_start: usize,
    phys_end: usize,
    matcher: &CompiledMatcher,
    text_buf: &mut String,
    lower_buf: &mut Vec<u8>,
    spans: &mut Vec<CellSpan>,
    unicode_ci_fallback: &mut Option<regex::Regex>,
    results: &mut Vec<SearchMatch>,
) -> Result<(), SearchError> {
    extract_logical_line_with_spans(grid, phys_start, phys_end, text_buf, spans);
    if text_buf.is_empty() || spans.is_empty() {
        return Ok(());
    }

    match matcher {
        CompiledMatcher::LiteralSensitive {
            finder,
            pat_len,
            whole_word,
        } => {
            let haystack = text_buf.as_bytes();
            let mut pos = 0usize;
            while pos + *pat_len <= haystack.len() {
                let Some(rel) = finder.find(&haystack[pos..]) else {
                    break;
                };
                let m_start = pos + rel;
                let m_end = m_start + *pat_len;
                if *whole_word && !is_whole_word_match(text_buf, m_start, m_end) {
                    pos = next_char_boundary(text_buf, m_start);
                    continue;
                }
                if let Some(m) = span_range_to_match(spans, m_start, m_end) {
                    results.push(m);
                }
                pos = m_end;
            }
        }
        CompiledMatcher::LiteralAsciiInsensitive {
            finder,
            pat_len,
            pattern,
            whole_word,
        } => {
            if text_buf.is_ascii() {
                lower_buf.clear();
                lower_buf.extend(text_buf.bytes().map(|b| b.to_ascii_lowercase()));
                let mut pos = 0usize;
                while pos + *pat_len <= lower_buf.len() {
                    let Some(rel) = finder.find(&lower_buf[pos..]) else {
                        break;
                    };
                    let m_start = pos + rel;
                    let m_end = m_start + *pat_len;
                    if *whole_word && !is_whole_word_match(text_buf, m_start, m_end) {
                        pos = m_start + 1;
                        continue;
                    }
                    if let Some(m) = span_range_to_match(spans, m_start, m_end) {
                        results.push(m);
                    }
                    pos = m_end;
                }
            } else {
                let re = match unicode_ci_fallback {
                    Some(re) => re,
                    None => {
                        let built = RegexBuilder::new(&regex::escape(pattern))
                            .case_insensitive(true)
                            .build()
                            .map_err(|e| SearchError::InvalidRegex(e.to_string()))?;
                        unicode_ci_fallback.insert(built)
                    }
                };
                if *whole_word {
                    let mut pos = 0usize;
                    while pos <= text_buf.len() {
                        let Some(m) = re.find_at(text_buf, pos) else {
                            break;
                        };
                        if m.start() < m.end() {
                            if is_whole_word_match(text_buf, m.start(), m.end()) {
                                if let Some(sm) = span_range_to_match(spans, m.start(), m.end()) {
                                    results.push(sm);
                                }
                                pos = m.end();
                            } else {
                                pos = next_char_boundary(text_buf, m.start());
                            }
                        } else {
                            pos = next_char_boundary(text_buf, m.start());
                        }
                    }
                } else {
                    for m in re.find_iter(text_buf) {
                        if m.start() < m.end()
                            && let Some(sm) = span_range_to_match(spans, m.start(), m.end())
                        {
                            results.push(sm);
                        }
                    }
                }
            }
        }
        CompiledMatcher::Regex {
            regex,
            whole_word_filter,
        } => {
            if *whole_word_filter {
                let mut pos = 0usize;
                while pos <= text_buf.len() {
                    let Some(m) = regex.find_at(text_buf, pos) else {
                        break;
                    };
                    if m.start() < m.end() {
                        if is_whole_word_match(text_buf, m.start(), m.end()) {
                            if let Some(sm) = span_range_to_match(spans, m.start(), m.end()) {
                                results.push(sm);
                            }
                            pos = m.end();
                        } else {
                            pos = next_char_boundary(text_buf, m.start());
                        }
                    } else {
                        pos = next_char_boundary(text_buf, m.start());
                    }
                }
            } else {
                for m in regex.find_iter(text_buf) {
                    if m.start() < m.end()
                        && let Some(sm) = span_range_to_match(spans, m.start(), m.end())
                    {
                        results.push(sm);
                    }
                }
            }
        }
    }
    Ok(())
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

    #[test]
    fn whole_word_literal_ascii_ci_unicode_ci_and_regex() {
        let mut term = Terminal::new(40, 6, CoreConfig::default());
        term.advance("foo foobar xfoofoo Foo café cafés\r\n".as_bytes());
        term.advance("alpha_1 alpha\r\n".as_bytes());

        // Case-sensitive literal whole word: matches "foo" only, skips "foobar" and "xfoofoo".
        let cs = term
            .search(&SearchQuery::literal("foo").with_whole_word(true))
            .unwrap();
        assert_eq!(cs.len(), 1);
        assert_eq!((cs[0].line, cs[0].start_col, cs[0].end_col), (0, 0, 3));

        // Overlapping literal rejection: "aa" in "xaaa aa" must advance by 1 char on rejection,
        // matching only the standalone "aa" at the end (not the "aa" inside "xaaa").
        let mut overlap_term = Terminal::new(20, 2, CoreConfig::default());
        overlap_term.advance(b"xaaa aa");
        let ov = overlap_term
            .search(&SearchQuery::literal("aa").with_whole_word(true))
            .unwrap();
        assert_eq!(ov.len(), 1);
        assert_eq!((ov[0].start_col, ov[0].end_col), (5, 7));

        // ASCII case-insensitive whole word: matches "foo" and "Foo".
        let ci = term
            .search(
                &SearchQuery::literal("FOO")
                    .with_case_sensitive(false)
                    .with_whole_word(true),
            )
            .unwrap();
        assert_eq!(ci.len(), 2);
        assert_eq!((ci[0].start_col, ci[1].start_col), (0, 19));

        // Unicode case-insensitive whole word: matches "café", skips "cafés".
        let uci = term
            .search(
                &SearchQuery::literal("CAFÉ")
                    .with_case_sensitive(false)
                    .with_whole_word(true),
            )
            .unwrap();
        assert_eq!(uci.len(), 1);
        assert_eq!(uci[0].start_col, 23);

        // Regex whole word: skips "alpha_1" because '_' is a word char, matches "alpha".
        let re = term
            .search(&SearchQuery::regex(r"alpha").with_whole_word(true))
            .unwrap();
        assert_eq!(re.len(), 1);
        assert_eq!((re[0].line, re[0].start_col, re[0].end_col), (1, 8, 13));
    }

    #[test]
    fn binary_search_lookup_incremental_refresh_refinement_and_cursor() {
        use crate::color::PackedColor;

        let mut grid = Grid::new(ScreenKind::Primary, 10, 4, 10);
        let write_str = |g: &mut Grid, vp_row: usize, s: &str, soft_wrap: bool| {
            for (c, ch) in s.chars().enumerate() {
                g.write_cell(
                    vp_row,
                    c,
                    crate::cell::Cell::new(
                        ch,
                        PackedColor::Default,
                        PackedColor::Default,
                        crate::cell::CellFlags::empty(),
                    ),
                    None,
                    PackedColor::Default,
                );
            }
            if soft_wrap {
                g.set_row_soft_wrapped(vp_row, true);
            }
        };

        write_str(&mut grid, 0, "hit0 hit0", false);
        write_str(&mut grid, 1, "wrap_01_hi", true);
        write_str(&mut grid, 2, "t_end", false);
        write_str(&mut grid, 3, "tail hit3", false);

        let mut state = SearchState::default();
        let q_hit = SearchQuery::literal("hit");
        let found = state.execute(&grid, &q_hit).unwrap();
        assert_eq!(found.len(), 4);
        assert_eq!(
            found[2],
            SearchMatch {
                line: 1,
                start_col: 8,
                end_col: 1,
                end_line: 2,
            }
        );

        // O(log M) line / abs_line / range lookups including soft-wrapped match (lines 1..=2).
        assert_eq!(state.matches_on_line(&grid, 0).len(), 2);
        assert_eq!(state.matches_on_line(&grid, 1).len(), 1);
        assert_eq!(state.matches_on_line(&grid, 2).len(), 1);
        assert_eq!(state.matches_on_line(&grid, 3).len(), 1);
        assert_eq!(
            state
                .matches_on_abs_line(&grid, grid.abs_line_of_buf(1))
                .len(),
            1
        );
        assert_eq!(
            state
                .matches_on_abs_line(&grid, grid.abs_line_of_buf(2))
                .len(),
            1
        );
        assert_eq!(state.matches_in_range(&grid, 1, 3).len(), 1);
        assert_eq!(state.matches_in_range(&grid, 0, 4).len(), 4);
        assert!(state.matches_in_range(&grid, 2, 2).is_empty());

        // Step active index tracking.
        assert!(state.active_index(&grid).is_none());
        let _ = state.step_next(&grid);
        assert_eq!(state.active_index(&grid), Some(0));
        let _ = state.step_next(&grid);
        assert_eq!(state.active_index(&grid), Some(1));

        // Literal query refinement fast path ("hit" -> "hit3").
        let refined = state.execute(&grid, &SearchQuery::literal("hit3")).unwrap();
        assert_eq!(refined.len(), 1);
        assert_eq!(refined[0].line, 3);

        // Incremental refresh after scroll-up + new tail match.
        let _ = state.execute(&grid, &q_hit).unwrap();
        let _ = state.step_prev(&grid);
        assert_eq!(state.active_index(&grid), Some(3));
        grid.scroll_up_region(0, 3, 1, PackedColor::Default, true);
        let dirty_abs = grid.live_viewport_to_abs(3);
        write_str(&mut grid, 3, "new hit4", false);
        assert!(state.refresh(&grid, Some(dirty_abs)).unwrap());
        assert_eq!(state.matches(&grid).len(), 5);
        assert_eq!(state.matches(&grid).last().unwrap().line, 4);
        assert_eq!(state.active_index(&grid), Some(3));

        // Chunked incremental cursor steps 1 physical line at a time.
        let mut cursor_state = SearchState::default();
        let p0 = cursor_state.begin_incremental(&grid, &q_hit).unwrap();
        assert!(!p0.complete);
        let mut steps = 0usize;
        let mut last_p = p0;
        while !last_p.complete {
            last_p = cursor_state.step_incremental(&grid, 1);
            steps += 1;
            assert!(steps <= 10);
        }
        assert!(steps >= 3);
        assert_eq!(cursor_state.matches(&grid), state.matches(&grid));
    }

    #[test]
    fn incremental_refresh_drops_eviction_straddling_soft_wrapped_match() {
        use crate::color::PackedColor;

        let mut grid = Grid::new(ScreenKind::Primary, 10, 4, 0);
        let write_str = |g: &mut Grid, vp_row: usize, s: &str, soft_wrap: bool| {
            for (c, ch) in s.chars().enumerate() {
                g.write_cell(
                    vp_row,
                    c,
                    crate::cell::Cell::new(
                        ch,
                        PackedColor::Default,
                        PackedColor::Default,
                        crate::cell::CellFlags::empty(),
                    ),
                    None,
                    PackedColor::Default,
                );
            }
            if soft_wrap {
                g.set_row_soft_wrapped(vp_row, true);
            }
        };

        write_str(&mut grid, 0, "wrap_01_hi", true);
        write_str(&mut grid, 1, "t_end", false);
        write_str(&mut grid, 2, "keep hit2", false);

        let mut state = SearchState::default();
        let initial = state.execute(&grid, &SearchQuery::literal("hit")).unwrap();
        assert_eq!(initial.len(), 2);

        // Evict row 0 so the wrapped match (lines 0..=1) straddles the eviction boundary.
        grid.scroll_up_region(0, 3, 1, PackedColor::Default, true);
        assert!(state.refresh(&grid, None).unwrap());
        let after = state.matches(&grid);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].line, 1);
        assert_eq!(after[0].start_col, 5);
    }

    #[test]
    fn refresh_drops_stale_matches_on_tail_and_full_grid_erasure() {
        // ED 0 erasing tail rows below an empty row (raw > end_buf_idx) drops stale matches.
        let mut t = Terminal::new(20, 4, CoreConfig::default());
        t.advance(b"top\r\n\r\nfoo\r\nbar foo");
        assert_eq!(t.search(&SearchQuery::literal("foo")).unwrap().len(), 2);
        t.advance(b"\x1b[2;1H\x1b[0J");
        assert!(t.refresh_search().unwrap());
        assert_eq!(t.search_matches().len(), 0);
        assert!(t.active_search_match().is_none());

        // EL 2 erasing only the last non-empty row drops its match while keeping earlier matches.
        let mut t2 = Terminal::new(20, 4, CoreConfig::default());
        t2.advance(b"hit0\r\nhit1");
        assert_eq!(t2.search(&SearchQuery::literal("hit")).unwrap().len(), 2);
        t2.advance(b"\x1b[2;1H\x1b[2K");
        assert!(t2.refresh_search().unwrap());
        assert_eq!(t2.search_matches().len(), 1);
        assert_eq!(t2.search_matches()[0].line, 0);

        // Full-grid erase (last_non_empty_buf_idx() == None) clears all matches and syncs epoch.
        assert!(t2.next_search_match().is_some());
        t2.advance(b"\x1b[2J");
        assert!(t2.refresh_search().unwrap());
        assert!(t2.search_matches().is_empty());
        assert!(t2.active_search_match().is_none());
        assert!(!t2.refresh_search().unwrap());
    }

    #[test]
    fn refresh_rescans_truncated_soft_wrapped_tail_when_continuation_erased() {
        let mut t = Terminal::new(4, 3, CoreConfig::default());
        t.advance(b"aaabcd");
        assert_eq!(t.search(&SearchQuery::literal("bc")).unwrap().len(), 1);
        // Erase row 1 ("cd") via EL 2; row 0 ("aaab") remains soft_wrapped and end_buf_idx == 0 < raw == 1.
        t.advance(b"\x1b[2;1H\x1b[2K");
        assert!(t.refresh_search().unwrap());
        assert!(t.search_matches().is_empty());

        // Re-test with a match wholly inside row 0 plus one spanning 0..=1.
        let mut t2 = Terminal::new(4, 3, CoreConfig::default());
        t2.advance(b"ab_ab_cd");
        assert_eq!(t2.search(&SearchQuery::literal("ab")).unwrap().len(), 2);
        t2.advance(b"\x1b[2;1H\x1b[2K");
        assert!(t2.refresh_search().unwrap());
        assert_eq!(t2.search_matches().len(), 1);
        assert_eq!(t2.search_matches()[0].line, 0);
        assert_eq!(t2.search_matches()[0].start_col, 0);
    }

    #[test]
    fn refresh_discovers_unblocked_whole_word_at_line_zero_after_eviction_and_preserves_active() {
        let cfg = CoreConfig {
            scrollback_limit: 5,
            ..CoreConfig::default()
        };
        let mut t = Terminal::new(4, 2, cfg.clone());
        t.advance(b"aaaxfoo\r\nbb\r\ncc\r\ndd\r\nee\r\nff");
        let q = SearchQuery::literal("foo").with_whole_word(true);
        assert_eq!(t.search(&q).unwrap().len(), 0);

        // Evict row 0 ("aaax"); line 0 is now "foo", dirty_from is at the bottom viewport (> 0).
        t.advance(b"\r\n");
        assert!(t.refresh_search().unwrap());
        assert_eq!(t.search_matches().len(), 1);
        assert_eq!(t.search_matches()[0].line, 0);
        assert_eq!(t.search_matches()[0].start_col, 0);
        assert_eq!(t.search_matches()[0].end_col, 3);

        // Active match on a later line stays tracked when front_matches are spliced in.
        let mut t2 = Terminal::new(4, 2, cfg);
        t2.advance(b"aaaxfoo\r\nbb\r\ncc\r\ndd\r\nee\r\nfoo");
        assert_eq!(t2.search(&q).unwrap().len(), 1);
        assert_eq!(t2.next_search_match().unwrap().line, 6);
        t2.advance(b"\r\n");
        assert!(t2.refresh_search().unwrap());
        let matches = t2.search_matches();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line, 0);
        assert_eq!(matches[1].line, 5);
        assert_eq!(t2.active_search_match().map(|m| m.line), Some(5));
    }

    #[test]
    fn refresh_clears_stale_incremental_cursor() {
        let mut t = Terminal::new(20, 4, CoreConfig::default());
        t.advance(b"alpha\r\nbeta");
        assert_eq!(t.search(&SearchQuery::literal("alpha")).unwrap().len(), 1);

        let p = t
            .begin_incremental_search(&SearchQuery::literal("beta"))
            .unwrap();
        assert!(!p.complete);

        t.advance(b"\r\nalpha");
        assert!(t.refresh_search().unwrap());
        assert_eq!(t.search_matches().len(), 2);

        // Incremental cursor was cleared by refresh() rather than continuing stale scan.
        let step = t.step_incremental_search(1);
        assert!(step.complete);
        assert_eq!(step.matches_found, 2);
    }

    #[test]
    fn execute_invalid_regex_preserves_cursor_while_repeat_and_empty_clear_cursor() {
        let mut t = Terminal::new(20, 4, CoreConfig::default());
        t.advance(b"foo one\r\nbar two\r\n");
        assert_eq!(t.search(&SearchQuery::literal("foo")).unwrap().len(), 1);

        let p0 = t
            .begin_incremental_search(&SearchQuery::literal("bar"))
            .unwrap();
        assert!(!p0.complete);

        // Invalid regex must not disturb the active incremental cursor.
        assert!(matches!(
            t.search(&SearchQuery::regex("[unclosed")),
            Err(SearchError::InvalidRegex(_))
        ));

        // Exact-repeat fast path cancels the in-flight incremental cursor and restores "foo".
        let repeated = t.search(&SearchQuery::literal("foo")).unwrap();
        assert_eq!(repeated.len(), 1);
        let step = t.step_incremental_search(100);
        assert!(step.complete);
        assert_eq!(step.matches_found, 1);
        assert_eq!(t.search_matches().len(), 1);

        // Empty query also cancels an active incremental cursor cleanly.
        let p1 = t
            .begin_incremental_search(&SearchQuery::literal("bar"))
            .unwrap();
        assert!(!p1.complete);
        assert!(t.search(&SearchQuery::literal("")).unwrap().is_empty());
        let step2 = t.step_incremental_search(100);
        assert!(step2.complete);
        assert_eq!(step2.matches_found, 0);
        assert!(t.search_matches().is_empty());
    }

    #[test]
    fn repro6a_stale_navigation_before_refresh_preserves_prior_matches() {
        let mut t = Terminal::new(20, 6, CoreConfig::default());
        t.advance(b"foo 1\r\nfoo 2\r\n");
        assert_eq!(t.search(&SearchQuery::literal("foo")).unwrap().len(), 2);
        assert_eq!(t.next_search_match().map(|m| m.line), Some(0));

        // Grid mutation advances content_epoch; stale navigation must not wipe cached matches.
        t.advance(b"foo 3\r\n");
        assert!(t.search_matches().is_empty());
        assert!(t.active_search_match().is_none());
        assert!(t.active_search_match_index().is_none());
        assert!(t.next_search_match().is_none());
        assert!(t.prev_search_match().is_none());

        assert_eq!(t.refresh_search(), Ok(true));
        let matches = t.search_matches();
        assert_eq!(matches.len(), 3);
        assert_eq!(
            matches.iter().map(|m| m.line).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(t.next_search_match().map(|m| m.line), Some(0));
        assert_eq!(t.next_search_match().map(|m| m.line), Some(1));
        assert_eq!(t.next_search_match().map(|m| m.line), Some(2));

        // Same invariant holds for prev_search_match() and scrollback lines before dirty_from.
        let cfg = CoreConfig {
            scrollback_limit: 20,
            ..CoreConfig::default()
        };
        let mut t2 = Terminal::new(10, 3, cfg);
        t2.advance(b"foo_wrap00foo_wrap01\r\nmid\r\n");
        assert_eq!(t2.search(&SearchQuery::literal("foo")).unwrap().len(), 2);
        t2.advance(b"tail foo2\r\n");
        assert!(t2.prev_search_match().is_none());
        assert_eq!(t2.refresh_search(), Ok(true));
        assert_eq!(t2.search_matches().len(), 3);
        assert_eq!(t2.prev_search_match().map(|m| m.line), Some(3));
    }

    #[test]
    fn repro6b_alt_screen_navigation_preserves_primary_search_cache() {
        let mut t = Terminal::new(20, 6, CoreConfig::default());
        t.advance(b"foobar 1\r\nfoobar 2\r\n");
        assert_eq!(t.search(&SearchQuery::literal("foo")).unwrap().len(), 2);
        assert_eq!(t.next_search_match().map(|m| m.line), Some(0));

        // Enter alternate screen and attempt navigation while primary cache is off-screen.
        t.advance(b"\x1b[?1049h");
        assert!(t.search_matches().is_empty());
        assert!(t.active_search_match().is_none());
        assert!(t.next_search_match().is_none());
        assert!(t.prev_search_match().is_none());

        // Returning to primary restores visibility and keeps exact-repeat & refinement intact.
        t.advance(b"\x1b[?1049l");
        assert_eq!(t.search_matches().len(), 2);
        assert_eq!(t.search_matches_on_line(0).len(), 1);
        assert_eq!(t.search_matches_on_line(1).len(), 1);

        let repeated = t.search(&SearchQuery::literal("foo")).unwrap();
        assert_eq!(repeated.len(), 2);
        let refined = t.search(&SearchQuery::literal("foobar")).unwrap();
        assert_eq!(refined.len(), 2);
        assert_eq!(t.next_search_match().map(|m| m.line), Some(0));
        assert_eq!(t.next_search_match().map(|m| m.line), Some(1));
    }

    #[test]
    fn stale_navigation_during_incremental_search_stays_coherent() {
        let mut t = Terminal::new(20, 6, CoreConfig::default());
        t.advance(b"foo 1\r\nbar 1\r\n");
        assert_eq!(t.search(&SearchQuery::literal("foo")).unwrap().len(), 1);

        let prog = t
            .begin_incremental_search(&SearchQuery::literal("bar"))
            .unwrap();
        assert!(!prog.complete);

        // Mutate grid and call stale navigation; neither prior cache nor active cursor breaks.
        t.advance(b"bar 2\r\n");
        assert!(t.next_search_match().is_none());
        assert!(t.prev_search_match().is_none());

        let done = t.step_incremental_search(100);
        assert!(done.complete);
        assert_eq!(done.matches_found, 2);
        assert_eq!(t.search_matches().len(), 2);
        assert_eq!(t.next_search_match().map(|m| m.line), Some(1));
        assert_eq!(t.next_search_match().map(|m| m.line), Some(2));
    }
}
