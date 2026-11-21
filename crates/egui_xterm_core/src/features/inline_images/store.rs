//! Storage, LRU memory budgeting, animation playback, and placement management for inline
//! terminal images.

use std::collections::BTreeMap;

use crate::grid::{AbsLine, GridPoint, ScreenKind};

/// Default memory budget for decoded inline images (64 MiB).
pub const DEFAULT_IMAGE_MEMORY_BUDGET: usize = 64 * 1024 * 1024;

/// Maximum pixel dimension (width or height) accepted for a single inline image.
pub const MAX_IMAGE_DIMENSION: u32 = 8192;

/// Maximum depth supported for chains of relative image placements.
pub const MAX_RELATIVE_PLACEMENT_DEPTH: usize = 32;

#[cfg(feature = "kitty-graphics")]
const MAX_ANIMATION_STEPS_PER_TICK: usize = 4096;

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
    /// Raw pixel bytes of the currently active frame (`width * height * 4` for [`ImageFormat::Rgba8`]).
    pub rgba: Vec<u8>,
    /// Monotonic revision bumped on insert, replace, frame edit/compose, or animation frame switch.
    pub revision: u64,
}

impl StoredImage {
    /// Byte size of the active decoded pixel buffer.
    #[must_use]
    pub const fn byte_len(&self) -> usize {
        self.rgba.len()
    }

    /// Returns the RGBA8 pixel bytes for the currently active frame.
    #[must_use]
    pub fn active_rgba(&self) -> &[u8] {
        &self.rgba
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
    /// Whether this is a Kitty virtual placement (`U=1`) used by `U+10EEEE` placeholders.
    pub is_virtual: bool,
    /// Sub-cell horizontal pixel offset (`X=`).
    pub sub_x: u16,
    /// Sub-cell vertical pixel offset (`Y=`).
    pub sub_y: u16,
    /// Parent image ID (`P=`) for relative placements (`0` = none).
    pub parent_image_id: u32,
    /// Parent placement ID (`Q=`) for relative placements (`0` = any/default).
    pub parent_placement_id: u32,
    /// Signed horizontal cell offset (`H=`) relative to the parent placement origin.
    pub offset_cols: i32,
    /// Signed vertical cell offset (`V=`) relative to the parent placement origin.
    pub offset_rows: i32,
}

/// Playback state of an animated Kitty Graphics image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationPlaybackState {
    /// Animation is stopped (`a=a,s=1`).
    #[default]
    Stopped,
    /// Playing forward and pausing at the last frame while waiting for additional frames (`a=a,s=2`).
    Loading,
    /// Playing forward and looping at the end (`a=a,s=3`).
    Playing,
}

/// A single animation frame stored alongside an image.
///
/// For the currently active frame index, `rgba` is kept empty here because its pixel buffer lives
/// in [`StoredImage::rgba`] to avoid duplicate allocations.
#[cfg(feature = "kitty-graphics")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnimationFrame {
    pub rgba: Vec<u8>,
    pub gap_ms: i32,
}

/// Per-image animation timeline and frame store.
#[cfg(feature = "kitty-graphics")]
#[derive(Debug, Clone)]
pub(crate) struct ImageAnimation {
    pub frames: Vec<AnimationFrame>,
    pub current_frame: usize,
    pub state: AnimationPlaybackState,
    pub loop_remaining: Option<u32>,
    pub anchor_secs: Option<f64>,
    pub next_offset_ms: u64,
}

#[cfg(feature = "kitty-graphics")]
impl ImageAnimation {
    fn extra_byte_len(&self) -> usize {
        self.frames.iter().map(|f| f.rgba.len()).sum()
    }

    fn gap_ms_u64(&self, frame_idx: usize) -> u64 {
        let gap_ms = self.frames.get(frame_idx).map_or(0, |f| f.gap_ms);
        if gap_ms <= 0 {
            0
        } else {
            u64::try_from(gap_ms).unwrap_or(0)
        }
    }

    fn deadline_secs(&self) -> Option<f64> {
        self.anchor_secs
            .map(|anchor| anchor + (self.next_offset_ms as f64) / 1000.0)
    }

    fn clear_schedule(&mut self) {
        self.anchor_secs = None;
        self.next_offset_ms = 0;
    }

    fn is_waiting_for_frames(&self) -> bool {
        self.state == AnimationPlaybackState::Loading && self.current_frame + 1 >= self.frames.len()
    }

    fn has_active_timer(&self) -> bool {
        match self.state {
            AnimationPlaybackState::Stopped => false,
            AnimationPlaybackState::Loading => self.current_frame + 1 < self.frames.len(),
            AnimationPlaybackState::Playing => self.frames.len() > 1,
        }
    }
}

/// Key identifying concurrent chunked Kitty Graphics uploads (`m=1` ... `m=0`).
#[cfg(feature = "kitty-graphics")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PendingChunkKey {
    /// Explicit non-zero image ID (`i=`).
    ImageId(u32),
    /// Explicit non-zero image number (`I=`) with `i=0`.
    ImageNumber(u32),
    /// Unidentified sequential chunk transfer (`i=0, I=0`).
    Anonymous,
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
    pub medium: u8,
    pub compressed: bool,
    pub width: u32,
    pub height: u32,
    pub file_size: u64,
    pub file_offset: u64,
    pub cols: u16,
    pub rows: u16,
    pub src_x: u32,
    pub src_y: u32,
    pub src_w: u32,
    pub src_h: u32,
    pub cap_x: u32,
    pub cap_y: u32,
    pub c_param: u32,
    pub r_param: u32,
    pub z_index: i32,
    pub no_cursor_move: bool,
    pub is_virtual: bool,
    pub parent_image_id: u32,
    pub parent_placement_id: u32,
    pub offset_cols: i32,
    pub offset_rows: i32,
    pub quiet: u8,
    pub payload: Vec<u8>,
}

