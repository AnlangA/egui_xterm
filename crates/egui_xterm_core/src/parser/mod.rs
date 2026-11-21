//! Clean-room Paul Williams / DEC VT500 parser with streaming UTF-8 and ASCII fast-path.

mod handler;
mod machine;
mod params;
mod state;
mod utf8;

pub use self::handler::VtHandler;
pub use self::machine::Parser;
pub use self::params::{
    CsiParams, CsiParamsIter, DEFAULT_MAX_APC_BYTES, DEFAULT_MAX_OSC_BYTES, MAX_CSI_SLOTS,
    MAX_INTERMEDIATES,
};
pub use self::state::{C1Policy, OscTerminator, State};
