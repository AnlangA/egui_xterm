//! Terminal parsing and rendering telemetry counters (`metrics` feature).

/// Monotonic counters tracking parser throughput, resize reflows, and rendered frame activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TerminalMetrics {
    /// Total raw VT/ANSI bytes ingested via [`Terminal::advance`](crate::Terminal::advance)
    /// and [`Terminal::advance_with_time`](crate::Terminal::advance_with_time).
    pub bytes_processed: u64,
    /// Total rendered frames recorded via
    /// [`Terminal::record_frame_rendered`](crate::Terminal::record_frame_rendered).
    pub frames_rendered: u64,
    /// Cumulative dirty viewport rows rendered across recorded frames.
    pub dirty_rows_rendered: u64,
    /// Total primary-grid resize/reflow operations executed.
    pub reflow_count: u64,
    /// Total CSI sequences dispatched by the VT parser.
    #[cfg_attr(feature = "serde", serde(default))]
    pub csi_sequences: u64,
    /// Total OSC sequences dispatched by the VT parser.
    #[cfg_attr(feature = "serde", serde(default))]
    pub osc_sequences: u64,
    /// Total `HyperlinkPool` garbage-collection sweeps executed.
    #[cfg_attr(feature = "serde", serde(default))]
    pub hyperlink_gc_runs: u64,
}

impl TerminalMetrics {
    /// Creates a zero-initialized [`TerminalMetrics`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes_processed: 0,
            frames_rendered: 0,
            dirty_rows_rendered: 0,
            reflow_count: 0,
            csi_sequences: 0,
            osc_sequences: 0,
            hyperlink_gc_runs: 0,
        }
    }

    /// Records `bytes` ingested by the VT parser.
    pub(crate) fn record_bytes(&mut self, bytes: usize) {
        self.bytes_processed = self
            .bytes_processed
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
    }

    /// Records a rendered frame and the number of dirty rows redrawn (`dirty_rows`).
    pub fn record_frame(&mut self, dirty_rows: u64) {
        self.frames_rendered = self.frames_rendered.saturating_add(1);
        self.dirty_rows_rendered = self.dirty_rows_rendered.saturating_add(dirty_rows);
    }

    /// Records a terminal resize/reflow operation.
    pub(crate) fn record_reflow(&mut self) {
        self.reflow_count = self.reflow_count.saturating_add(1);
    }

    /// Records a dispatched CSI sequence.
    pub(crate) fn record_csi(&mut self) {
        self.csi_sequences = self.csi_sequences.saturating_add(1);
    }

    /// Records a dispatched OSC sequence.
    pub(crate) fn record_osc(&mut self) {
        self.osc_sequences = self.osc_sequences.saturating_add(1);
    }

    /// Records a `HyperlinkPool` garbage-collection sweep.
    pub(crate) fn record_hyperlink_gc(&mut self) {
        self.hyperlink_gc_runs = self.hyperlink_gc_runs.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use crate::term::{CoreConfig, Terminal};

    #[test]
    fn metrics_bytes_frames_and_reflow_monotonic_without_double_counting() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        assert_eq!(term.metrics().bytes_processed, 0);
        assert_eq!(term.metrics().frames_rendered, 0);
        assert_eq!(term.metrics().dirty_rows_rendered, 0);
        assert_eq!(term.metrics().reflow_count, 0);

        term.advance(b"");
        term.advance_with_time(b"", 1.0);
        assert_eq!(term.metrics().bytes_processed, 0);

        term.advance(b"hello");
        assert_eq!(term.metrics().bytes_processed, 5);
        term.advance_with_time(b"world!", 1.1);
        assert_eq!(term.metrics().bytes_processed, 11);

        term.resize(80, 24);
        assert_eq!(term.metrics().reflow_count, 0);
        term.resize(40, 12);
        assert_eq!(term.metrics().reflow_count, 1);

        term.record_frame_rendered(3);
        term.record_frame_rendered(0);
        assert_eq!(term.metrics().frames_rendered, 2);
        assert_eq!(term.metrics().dirty_rows_rendered, 3);

        // RIS and ED 2 never reset metrics (`11 + 6 = 17` bytes).
        term.advance(b"\x1b[2J\x1bc");
        assert_eq!(term.metrics().bytes_processed, 17);
        assert_eq!(term.metrics().frames_rendered, 2);
        assert_eq!(term.metrics().reflow_count, 1);
    }

    #[test]
    fn metrics_csi_osc_hyperlink_gc_counters_and_saturation() {
        let mut m = super::TerminalMetrics::new();
        assert_eq!(m, super::TerminalMetrics::default());
        assert_eq!(m.csi_sequences, 0);
        assert_eq!(m.osc_sequences, 0);
        assert_eq!(m.hyperlink_gc_runs, 0);

        m.record_csi();
        m.record_csi();
        m.record_osc();
        m.record_hyperlink_gc();
        assert_eq!(m.csi_sequences, 2);
        assert_eq!(m.osc_sequences, 1);
        assert_eq!(m.hyperlink_gc_runs, 1);

        m.csi_sequences = u64::MAX;
        m.osc_sequences = u64::MAX;
        m.hyperlink_gc_runs = u64::MAX;
        m.record_csi();
        m.record_osc();
        m.record_hyperlink_gc();
        assert_eq!(m.csi_sequences, u64::MAX);
        assert_eq!(m.osc_sequences, u64::MAX);
        assert_eq!(m.hyperlink_gc_runs, u64::MAX);
    }
}