/// Bounded LRU store for decoded inline images, animations, and per-screen placements.
#[derive(Debug, Clone)]
pub struct ImageStore {
    memory_budget: usize,
    used_bytes: usize,
    next_id: u32,
    next_revision: u64,
    images: BTreeMap<u32, StoredImage>,
    #[cfg(feature = "kitty-graphics")]
    animations: BTreeMap<u32, ImageAnimation>,
    lru_order: Vec<u32>,
    image_numbers: BTreeMap<u32, u32>,
    primary_placements: Vec<ImagePlacement>,
    alt_placements: Vec<ImagePlacement>,
    #[cfg(feature = "kitty-graphics")]
    pub(crate) pending_kitty: BTreeMap<PendingChunkKey, PendingKittyUpload>,
    #[cfg(feature = "kitty-graphics")]
    pub(crate) last_pending_key: Option<PendingChunkKey>,
    #[cfg(feature = "kitty-graphics")]
    pub(crate) pending_kitty_bytes: usize,
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
            next_revision: 1,
            images: BTreeMap::new(),
            #[cfg(feature = "kitty-graphics")]
            animations: BTreeMap::new(),
            lru_order: Vec::new(),
            image_numbers: BTreeMap::new(),
            primary_placements: Vec::new(),
            alt_placements: Vec::new(),
            #[cfg(feature = "kitty-graphics")]
            pending_kitty: BTreeMap::new(),
            #[cfg(feature = "kitty-graphics")]
            last_pending_key: None,
            #[cfg(feature = "kitty-graphics")]
            pending_kitty_bytes: 0,
        }
    }

    /// Returns the total byte size of all currently stored images and animation frames.
    #[must_use]
    pub const fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// Returns the configured memory budget in bytes.
    #[must_use]
    pub const fn memory_budget(&self) -> usize {
        self.memory_budget
    }

    pub(crate) fn alloc_revision(&mut self) -> u64 {
        let rev = self.next_revision;
        self.next_revision = self.next_revision.saturating_add(1).max(1);
        rev
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
    pub fn insert_image(&mut self, mut image: StoredImage) -> bool {
        let new_bytes = image.byte_len();
        if new_bytes > self.memory_budget {
            return false;
        }
        let id = image.id;
        if let Some(old) = self.images.remove(&id) {
            self.used_bytes = self.used_bytes.saturating_sub(old.byte_len());
            self.lru_order.retain(|&x| x != id);
        }
        #[cfg(feature = "kitty-graphics")]
        if let Some(anim) = self.animations.remove(&id) {
            self.used_bytes = self.used_bytes.saturating_sub(anim.extra_byte_len());
        }
        while self.used_bytes.saturating_add(new_bytes) > self.memory_budget {
            if let Some(evict_id) = self.lru_order.first().copied() {
                self.remove_image(evict_id);
            } else {
                break;
            }
        }
        image.revision = self.alloc_revision();
        self.used_bytes = self.used_bytes.saturating_add(new_bytes);
        self.images.insert(id, image);
        self.lru_order.push(id);
        true
    }

    /// Removes an image by `id`, its animation frames, and all placements referencing it
    /// (including cascade-deleting any relative child placements).
    pub fn remove_image(&mut self, id: u32) {
        if let Some(old) = self.images.remove(&id) {
            self.used_bytes = self.used_bytes.saturating_sub(old.byte_len());
        }
        #[cfg(feature = "kitty-graphics")]
        if let Some(anim) = self.animations.remove(&id) {
            self.used_bytes = self.used_bytes.saturating_sub(anim.extra_byte_len());
        }
        self.lru_order.retain(|&x| x != id);
        self.image_numbers.retain(|_, &mut v| v != id);
        self.delete_placements_where(ScreenKind::Primary, false, |p| p.image_id == id);
        self.delete_placements_where(ScreenKind::Alternate, false, |p| p.image_id == id);
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
    /// `(image_id, placement_id)` on `screen` is replaced without cascade-deleting its relative
    /// children; instead, relative children are re-resolved to the updated position.
    pub fn add_placement(&mut self, screen: ScreenKind, mut placement: ImagePlacement) {
        self.touch_image(placement.image_id);
        if placement.parent_image_id != 0
            && let Some((line, col)) = Self::resolve_coords_for(self.placements(screen), &placement)
        {
            placement.abs_line = AbsLine(line.max(0) as u64);
            placement.col = u16::try_from(col.clamp(0, i32::from(u16::MAX))).unwrap_or(0);
        }
        let list = self.placements_mut(screen);
        if placement.placement_id != 0
            && let Some(existing) = list.iter_mut().find(|p| {
                p.image_id == placement.image_id && p.placement_id == placement.placement_id
            })
        {
            *existing = placement;
            self.resolve_relative_placements(screen);
            return;
        }
        list.push(placement);
        self.resolve_relative_placements(screen);
    }

    /// Returns all placements for `screen` (including virtual placements).
    #[must_use]
    pub fn placements(&self, screen: ScreenKind) -> &[ImagePlacement] {
        match screen {
            ScreenKind::Primary => &self.primary_placements,
            ScreenKind::Alternate => &self.alt_placements,
        }
    }

    /// Looks up a virtual (`U=1`) placement matching `(image_id, placement_id)` across stored
    /// screens.
    ///
    /// When `placement_id == 0`, returns the most recently registered virtual placement for
    /// `image_id`.
    #[must_use]
    pub fn virtual_placement(&self, image_id: u32, placement_id: u32) -> Option<ImagePlacement> {
        self.virtual_placement_on(ScreenKind::Alternate, image_id, placement_id)
            .or_else(|| self.virtual_placement_on(ScreenKind::Primary, image_id, placement_id))
    }

    /// Looks up a virtual (`U=1`) placement matching `(image_id, placement_id)` on `screen`.
    ///
    /// When `placement_id == 0`, returns the most recently registered virtual placement for
    /// `image_id` on `screen`.
    #[must_use]
    pub fn virtual_placement_on(
        &self,
        screen: ScreenKind,
        image_id: u32,
        placement_id: u32,
    ) -> Option<ImagePlacement> {
        let list = self.placements(screen);
        if placement_id != 0 {
            list.iter()
                .rev()
                .copied()
                .find(|p| p.is_virtual && p.image_id == image_id && p.placement_id == placement_id)
        } else {
            list.iter()
                .rev()
                .copied()
                .find(|p| p.is_virtual && p.image_id == image_id)
        }
    }

    /// Returns non-virtual placements on `screen` that intersect the visible viewport
    /// `[top_abs_line, top_abs_line + rows)`.
    #[must_use]
    pub fn visible_placements(
        &self,
        screen: ScreenKind,
        top_abs_line: AbsLine,
        rows: u16,
    ) -> Vec<ImagePlacement> {
        let view_start = i64::try_from(top_abs_line.0).unwrap_or(i64::MAX);
        let view_end = view_start.saturating_add(i64::from(rows));
        let all = self.placements(screen);
        all.iter()
            .copied()
            .filter_map(|mut p| {
                if p.is_virtual {
                    return None;
                }
                let (p_start, p_col) = if p.parent_image_id != 0 {
                    Self::resolve_coords_for(all, &p)?
                } else {
                    (
                        i64::try_from(p.abs_line.0).unwrap_or(i64::MAX),
                        i32::from(p.col),
                    )
                };
                let p_end = p_start.saturating_add(i64::from(p.rows.max(1)));
                let p_col_end = p_col.saturating_add(i32::from(p.cols.max(1)));
                if p_end > view_start && p_start < view_end && p_col_end > 0 {
                    if p_start >= 0 && p_col >= 0 {
                        p.abs_line = AbsLine(p_start as u64);
                        p.col = u16::try_from(p_col).unwrap_or(u16::MAX);
                    } else if p_start < 0 {
                        // Placement starts above line 0; clamp top while preserving visible tail height.
                        let clipped_top = u16::try_from(-p_start).unwrap_or(u16::MAX);
                        p.abs_line = AbsLine(0);
                        if p.rows > 0 {
                            p.rows = p.rows.saturating_sub(clipped_top);
                            if p.rows == 0 {
                                return None;
                            }
                        }
                        if p_col >= 0 {
                            p.col = u16::try_from(p_col).unwrap_or(u16::MAX);
                        }
                    } else {
                        return None;
                    }
                    Some(p)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Prunes non-virtual root placements on `screen` whose bottom edge is strictly above
    /// `min_abs_line` (evicted from scrollback), cascade-deletes any relative children, and
    /// frees unreferenced images.
    ///
    /// Virtual placements (`is_virtual == true`) are never pruned by scrollback eviction.
    pub fn prune_before(&mut self, screen: ScreenKind, min_abs_line: AbsLine) {
        let before = self.placements(screen).len();
        self.delete_placements_where(screen, false, |p| {
            if p.is_virtual || p.parent_image_id != 0 {
                return false;
            }
            let p_end = p.abs_line.0.saturating_add(u64::from(p.rows.max(1)));
            p_end <= min_abs_line.0
        });
        if self.placements(screen).len() != before {
            self.gc_unreferenced_if_over(0);
        }
    }

    /// Clears all placements on `screen` (e.g., alt-screen reset) and frees images no longer
    /// referenced by either screen.
    pub fn clear_screen(&mut self, screen: ScreenKind) {
        self.placements_mut(screen).clear();
        self.gc_unreferenced();
    }

    /// Clears non-virtual placements on `screen` intersecting the viewport
    /// `[top_abs_line, top_abs_line + rows)` (or all non-virtual placements on `screen` when
    /// `rows == 0`), cascade-deletes relative children, and optionally frees unreferenced images.
    ///
    /// Virtual placements (`is_virtual == true`) and scrollback placements strictly above
    /// `top_abs_line` are preserved.
    pub fn clear_viewport_placements(
        &mut self,
        screen: ScreenKind,
        top_abs_line: AbsLine,
        rows: u16,
        free_images: bool,
    ) {
        if rows == 0 {
            self.delete_placements_where(screen, free_images, |p| !p.is_virtual);
            return;
        }
        let view_start = top_abs_line.0;
        let view_end = view_start.saturating_add(u64::from(rows));
        self.delete_placements_where(screen, free_images, |p| {
            if p.is_virtual {
                return false;
            }
            let p_start = p.abs_line.0;
            let p_end = p_start.saturating_add(u64::from(p.rows.max(1)));
            p_end > view_start && p_start < view_end
        });
    }

    /// Clears non-virtual placements on `screen` whose bottom edge is strictly below
    /// `min_abs_line` (e.g., `ED 2` / `DECCOLM` preserving scrollback images), cascade-deletes
    /// relative children, and optionally frees unreferenced images.
    pub fn clear_placements_from(
        &mut self,
        screen: ScreenKind,
        min_abs_line: AbsLine,
        free_images: bool,
    ) {
        self.delete_placements_where(screen, free_images, |p| {
            if p.is_virtual {
                return false;
            }
            let p_end = p.abs_line.0.saturating_add(u64::from(p.rows.max(1)));
            p_end > min_abs_line.0
        });
    }

    /// Collects [`GridPoint`] anchors for all root non-virtual placements on `screen` so
    /// `Grid::resize_with_selection` can remap them across resize/reflow.
    pub fn collect_reflow_points(&self, screen: ScreenKind, out: &mut Vec<GridPoint>) -> usize {
        let start = out.len();
        for p in self.placements(screen) {
            if !p.is_virtual && p.parent_image_id == 0 {
                out.push(GridPoint::new(p.abs_line, usize::from(p.col)));
            }
        }
        out.len() - start
    }

    /// Applies remapped [`GridPoint`] results from `Grid::resize_with_selection` to the root
    /// non-virtual placements on `screen`.
    ///
    /// Evicted (`None`) placements and their relative children are cascade-deleted, surviving
    /// relative placements are re-resolved, and unreferenced images are freed if `free_images`
    /// is `true`.
    pub fn apply_reflow_points(
        &mut self,
        screen: ScreenKind,
        mapped: &[Option<GridPoint>],
        free_images: bool,
    ) {
        let mut evicted_keys: Vec<(u32, u32)> = Vec::new();
        let mut map_idx = 0usize;
        {
            let list = self.placements_mut(screen);
            let mut keep = vec![true; list.len()];
            for (idx, p) in list.iter_mut().enumerate() {
                if p.is_virtual || p.parent_image_id != 0 {
                    continue;
                }
                let Some(slot) = mapped.get(map_idx) else {
                    break;
                };
                map_idx += 1;
                match slot {
                    Some(pt) => {
                        p.abs_line = pt.line;
                        p.col = u16::try_from(pt.col).unwrap_or(u16::MAX);
                    }
                    None => {
                        keep[idx] = false;
                        evicted_keys.push((p.image_id, p.placement_id));
                    }
                }
            }
            if !evicted_keys.is_empty() {
                let mut i = 0usize;
                list.retain(|_| {
                    let k = keep[i];
                    i += 1;
                    k
                });
            }
        }
        if !evicted_keys.is_empty() {
            self.cascade_delete_children_of(screen, &evicted_keys);
        }
        self.resolve_relative_placements(screen);
        if free_images && !evicted_keys.is_empty() {
            self.gc_unreferenced();
        }
    }

    /// Shifts or clips non-virtual root placements inside `[region_top_abs, region_bottom_abs]`
    /// by `delta_rows` (negative = scrolled up, positive = scrolled down) for non-history scrolls
    /// (`Alternate` screen or `DECSTBM` sub-regions).
    pub fn shift_placements_in_region(
        &mut self,
        screen: ScreenKind,
        region_top_abs: AbsLine,
        region_bottom_abs: AbsLine,
        delta_rows: isize,
        free_images: bool,
    ) {
        if delta_rows == 0 {
            return;
        }
        let top_i = i64::try_from(region_top_abs.0).unwrap_or(i64::MAX);
        let bot_i = i64::try_from(region_bottom_abs.0).unwrap_or(i64::MAX);
        let delta_i = i64::try_from(delta_rows).unwrap_or(0);
        let mut deleted_keys: Vec<(u32, u32)> = Vec::new();
        {
            let list = self.placements_mut(screen);
            let mut keep = vec![true; list.len()];
            for (idx, p) in list.iter_mut().enumerate() {
                if p.is_virtual || p.parent_image_id != 0 {
                    continue;
                }
                let p_start = i64::try_from(p.abs_line.0).unwrap_or(i64::MAX);
                let p_rows = i64::from(p.rows.max(1));
                let p_end = p_start.saturating_add(p_rows);
                // Only shift placements intersecting the scroll region.
                if p_end <= top_i || p_start > bot_i {
                    continue;
                }
                let new_start = p_start.saturating_add(delta_i);
                let new_end = new_start.saturating_add(p_rows);
                if new_end <= top_i || new_start > bot_i {
                    keep[idx] = false;
                    deleted_keys.push((p.image_id, p.placement_id));
                    continue;
                }
                let clamped_start = new_start.max(top_i);
                let clamped_end = new_end.min(bot_i.saturating_add(1));
                let visible_rows = clamped_end.saturating_sub(clamped_start);
                if visible_rows <= 0 {
                    keep[idx] = false;
                    deleted_keys.push((p.image_id, p.placement_id));
                    continue;
                }
                p.abs_line = AbsLine(clamped_start.max(0) as u64);
                if p.rows > 0 {
                    p.rows = u16::try_from(visible_rows).unwrap_or(u16::MAX);
                }
            }
            if !deleted_keys.is_empty() {
                let mut i = 0usize;
                list.retain(|_| {
                    let k = keep[i];
                    i += 1;
                    k
                });
            }
        }
        if !deleted_keys.is_empty() {
            self.cascade_delete_children_of(screen, &deleted_keys);
        }
        self.resolve_relative_placements(screen);
        if free_images && !deleted_keys.is_empty() {
            self.gc_unreferenced();
        }
    }

    /// Clears all stored images, animations, placements, and pending uploads (e.g. `RIS` full reset).
    pub fn clear_all(&mut self) {
        self.images.clear();
        #[cfg(feature = "kitty-graphics")]
        self.animations.clear();
        self.lru_order.clear();
        self.image_numbers.clear();
        self.primary_placements.clear();
        self.alt_placements.clear();
        self.used_bytes = 0;
        #[cfg(feature = "kitty-graphics")]
        {
            self.pending_kitty.clear();
            self.last_pending_key = None;
            self.pending_kitty_bytes = 0;
        }
    }

    /// Deletes placements matching `pred` on `screen`, cascade-deletes all relative child
    /// placements whose ancestors were removed, and optionally frees unreferenced images.
    pub fn delete_placements_where<F>(&mut self, screen: ScreenKind, free_images: bool, mut pred: F)
    where
        F: FnMut(&ImagePlacement) -> bool,
    {
        let mut deleted_keys: Vec<(u32, u32)> = Vec::new();
        {
            let list = self.placements_mut(screen);
            list.retain(|p| {
                if pred(p) {
                    deleted_keys.push((p.image_id, p.placement_id));
                    false
                } else {
                    true
                }
            });
        }
        if !deleted_keys.is_empty() {
            self.cascade_delete_children_of(screen, &deleted_keys);
        }
        if free_images {
            self.gc_unreferenced();
        }
    }

    /// Advances active Kitty image animations up to `now_secs`, updating active frame pixels and
    /// monotonically bumping [`StoredImage::revision`] whenever any displayed frame changes.
    ///
    /// Returns `true` if at least one image switched its visible frame.
    pub fn tick_animations(&mut self, now_secs: f64) -> bool {
        #[cfg(not(feature = "kitty-graphics"))]
        {
            let _unused_now = now_secs;
            false
        }
        #[cfg(feature = "kitty-graphics")]
        {
            const DEADLINE_EPSILON_SECS: f64 = 1e-9;

            if !now_secs.is_finite() || self.animations.is_empty() {
                return false;
            }
            let anim_ids: Vec<u32> = self.animations.keys().copied().collect();
            let mut any_changed = false;
            for id in anim_ids {
                let Some(anim) = self.animations.get_mut(&id) else {
                    continue;
                };
                if !anim.has_active_timer() {
                    anim.clear_schedule();
                    continue;
                }
                let start_frame = anim.current_frame;
                if anim.anchor_secs.is_none() {
                    anim.anchor_secs = Some(now_secs);
                    anim.next_offset_ms = anim.gap_ms_u64(anim.current_frame);
                }
                let mut steps = 0usize;
                while let Some(deadline) = anim.deadline_secs()
                    && now_secs + DEADLINE_EPSILON_SECS >= deadline
                    && steps < MAX_ANIMATION_STEPS_PER_TICK
                {
                    steps += 1;
                    if !Self::advance_one_frame(anim, now_secs) {
                        break;
                    }
                }
                if anim
                    .deadline_secs()
                    .is_some_and(|d| now_secs + DEADLINE_EPSILON_SECS >= d)
                    && anim.has_active_timer()
                {
                    // Prevent unbounded catch-up work if `now_secs` jumped far ahead.
                    anim.anchor_secs = Some(now_secs);
                    anim.next_offset_ms = anim.gap_ms_u64(anim.current_frame);
                }
                let end_frame = anim.current_frame;
                if end_frame != start_frame
                    && let Some(img) = self.images.get_mut(&id)
                {
                    std::mem::swap(&mut img.rgba, &mut anim.frames[start_frame].rgba);
                    std::mem::swap(&mut img.rgba, &mut anim.frames[end_frame].rgba);
                    let rev = self.next_revision;
                    self.next_revision = self.next_revision.saturating_add(1).max(1);
                    img.revision = rev;
                    any_changed = true;
                }
            }
            any_changed
        }
    }

    /// Returns the earliest `now_secs` timestamp at which an active Kitty animation needs to
    /// advance, or `None` if no animations are currently playing.
    #[must_use]
    pub fn next_animation_deadline(&self) -> Option<f64> {
        #[cfg(not(feature = "kitty-graphics"))]
        {
            None
        }
        #[cfg(feature = "kitty-graphics")]
        {
            let mut earliest: Option<f64> = None;
            for anim in self.animations.values() {
                if !anim.has_active_timer() {
                    continue;
                }
                let d = anim.deadline_secs().unwrap_or(0.0);
                earliest = Some(match earliest {
                    Some(prev) => prev.min(d),
                    None => d,
                });
            }
            earliest
        }
    }

    #[cfg(feature = "kitty-graphics")]
    fn advance_one_frame(anim: &mut ImageAnimation, now_secs: f64) -> bool {
        let len = anim.frames.len();
        if len <= 1 {
            anim.clear_schedule();
            return false;
        }
        if anim.current_frame + 1 < len {
            anim.current_frame += 1;
            if anim.is_waiting_for_frames() {
                anim.clear_schedule();
                return false;
            }
            let next_gap = anim.gap_ms_u64(anim.current_frame);
            anim.next_offset_ms = anim.next_offset_ms.saturating_add(next_gap);
            return true;
        }

        // At last frame: either wait (Loading) or loop (Playing).
        if anim.state == AnimationPlaybackState::Loading {
            anim.clear_schedule();
            return false;
        }
        if let Some(rem) = anim.loop_remaining {
            if rem <= 1 {
                anim.state = AnimationPlaybackState::Stopped;
                anim.loop_remaining = None;
                anim.clear_schedule();
                return false;
            }
            anim.loop_remaining = Some(rem - 1);
        }
        anim.current_frame = 0;
        let next_gap = anim.gap_ms_u64(0);
        // Fast-forward full loop periods if unlimited looping and `now_secs` jumped ahead.
        if anim.loop_remaining.is_none()
            && let Some(anchor) = anim.anchor_secs
        {
            let cycle_ms: u64 = (0..len).map(|i| anim.gap_ms_u64(i)).sum();
            let elapsed_ms = ((now_secs - anchor) * 1000.0).max(0.0) as u64;
            if cycle_ms > 0 && elapsed_ms.saturating_sub(anim.next_offset_ms) > cycle_ms {
                let skip = (elapsed_ms - anim.next_offset_ms) / cycle_ms;
                anim.next_offset_ms = anim
                    .next_offset_ms
                    .saturating_add(skip.saturating_mul(cycle_ms));
            }
        }
        anim.next_offset_ms = anim.next_offset_ms.saturating_add(next_gap);
        true
    }

    #[cfg(feature = "kitty-graphics")]
    pub(crate) fn animation_frame_count(&self, id: u32) -> usize {
        if let Some(anim) = self.animations.get(&id) {
            anim.frames.len()
        } else if self.images.contains_key(&id) {
            1
        } else {
            0
        }
    }

    #[cfg(feature = "kitty-graphics")]
    pub(crate) fn ensure_animation_entry(&mut self, id: u32) -> Option<&mut ImageAnimation> {
        if !self.images.contains_key(&id) {
            return None;
        }
        Some(self.animations.entry(id).or_insert_with(|| ImageAnimation {
            frames: vec![AnimationFrame {
                rgba: Vec::new(),
                gap_ms: 0,
            }],
            current_frame: 0,
            state: AnimationPlaybackState::Stopped,
            loop_remaining: None,
            anchor_secs: None,
            next_offset_ms: 0,
        }))
    }

    #[cfg(feature = "kitty-graphics")]
    pub(crate) fn copy_frame_rgba(&self, id: u32, frame_idx: usize) -> Option<Vec<u8>> {
        let img = self.images.get(&id)?;
        if let Some(anim) = self.animations.get(&id) {
            if frame_idx >= anim.frames.len() {
                return None;
            }
            if frame_idx == anim.current_frame {
                Some(img.rgba.clone())
            } else {
                Some(anim.frames[frame_idx].rgba.clone())
            }
        } else if frame_idx == 0 {
            Some(img.rgba.clone())
        } else {
            None
        }
    }

    #[cfg(feature = "kitty-graphics")]
    pub(crate) fn try_reserve_frame_bytes(&mut self, id: u32, additional: usize) -> bool {
        if additional == 0 {
            return true;
        }
        let img_bytes = self.images.get(&id).map_or(0, StoredImage::byte_len);
        let anim_bytes = self
            .animations
            .get(&id)
            .map_or(0, ImageAnimation::extra_byte_len);
        if img_bytes
            .saturating_add(anim_bytes)
            .saturating_add(additional)
            > self.memory_budget
        {
            return false;
        }
        while self.used_bytes.saturating_add(additional) > self.memory_budget {
            let Some(evict_id) = self.lru_order.iter().copied().find(|&x| x != id) else {
                return false;
            };
            self.remove_image(evict_id);
        }
        self.used_bytes = self.used_bytes.saturating_add(additional);
        true
    }

    #[cfg(feature = "kitty-graphics")]
    pub(crate) fn append_animation_frame(
        &mut self,
        id: u32,
        rgba: Vec<u8>,
        gap_ms: i32,
    ) -> Result<usize, &'static str> {
        let byte_len = rgba.len();
        if !self.try_reserve_frame_bytes(id, byte_len) {
            return Err("ENOSPC:animation frame exceeds memory budget");
        }
        let Some(anim) = self.ensure_animation_entry(id) else {
            self.used_bytes = self.used_bytes.saturating_sub(byte_len);
            return Err("ENOENT:image not found");
        };
        let was_waiting = anim.is_waiting_for_frames();
        anim.frames.push(AnimationFrame { rgba, gap_ms });
        if was_waiting {
            anim.clear_schedule();
        }
        Ok(anim.frames.len())
    }

    #[cfg(feature = "kitty-graphics")]
    pub(crate) fn with_frame_rgba_mut<F, R>(
        &mut self,
        id: u32,
        frame_idx: usize,
        f: F,
    ) -> Option<(R, bool)>
    where
        F: FnOnce(&mut Vec<u8>) -> R,
    {
        if !self.images.contains_key(&id) {
            return None;
        }
        let current = self.animations.get(&id).map_or(0, |a| a.current_frame);
        let res = if frame_idx == current {
            let img = self.images.get_mut(&id)?;
            f(&mut img.rgba)
        } else {
            let anim = self.animations.get_mut(&id)?;
            let frame = anim.frames.get_mut(frame_idx)?;
            f(&mut frame.rgba)
        };
        let is_active = frame_idx == current;
        if is_active {
            let rev = self.alloc_revision();
            if let Some(img) = self.images.get_mut(&id) {
                img.revision = rev;
            }
        }
        Some((res, is_active))
    }

    #[cfg(feature = "kitty-graphics")]
    pub(crate) fn set_active_frame(&mut self, id: u32, target_frame_idx: usize) -> bool {
        let Some(anim) = self.animations.get_mut(&id) else {
            return target_frame_idx == 0 && self.images.contains_key(&id);
        };
        if target_frame_idx >= anim.frames.len() {
            return false;
        }
        let old = anim.current_frame;
        if old == target_frame_idx {
            return true;
        }
        if let Some(img) = self.images.get_mut(&id) {
            std::mem::swap(&mut img.rgba, &mut anim.frames[old].rgba);
            std::mem::swap(&mut img.rgba, &mut anim.frames[target_frame_idx].rgba);
            let rev = self.next_revision;
            self.next_revision = self.next_revision.saturating_add(1).max(1);
            img.revision = rev;
        }
        anim.current_frame = target_frame_idx;
        anim.clear_schedule();
        true
    }

    #[cfg(feature = "kitty-graphics")]
    pub(crate) fn set_animation_params(
        &mut self,
        id: u32,
        state: Option<AnimationPlaybackState>,
        loop_count: u32,
        target_gap_frame: Option<(usize, i32)>,
    ) -> Result<(), &'static str> {
        let Some(anim) = self.ensure_animation_entry(id) else {
            return Err("ENOENT:image not found");
        };
        if let Some((frame_idx, gap_ms)) = target_gap_frame {
            let Some(frame) = anim.frames.get_mut(frame_idx) else {
                return Err("ENOENT:frame not found");
            };
            if gap_ms != 0 {
                frame.gap_ms = gap_ms;
            }
        }
        if loop_count == 1 {
            anim.loop_remaining = None;
        } else if loop_count > 1 {
            anim.loop_remaining = Some(loop_count - 1);
        }
        if let Some(st) = state {
            anim.state = st;
            match st {
                AnimationPlaybackState::Stopped => {
                    anim.loop_remaining = None;
                    anim.clear_schedule();
                }
                AnimationPlaybackState::Loading | AnimationPlaybackState::Playing => {
                    anim.clear_schedule();
                }
            }
        }
        Ok(())
    }

    #[cfg(feature = "kitty-graphics")]
    pub(crate) fn delete_animation_frames(&mut self, id: u32, target_frame_1based: u32) -> bool {
        if !self.images.contains_key(&id) {
            return false;
        }
        let Some(mut anim) = self.animations.remove(&id) else {
            return false;
        };
        if target_frame_1based == 0 {
            if anim.frames.len() <= 1 {
                self.animations.insert(id, anim);
                return false;
            }
            if anim.current_frame != 0
                && let Some(img) = self.images.get_mut(&id)
            {
                std::mem::swap(&mut img.rgba, &mut anim.frames[anim.current_frame].rgba);
                std::mem::swap(&mut img.rgba, &mut anim.frames[0].rgba);
                let rev = self.alloc_revision();
                if let Some(img_mut) = self.images.get_mut(&id) {
                    img_mut.revision = rev;
                }
            }
            let freed: usize = anim.frames.drain(1..).map(|f| f.rgba.len()).sum();
            self.used_bytes = self.used_bytes.saturating_sub(freed);
            return true;
        }

        let idx = usize::try_from(target_frame_1based - 1).unwrap_or(usize::MAX);
        if idx >= anim.frames.len() {
            self.animations.insert(id, anim);
            return false;
        }
        if anim.frames.len() <= 1 {
            self.animations.insert(id, anim);
            return false;
        }
        let mut active_changed = false;
        if idx == anim.current_frame {
            let next_active = if idx == 0 { 1 } else { 0 };
            if let Some(img) = self.images.get_mut(&id) {
                std::mem::swap(&mut img.rgba, &mut anim.frames[idx].rgba);
                std::mem::swap(&mut img.rgba, &mut anim.frames[next_active].rgba);
                active_changed = true;
            }
            anim.current_frame = next_active;
        }
        let removed = anim.frames.remove(idx);
        self.used_bytes = self.used_bytes.saturating_sub(removed.rgba.len());
        if idx < anim.current_frame {
            anim.current_frame -= 1;
        }
        if anim.frames.len() > 1 {
            self.animations.insert(id, anim);
        }
        if active_changed {
            let rev = self.alloc_revision();
            if let Some(img) = self.images.get_mut(&id) {
                img.revision = rev;
            }
        }
        true
    }

    fn placements_mut(&mut self, screen: ScreenKind) -> &mut Vec<ImagePlacement> {
        match screen {
            ScreenKind::Primary => &mut self.primary_placements,
            ScreenKind::Alternate => &mut self.alt_placements,
        }
    }

    fn cascade_delete_children_of(&mut self, screen: ScreenKind, initial_deleted: &[(u32, u32)]) {
        let mut deleted: Vec<(u32, u32)> = initial_deleted.to_vec();
        let mut cursor = 0usize;
        while cursor < deleted.len() {
            let batch_end = deleted.len();
            let list = self.placements_mut(screen);
            let mut newly_deleted = Vec::new();
            let mut idx = 0usize;
            while idx < list.len() {
                let p = list[idx];
                if p.parent_image_id != 0
                    && deleted[cursor..batch_end].iter().any(|&(d_img, d_pid)| {
                        p.parent_image_id == d_img
                            && (p.parent_placement_id == d_pid
                                || (p.parent_placement_id == 0
                                    && (d_pid == 0
                                        || !list.iter().any(|other| {
                                            other.image_id == d_img
                                                && !(other.image_id == p.image_id
                                                    && other.placement_id == p.placement_id
                                                    && p.placement_id != 0)
                                        }))))
                    })
                {
                    let removed = list.remove(idx);
                    newly_deleted.push((removed.image_id, removed.placement_id));
                } else {
                    idx += 1;
                }
            }
            cursor = batch_end;
            deleted.extend(newly_deleted);
        }
    }

    fn resolve_relative_placements(&mut self, screen: ScreenKind) {
        let snapshot = self.placements(screen).to_vec();
        let list = self.placements_mut(screen);
        for p in list.iter_mut() {
            if p.parent_image_id == 0 {
                continue;
            }
            if let Some((line, col)) = Self::resolve_coords_for(&snapshot, p) {
                p.abs_line = AbsLine(line.max(0) as u64);
                p.col = u16::try_from(col.clamp(0, i32::from(u16::MAX))).unwrap_or(0);
            }
        }
    }

    fn resolve_coords_for(all: &[ImagePlacement], target: &ImagePlacement) -> Option<(i64, i32)> {
        let mut cur = *target;
        let mut sum_rows: i64 = 0;
        let mut sum_cols: i32 = 0;
        for _ in 0..=MAX_RELATIVE_PLACEMENT_DEPTH {
            if cur.parent_image_id == 0 {
                let base_line = i64::try_from(cur.abs_line.0).unwrap_or(i64::MAX);
                let base_col = i32::from(cur.col);
                return Some((
                    base_line.saturating_add(sum_rows),
                    base_col.saturating_add(sum_cols),
                ));
            }
            sum_rows = sum_rows.saturating_add(i64::from(cur.offset_rows));
            sum_cols = sum_cols.saturating_add(cur.offset_cols);
            let parent =
                Self::find_parent_in_slice(all, cur.parent_image_id, cur.parent_placement_id)?;
            cur = *parent;
        }
        None
    }

    pub(crate) fn find_parent_in_slice(
        all: &[ImagePlacement],
        parent_image_id: u32,
        parent_placement_id: u32,
    ) -> Option<&ImagePlacement> {
        if parent_placement_id != 0 {
            all.iter()
                .rev()
                .find(|p| p.image_id == parent_image_id && p.placement_id == parent_placement_id)
        } else {
            all.iter()
                .rev()
                .find(|p| p.image_id == parent_image_id && p.placement_id == 0)
                .or_else(|| all.iter().rev().find(|p| p.image_id == parent_image_id))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_image(id: u32, bytes: usize) -> StoredImage {
        StoredImage {
            id,
            width: 1,
            height: 1,
            format: ImageFormat::Rgba8,
            rgba: vec![u8::try_from(id & 0xFF).unwrap_or(0); bytes],
            revision: 0,
        }
    }

    fn mk_placement(image_id: u32, placement_id: u32, line: u64, col: u16) -> ImagePlacement {
        ImagePlacement {
            image_id,
            placement_id,
            abs_line: AbsLine(line),
            col,
            cols: 2,
            rows: 2,
            src_x: 0,
            src_y: 0,
            src_w: 0,
            src_h: 0,
            z_index: 0,
            is_virtual: false,
            sub_x: 0,
            sub_y: 0,
            parent_image_id: 0,
            parent_placement_id: 0,
            offset_cols: 0,
            offset_rows: 0,
        }
    }

    #[test]
    fn revision_increments_monotonically_and_active_rgba_works() {
        let mut store = ImageStore::new(1024);
        assert!(store.insert_image(mk_image(7, 4)));
        let r1 = store.get_image(7).unwrap().revision;
        assert!(r1 >= 1);
        assert_eq!(store.get_image(7).unwrap().active_rgba(), &[7, 7, 7, 7]);

        assert!(store.insert_image(mk_image(7, 4)));
        let r2 = store.get_image(7).unwrap().revision;
        assert!(r2 > r1);
    }

    #[test]
    fn virtual_placements_not_visible_and_not_pruned_by_scrollback() {
        let mut store = ImageStore::new(1024);
        assert!(store.insert_image(mk_image(1, 4)));
        let mut vp = mk_placement(1, 5, 0, 0);
        vp.is_virtual = true;
        store.add_placement(ScreenKind::Primary, vp);

        assert!(
            store
                .visible_placements(ScreenKind::Primary, AbsLine(0), 24)
                .is_empty()
        );
        assert_eq!(store.virtual_placement(1, 5).unwrap().placement_id, 5);
        assert_eq!(store.virtual_placement(1, 0).unwrap().placement_id, 5);

        store.prune_before(ScreenKind::Primary, AbsLine(100));
        assert!(store.virtual_placement(1, 5).is_some());
        assert!(store.get_image(1).is_some());
    }

    #[test]
    fn relative_placements_cascade_delete_shift_and_reflow() {
        let mut store = ImageStore::new(1024);
        assert!(store.insert_image(mk_image(1, 4)));
        assert!(store.insert_image(mk_image(2, 4)));
        assert!(store.insert_image(mk_image(3, 4)));

        store.add_placement(ScreenKind::Primary, mk_placement(1, 1, 10, 4));
        let mut child = mk_placement(2, 2, 0, 0);
        child.parent_image_id = 1;
        child.parent_placement_id = 1;
        child.offset_cols = 3;
        child.offset_rows = 2;
        store.add_placement(ScreenKind::Primary, child);

        let mut grandchild = mk_placement(3, 3, 0, 0);
        grandchild.parent_image_id = 2;
        grandchild.parent_placement_id = 2;
        grandchild.offset_cols = -1;
        grandchild.offset_rows = 1;
        store.add_placement(ScreenKind::Primary, grandchild);

        let vis = store.visible_placements(ScreenKind::Primary, AbsLine(0), 30);
        assert_eq!(vis.len(), 3);
        assert_eq!((vis[1].abs_line, vis[1].col), (AbsLine(12), 7));
        assert_eq!((vis[2].abs_line, vis[2].col), (AbsLine(13), 6));

        // Reflow remaps root parent -> children move along.
        let mut pts = Vec::new();
        assert_eq!(
            store.collect_reflow_points(ScreenKind::Primary, &mut pts),
            1
        );
        assert_eq!(pts[0], GridPoint::new(AbsLine(10), 4));
        store.apply_reflow_points(
            ScreenKind::Primary,
            &[Some(GridPoint::new(AbsLine(15), 2))],
            true,
        );
        let vis2 = store.visible_placements(ScreenKind::Primary, AbsLine(0), 30);
        assert_eq!((vis2[1].abs_line, vis2[1].col), (AbsLine(17), 5));
        assert_eq!((vis2[2].abs_line, vis2[2].col), (AbsLine(18), 4));

        // Scroll shift inside region shifts parent and children.
        store.shift_placements_in_region(ScreenKind::Primary, AbsLine(10), AbsLine(25), -2, false);
        let vis3 = store.visible_placements(ScreenKind::Primary, AbsLine(0), 30);
        assert_eq!((vis3[0].abs_line, vis3[0].col), (AbsLine(13), 2));
        assert_eq!((vis3[1].abs_line, vis3[1].col), (AbsLine(15), 5));
        assert_eq!((vis3[2].abs_line, vis3[2].col), (AbsLine(16), 4));

        // Deleting parent cascades to child and grandchild.
        store.delete_placements_where(ScreenKind::Primary, true, |p| p.image_id == 1);
        assert!(store.placements(ScreenKind::Primary).is_empty());
        assert!(store.get_image(1).is_none());
        assert!(store.get_image(2).is_none());
        assert!(store.get_image(3).is_none());
    }

    #[test]
    fn clear_viewport_placements_preserves_scrollback_and_virtual() {
        let mut store = ImageStore::new(1024);
        assert!(store.insert_image(mk_image(1, 4)));
        assert!(store.insert_image(mk_image(2, 4)));
        assert!(store.insert_image(mk_image(3, 4)));

        store.add_placement(ScreenKind::Primary, mk_placement(1, 1, 2, 0));
        store.add_placement(ScreenKind::Primary, mk_placement(2, 1, 12, 0));
        let mut vp = mk_placement(3, 1, 12, 0);
        vp.is_virtual = true;
        store.add_placement(ScreenKind::Primary, vp);

        store.clear_viewport_placements(ScreenKind::Primary, AbsLine(10), 24, false);
        assert_eq!(store.placements(ScreenKind::Primary).len(), 2);
        assert!(store.get_image(2).is_some());
        assert!(store.virtual_placement(3, 1).is_some());

        // Re-add placement on line 12 and clear from line 10 with free_images=true.
        store.add_placement(ScreenKind::Primary, mk_placement(2, 1, 12, 0));
        store.clear_placements_from(ScreenKind::Primary, AbsLine(10), true);
        assert_eq!(store.placements(ScreenKind::Primary).len(), 2);
        assert!(store.get_image(1).is_some());
        assert!(store.get_image(2).is_none());
        assert!(store.virtual_placement(3, 1).is_some());
    }

    #[test]
    fn shift_placements_clips_and_prunes_out_of_region() {
        let mut store = ImageStore::new(1024);
        assert!(store.insert_image(mk_image(1, 4)));
        let mut p = mk_placement(1, 1, 10, 0);
        p.rows = 4; // covers 10..14
        store.add_placement(ScreenKind::Alternate, p);

        // Shift up by 2 inside region 10..15 -> clips top 2 rows to 10..12.
        store.shift_placements_in_region(ScreenKind::Alternate, AbsLine(10), AbsLine(15), -2, true);
        let list = store.placements(ScreenKind::Alternate);
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].abs_line, list[0].rows), (AbsLine(10), 2));

        // Shift up by 2 again -> completely exits region -> removed and image GC'd.
        store.shift_placements_in_region(ScreenKind::Alternate, AbsLine(10), AbsLine(15), -2, true);
        assert!(store.placements(ScreenKind::Alternate).is_empty());
        assert!(store.get_image(1).is_none());
    }
}
