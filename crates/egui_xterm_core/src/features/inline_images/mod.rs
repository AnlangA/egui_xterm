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

pub use self::store::{
    DEFAULT_IMAGE_MEMORY_BUDGET, ImageFormat, ImagePlacement, ImageStore, StoredImage,
};

#[cfg(test)]
mod tests;
