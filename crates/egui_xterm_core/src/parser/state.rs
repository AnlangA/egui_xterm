//! Parser state machine enums and policy types.

/// The 14 canonical DEC VT500 / Paul Williams parser states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum State {
    /// Default text state.
    #[default]
    Ground,
    /// Received `ESC` (`0x1B`), awaiting sequence introducer or final byte.
    Escape,
    /// Collecting intermediate bytes (`0x20..=0x2F`) after `ESC`.
    EscapeIntermediate,
    /// Entered `CSI` (`ESC [` or `0x9B`), awaiting prefix, params, or final byte.
    CsiEntry,
    /// Accumulating `CSI` parameter digits, `;`, and `:` subparameter separators.
    CsiParam,
    /// Accumulating `CSI` intermediate bytes (`0x20..=0x2F`).
    CsiIntermediate,
    /// Malformed `CSI` sequence; ignores until final byte (`0x40..=0x7E`).
    CsiIgnore,
    /// Entered `DCS` (`ESC P` or `0x90`), awaiting header parameters.
    DcsEntry,
    /// Accumulating `DCS` header parameters.
    DcsParam,
    /// Accumulating `DCS` header intermediate bytes (`0x20..=0x2F`).
    DcsIntermediate,
    /// Streaming `DCS` payload bytes to [`VtHandler::put`](super::VtHandler::put).
    DcsPassthrough,
    /// Malformed `DCS` sequence; ignores until `ST` / `CAN` / `SUB`.
    DcsIgnore,
    /// Accumulating `OSC` (`ESC ]` or `0x9D`) payload until `BEL` or `ST`.
    OscString,
    /// Accumulating `APC` (`ESC _` or `0x9F`) payload until `ST`.
    ApcString,
    /// Ignoring `SOS` (`ESC X`) or `PM` (`ESC ^`) string until `ST`.
    SosPmApcString,
}

/// Controls handling of raw bytes `0x80..=0x9F` across parser states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum C1Policy {
    /// Modern UTF-8 terminal standard (default): raw `0x80..=0xFF` in `Ground`
    /// decode as UTF-8, and `0x80..=0xFF` in `OscString` / `DcsPassthrough` pass
    /// through as payload bytes so multi-byte UTF-8 never triggers C1 controls
    /// or premature `ST`. Control introducers use 7-bit Fe sequences (`ESC [`,
    /// `ESC ]`, `ESC P`, `ESC \`).
    #[default]
    Utf8Only,
    /// Strict 8-bit VT500 mode: `anywhere` C1 transitions (`0x80..=0x9F`,
    /// including `0x90` DCS, `0x9B` CSI, `0x9C` ST, `0x9D` OSC) are active
    /// across states.
    Vt500EightBit,
}

/// How an `OSC` sequence was terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OscTerminator {
    /// Terminated by `BEL` (`0x07`).
    Bel,
    /// Terminated by `ST` (`ESC \` or 8-bit `0x9C`).
    St,
}
