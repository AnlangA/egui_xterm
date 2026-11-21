//! Storage, LRU memory budgeting, and placement management for inline terminal images.

use std::collections::BTreeMap;

use crate::grid::{AbsLine, ScreenKind};

/// Default memory budget for decoded inline images (64 MiB).
pub const DEFAULT_IMAGE_MEMORY_BUDGET: usize = 64 * 1024 * 1024;

/// Maximum pixel dimension (width or height) accepted for a single inline image.
pub const MAX_IMAGE_DIMENSION: u32 = 8192;

/// Pixel format of a decoded inline image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ImageFormat {
    /// 8-bit per channel RGBA (`[R, G, B, A]`).
    Rgba8,
}

/// Decoded inline image stored in [`ImageStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredImage {
    /// Unique image identifier.
    pub id: u32,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Pixel format.
    pub format: ImageFormat,
    /// Raw pixel bytes (`width * height * 4` for [`ImageFormat::Rgba8`]).
    pub rgba: Vec<u8>,
}

impl StoredImage {
    /// Byte size of the decoded pixel buffer.
    #[must_use]
    pub const fn byte_len(&self) -> usize {
        self.rgba.len()
    }
}

/// Placement of a [`StoredImage`] on the terminal grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImagePlacement {
    /// Referenced image ID in [`ImageStore`].
    pub image_id: u32,
    /// Placement ID (`0` if unspecified).
    pub placement_id: u32,
    /// Absolute line index where the top-left of the placement begins.
    pub abs_line: AbsLine,
    /// Starting column index.
    pub col: u16,
    /// Display width in terminal columns (`0` = auto from image width / cell width).
    pub cols: u16,
    /// Display height in terminal rows (`0` = auto from image height / cell height).
    pub rows: u16,
    /// Source crop X offset in pixels.
    pub src_x: u32,
    /// Source crop Y offset in pixels.
    pub src_y: u32,
    /// Source crop width in pixels (`0` = full remaining width).
    pub src_w: u32,
    /// Source crop height in pixels (`0` = full remaining height).
    pub src_h: u32,
    /// Stacking order (`< 0` behind text, `>= 0` in front of text).
    pub z_index: i32,
}

/// In-progress chunked Kitty Graphics upload (`m=1` ... `m=0`).
#[cfg(feature = "kitty-graphics")]
#[derive(Debug, Clone)]
pub(crate) struct PendingKittyUpload {
    pub image_id: u32,
    pub image_number: u32,
    pub placement_id: u32,
    pub action: super::kitty::KittyAction,
    pub format: u32,
    pub compressed: bool,
    pub width: u32,
    pub height: u32,
    pub cols: u16,
    pub rows: u16,
    pub src_x: u32,
    pub src_y: u32,
    pub src_w: u32,
    pub src_h: u32,
    pub z_index: i32,
    pub no_cursor_move: bool,
    pub quiet: u8,
    pub payload: Vec<u8>,
}

/// Bounded LRU store for decoded inline images and per-screen placements.
#[derive(Debug, Clone)]
pub struct ImageStore {
    memory_budget: usize,
    used_bytes: usize,
    next_id: u32,
    images: BTreeMap<u32, StoredImage>,
    lru_order: Vec<u32>,
    image_numbers: BTreeMap<u32, u32>,
    primary_placements: Vec<ImagePlacement>,
    alt_placements: Vec<ImagePlacement>,
    #[cfg(feature = "kitty-graphics")]
    pub(crate) pending_kitty: Option<PendingKittyUpload>,
}

impl Default for ImageStore {
    fn default() -> Self {
        Self::new(DEFAULT_IMAGE_MEMORY_BUDGET)
    }
}

impl ImageStore {
    /// Creates an empty [`ImageStore`] with the given `memory_budget` in bytes.
    #[must_use]
    pub const fn new(memory_budget: usize) -> Self {
        Self {
            memory_budget,
            used_bytes: 0,
            next_id: 1,
            images: BTreeMap::new(),
            lru_order: Vec::new(),
            image_numbers: BTreeMap::new(),
            primary_placements: Vec::new(),
            alt_placements: Vec::new(),
            #[cfg(feature = "kitty-graphics")]
            pending_kitty: None,
        }
    }

    /// Returns the total byte size of all currently stored images.
    #[must_use]
    pub const fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// Returns the configured memory budget in bytes.
    #[must_use]
    pub const fn memory_budget(&self) -> usize {
        self.memory_budget
    }

    /// Allocates a fresh non-zero image ID not currently in use.
    pub fn allocate_id(&mut self) -> u32 {
        loop {
            let candidate = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if !self.images.contains_key(&candidate) {
                return candidate;
            }
        }
    }

    /// Associates a Kitty `I` image number with an internal `image_id`.
    pub fn register_image_number(&mut self, number: u32, image_id: u32) {
        if number != 0 {
            self.image_numbers.insert(number, image_id);
        }
    }

    /// Resolves a Kitty `I` image number to its stored `image_id`, if any.
    #[must_use]
    pub fn resolve_image_number(&self, number: u32) -> Option<u32> {
        self.image_numbers.get(&number).copied()
    }

