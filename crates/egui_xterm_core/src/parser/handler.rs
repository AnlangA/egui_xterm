//! Consumer callback trait receiving events from [`Parser`](super::Parser).

use super::params::CsiParams;
use super::state::OscTerminator;

/// Consumer interface receiving parsed VT events from [`Parser`](super::Parser).
pub trait VtHandler {
    /// Handles a single decoded Unicode scalar value in `Ground` state.
    fn print(&mut self, ch: char);

    /// Fast-path batch for contiguous printable ASCII (`0x20..=0x7E`) in `Ground`.
    ///
    /// The default implementation calls [`Self::print`] for each byte.
    fn print_ascii_run(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.print(b as char);
        }
    }

    /// Executes a C0 control (`0x00..=0x1F`) or 8-bit C1 control (`0x80..=0x9F`).
    fn execute(&mut self, byte: u8);

    /// Begins a `DCS` payload stream upon receiving its final byte (`0x40..=0x7E`).
    fn hook(&mut self, params: &CsiParams, intermediates: &[u8], action: u8);

    /// Delivers one raw payload byte to the active `DCS` handler.
    fn put(&mut self, byte: u8);

    /// Ends the active `DCS` payload stream (`ST`, `ESC`, `CAN`, or `SUB`).
    fn unhook(&mut self);

    /// Dispatches a completed `OSC` command split on `;` into `params`.
    fn osc_dispatch(&mut self, params: &[&[u8]], terminator: OscTerminator);

    /// Dispatches a completed `CSI` sequence.
    fn csi_dispatch(&mut self, params: &CsiParams, intermediates: &[u8], action: u8);

    /// Dispatches a completed `ESC` sequence.
    fn esc_dispatch(&mut self, intermediates: &[u8], action: u8);
}
