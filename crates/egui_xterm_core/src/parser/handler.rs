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

    /// Fast-path batch for validated UTF-8 text in `Ground` (`C1Policy::Utf8Only`).
    ///
    /// Guaranteed free of C0 controls (`0x00..=0x1F`) and `DEL` (`0x7F`).
    /// The default implementation calls [`Self::print`] for each `char`.
    fn print_utf8_run(&mut self, text: &str) {
        for ch in text.chars() {
            self.print(ch);
        }
    }

    /// Executes a C0 control (`0x00..=0x1F`) or 8-bit C1 control (`0x80..=0x9F`).
    fn execute(&mut self, byte: u8);

    /// Begins a `DCS` payload stream upon receiving its final byte (`0x40..=0x7E`).
    fn hook(&mut self, params: &CsiParams, intermediates: &[u8], action: u8);

    /// Delivers one raw payload byte to the active `DCS` handler.
    fn put(&mut self, byte: u8);

    /// Fast-path batch for contiguous `DCS` passthrough payload bytes (free of `0x7F`).
    ///
    /// The default implementation calls [`Self::put`] for each byte.
    fn put_slice(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.put(b);
        }
    }

    /// Ends the active `DCS` payload stream (`ST`, `ESC`, `CAN`, or `SUB`).
    fn unhook(&mut self);

    /// Dispatches a completed `OSC` command split on `;` into `params`.
    fn osc_dispatch(&mut self, params: &[&[u8]], terminator: OscTerminator);

    /// Dispatches a completed `CSI` sequence.
    fn csi_dispatch(&mut self, params: &CsiParams, intermediates: &[u8], action: u8);

    /// Dispatches a completed `ESC` sequence.
    fn esc_dispatch(&mut self, intermediates: &[u8], action: u8);

    /// Dispatches a completed `APC` (`ESC _` or `0x9F` ... `ST`) payload.
    fn apc_dispatch(&mut self, _data: &[u8]) {}
}
