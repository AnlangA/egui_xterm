//! Inline terminal graphics support (Kitty Graphics Protocol & Sixel) with pure-Rust decoders.
//!
//! # Examples
//!
//! Manage decoded inline images in a bounded LRU [`ImageStore`] and track visible
//! placements across scrollback pruning:
//!
//! ```
//! use egui_xterm_core::{
//!     AbsLine, ImageFormat, ImagePlacement, ImageStore, ScreenKind, StoredImage,
//! };
//!
//! let mut store = ImageStore::new(16);
//! let first = StoredImage {
//!     id: 1,
//!     width: 2,
//!     height: 1,
//!     format: ImageFormat::Rgba8,
//!     rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
//!     revision: 0,
//! };
//! let second = StoredImage {
//!     id: 2,
//!     rgba: vec![0, 0, 255, 255, 255, 255, 255, 255],
//!     ..first.clone()
//! };
//! let third = StoredImage {
//!     id: 3,
//!     rgba: vec![10, 20, 30, 255, 40, 50, 60, 255],
//!     ..first.clone()
//! };
//!
//! assert!(store.insert_image(first));
//! assert!(store.insert_image(second));
//! assert_eq!(store.used_bytes(), 16);
//!
//! // Touch image 1 so image 2 is evicted as least recently used when inserting image 3.
//! store.touch_image(1);
//! assert!(store.insert_image(third));
//! assert!(store.get_image(1).is_some());
//! assert!(store.get_image(2).is_none());
//! let stored = store.get_image(3).ok_or("image 3 should be stored")?;
//! assert!(stored.revision >= 1);
//! assert_eq!(stored.active_rgba().len(), 8);
//!
//! store.add_placement(
//!     ScreenKind::Primary,
//!     ImagePlacement {
//!         image_id: 3,
//!         placement_id: 1,
//!         abs_line: AbsLine(4),
//!         col: 2,
//!         cols: 2,
//!         rows: 1,
//!         src_x: 0,
//!         src_y: 0,
//!         src_w: 0,
//!         src_h: 0,
//!         z_index: 0,
//!         is_virtual: false,
//!         sub_x: 0,
//!         sub_y: 0,
//!         parent_image_id: 0,
//!         parent_placement_id: 0,
//!         offset_cols: 0,
//!         offset_rows: 0,
//!     },
//! );
//! assert_eq!(
//!     store.visible_placements(ScreenKind::Primary, AbsLine(0), 10).len(),
//!     1
//! );
//!
//! // Evicting lines before line 5 removes the placement and unreferenced image 3.
//! store.prune_before(ScreenKind::Primary, AbsLine(5));
//! assert!(store.placements(ScreenKind::Primary).is_empty());
//! assert!(store.get_image(3).is_none());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#[cfg(feature = "kitty-graphics")]
mod base64;
#[cfg(feature = "kitty-graphics")]
mod inflate;
#[cfg(feature = "kitty-graphics")]
pub mod kitty;
#[cfg(feature = "kitty-graphics")]
pub mod png;
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