    /// Inserts or replaces an image in the store, evicting LRU images if needed to stay within
    /// [`Self::memory_budget`]. Returns `false` if the image itself exceeds the total budget.
    pub fn insert_image(&mut self, image: StoredImage) -> bool {
        let new_bytes = image.byte_len();
        if new_bytes > self.memory_budget {
            return false;
        }
        let id = image.id;
        if let Some(old) = self.images.remove(&id) {
            self.used_bytes = self.used_bytes.saturating_sub(old.byte_len());
            self.lru_order.retain(|&x| x != id);
        }
        while self.used_bytes.saturating_add(new_bytes) > self.memory_budget {
            if let Some(evict_id) = self.lru_order.first().copied() {
                self.remove_image(evict_id);
            } else {
                break;
            }
        }
        self.used_bytes = self.used_bytes.saturating_add(new_bytes);
        self.images.insert(id, image);
        self.lru_order.push(id);
        true
    }

    /// Removes an image by `id` and all placements referencing it.
    pub fn remove_image(&mut self, id: u32) {
        if let Some(old) = self.images.remove(&id) {
            self.used_bytes = self.used_bytes.saturating_sub(old.byte_len());
        }
        self.lru_order.retain(|&x| x != id);
        self.image_numbers.retain(|_, &mut v| v != id);
        self.primary_placements.retain(|p| p.image_id != id);
        self.alt_placements.retain(|p| p.image_id != id);
    }

    /// Looks up a stored image by `id`.
    #[must_use]
    pub fn get_image(&self, id: u32) -> Option<&StoredImage> {
        self.images.get(&id)
    }

    /// Marks `id` as most recently used if present.
    pub fn touch_image(&mut self, id: u32) {
        if self.images.contains_key(&id) {
            self.lru_order.retain(|&x| x != id);
            self.lru_order.push(id);
        }
    }

    /// Adds or replaces a placement on `screen`.
    ///
    /// If `placement.placement_id != 0`, any existing placement with the same
    /// `(image_id, placement_id)` on `screen` is replaced.
    pub fn add_placement(&mut self, screen: ScreenKind, placement: ImagePlacement) {
        self.touch_image(placement.image_id);
        let list = self.placements_mut(screen);
        if placement.placement_id != 0 {
            list.retain(|p| {
                !(p.image_id == placement.image_id && p.placement_id == placement.placement_id)
            });
        }
        list.push(placement);
    }

    /// Returns all placements for `screen`.
    #[must_use]
    pub fn placements(&self, screen: ScreenKind) -> &[ImagePlacement] {
        match screen {
            ScreenKind::Primary => &self.primary_placements,
            ScreenKind::Alternate => &self.alt_placements,
        }
    }

    /// Returns placements on `screen` that intersect the visible viewport
    /// `[top_abs_line, top_abs_line + rows)`.
    #[must_use]
    pub fn visible_placements(
        &self,
        screen: ScreenKind,
        top_abs_line: AbsLine,
        rows: u16,
    ) -> Vec<ImagePlacement> {
        let view_start = top_abs_line.0;
        let view_end = view_start.saturating_add(u64::from(rows));
        self.placements(screen)
            .iter()
            .copied()
            .filter(|p| {
                let p_start = p.abs_line.0;
                let p_end = p_start.saturating_add(u64::from(p.rows.max(1)));
                p_end > view_start && p_start < view_end
            })
            .collect()
    }

    /// Prunes placements on `screen` whose bottom edge is strictly above `min_abs_line`
    /// (evicted from scrollback), and removes any images no longer referenced by either screen
    /// if `gc_unreferenced` is `true`.
    pub fn prune_before(&mut self, screen: ScreenKind, min_abs_line: AbsLine) {
        let list = self.placements_mut(screen);
        let before = list.len();
        list.retain(|p| {
            let p_end = p.abs_line.0.saturating_add(u64::from(p.rows.max(1)));
            p_end > min_abs_line.0
        });
        if list.len() != before {
            self.gc_unreferenced_if_over(0);
        }
    }

    /// Clears all placements on `screen` (e.g. `ED 2` or alt-screen reset) and frees images
    /// no longer referenced by either screen.
    pub fn clear_screen(&mut self, screen: ScreenKind) {
        self.placements_mut(screen).clear();
        self.gc_unreferenced();
    }

    /// Clears all stored images, placements, and pending uploads (e.g. `RIS` full reset).
    pub fn clear_all(&mut self) {
        self.images.clear();
        self.lru_order.clear();
        self.image_numbers.clear();
        self.primary_placements.clear();
        self.alt_placements.clear();
        self.used_bytes = 0;
        #[cfg(feature = "kitty-graphics")]
        {
            self.pending_kitty = None;
        }
    }

    /// Deletes placements matching a predicate on `screen`, optionally freeing unreferenced images.
    pub fn delete_placements_where<F>(&mut self, screen: ScreenKind, free_images: bool, mut pred: F)
    where
        F: FnMut(&ImagePlacement) -> bool,
    {
        self.placements_mut(screen).retain(|p| !pred(p));
        if free_images {
            self.gc_unreferenced();
        }
    }

    fn placements_mut(&mut self, screen: ScreenKind) -> &mut Vec<ImagePlacement> {
        match screen {
            ScreenKind::Primary => &mut self.primary_placements,
            ScreenKind::Alternate => &mut self.alt_placements,
        }
    }

    fn is_referenced(&self, id: u32) -> bool {
        self.primary_placements.iter().any(|p| p.image_id == id)
            || self.alt_placements.iter().any(|p| p.image_id == id)
    }

    /// Removes all images not referenced by any placement on either screen.
    pub fn gc_unreferenced(&mut self) {
        let to_remove: Vec<u32> = self
            .images
            .keys()
            .copied()
            .filter(|&id| !self.is_referenced(id))
            .collect();
        for id in to_remove {
            self.remove_image(id);
        }
    }

    fn gc_unreferenced_if_over(&mut self, threshold: usize) {
        if self.used_bytes > threshold {
            self.gc_unreferenced();
        }
    }
}
