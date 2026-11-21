//! Inline terminal graphics support (Kitty Graphics Protocol & Sixel) with pure-Rust decoders.

#[cfg(feature = "kitty-graphics")]
mod base64;
#[cfg(feature = "kitty-graphics")]
mod inflate;
#[cfg(feature = "kitty-graphics")]
pub mod kitty;
#[cfg(feature = "kitty-graphics")]
mod png;
#[cfg(feature = "sixel")]
pub mod sixel;
pub mod store;

#[cfg(feature = "kitty-graphics")]
pub use self::kitty::{
    KITTY_DIACRITICS, KITTY_PLACEHOLDER_CHAR, KittyApcContext, KittyFileMediumPolicy, KittyOutcome,
    KittyPlaceholderCell, KittyShmReader, KittyTransportPolicy, decode_placeholder_cell,
    diacritic_index, handle_kitty_apc_with_context,
};
pub use self::store::{
    AnimationPlaybackState, DEFAULT_IMAGE_MEMORY_BUDGET, ImageFormat, ImagePlacement, ImageStore,
    MAX_IMAGE_DIMENSION, StoredImage,
};

#[cfg(test)]
mod tests;
