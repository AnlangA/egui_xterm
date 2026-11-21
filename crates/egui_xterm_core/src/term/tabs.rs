//! Horizontal tab stop table (`HTS`, `TBC`, `HT`, `CHT`, `CBT`).

/// Fixed/custom horizontal tab stops across terminal columns.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TabStops {
    stops: Vec<bool>,
}

impl TabStops {
    /// Creates default tab stops every 8 columns (`8, 16, 24, ...`) for `cols` columns.
    #[must_use]
    pub fn new(cols: usize) -> Self {
        let mut stops = vec![false; cols.max(1)];
        for (c, slot) in stops.iter_mut().enumerate() {
            if c > 0 && c % 8 == 0 {
                *slot = true;
            }
        }
        Self { stops }
    }

    /// Resets all tab stops to the default 8-column intervals for `cols` columns.
    pub fn reset(&mut self, cols: usize) {
        *self = Self::new(cols);
    }

    /// Resizes the tab-stop table, preserving existing stops and seeding newly added columns at
    /// multiples of 8.
    pub fn resize(&mut self, new_cols: usize) {
        let new_cols = new_cols.max(1);
        let old_cols = self.stops.len();
        self.stops.resize(new_cols, false);
        for (c, slot) in self
            .stops
            .iter_mut()
            .enumerate()
            .take(new_cols)
            .skip(old_cols)
        {
            if c > 0 && c % 8 == 0 {
                *slot = true;
            }
        }
    }

    /// Sets a tab stop at `col` (`HTS`, `ESC H`).
    pub fn set(&mut self, col: usize) {
        if let Some(slot) = self.stops.get_mut(col) {
            *slot = true;
        }
    }

    /// Clears the tab stop at `col` (`TBC 0`).
    pub fn clear(&mut self, col: usize) {
        if let Some(slot) = self.stops.get_mut(col) {
            *slot = false;
        }
    }

    /// Clears all tab stops (`TBC 3`).
    pub fn clear_all(&mut self) {
        self.stops.fill(false);
    }

    /// Advances `count` tab stops right from `col`, clamping at `cols - 1`.
    #[must_use]
    pub fn next(&self, mut col: usize, count: usize) -> usize {
        let last = self.stops.len().saturating_sub(1);
        for _ in 0..count {
            if col >= last {
                return last;
            }
            let mut next_col = col + 1;
            while next_col < last && !self.stops[next_col] {
                next_col += 1;
            }
            col = next_col;
        }
        col
    }

    /// Moves `count` tab stops left from `col`, clamping at `0`.
    #[must_use]
    pub fn prev(&self, mut col: usize, count: usize) -> usize {
        for _ in 0..count {
            if col == 0 {
                return 0;
            }
            let mut prev_col = col - 1;
            while prev_col > 0 && !self.stops[prev_col] {
                prev_col -= 1;
            }
            col = prev_col;
        }
        col
    }
}
