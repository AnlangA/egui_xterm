//! Per-grid sparse `CellExtra` free-list slab with deterministic reclamation.

use crate::cell::{Cell, CellExtra};

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
struct ExtraSlot {
    occupied: bool,
    next_free: Option<u32>,
    extra: CellExtra,
}

fn assign_extra(dst: &mut CellExtra, mut src: CellExtra) {
    if src.combining.is_empty() {
        dst.combining.clear();
    } else if dst.combining.capacity() >= src.combining.len() {
        dst.combining.clear();
        dst.combining.extend_from_slice(&src.combining);
    } else {
        dst.combining = std::mem::take(&mut src.combining);
    }
    dst.underline_color = src.underline_color;
    dst.hyperlink_id = src.hyperlink_id;
    dst.text_sizing = src.text_sizing;
}

/// Sparse per-grid table storing [`CellExtra`] entries indexed by 1-based `extra_id`.
///
/// Every non-zero [`Cell::extra_id`] in a [`Grid`](super::Grid) is uniquely owned by a single
/// grid cell, enabling O(1) lookup, zero-cost row movement during scrolling and reflow, and
/// deterministic reclamation without tracing GC.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CellExtraTable {
    slots: Vec<ExtraSlot>,
    free_head: Option<u32>,
    live_count: usize,
    #[cfg_attr(feature = "serde", serde(default))]
    hyperlink_extra_count: usize,
    #[cfg_attr(feature = "serde", serde(skip))]
    hyperlink_count_dirty: bool,
}

impl PartialEq for CellExtraTable {
    fn eq(&self, other: &Self) -> bool {
        self.slots == other.slots
            && self.free_head == other.free_head
            && self.live_count == other.live_count
            && self.hyperlink_extra_count == other.hyperlink_extra_count
    }
}

impl Eq for CellExtraTable {}

impl CellExtraTable {
    /// Creates an empty [`CellExtraTable`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_head: None,
            live_count: 0,
            hyperlink_extra_count: 0,
            hyperlink_count_dirty: false,
        }
    }

    /// Returns the number of currently live [`CellExtra`] entries.
    #[must_use]
    pub const fn live_count(&self) -> usize {
        self.live_count
    }

    /// Returns `true` if there are no live [`CellExtra`] entries.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.live_count == 0
    }

    /// Inserts `extra` into the slab and returns its 1-based `extra_id` (or `0` if full).
    pub fn insert(&mut self, extra: CellExtra) -> u32 {
        self.insert_with(|dst| assign_extra(dst, extra))
    }

    /// Allocates or reuses a slot in place, populating its [`CellExtra`] via `f`, and returns
    /// its 1-based `extra_id` (or `0` if full).
    pub fn insert_with<F: FnOnce(&mut CellExtra)>(&mut self, f: F) -> u32 {
        while let Some(slot_idx) = self.free_head {
            if let Some(slot) = self.slots.get_mut(slot_idx as usize)
                && !slot.occupied
            {
                self.free_head = slot.next_free;
                slot.occupied = true;
                slot.next_free = None;
                slot.extra.clear();
                f(&mut slot.extra);
                if slot.extra.hyperlink_id.is_some() {
                    self.hyperlink_extra_count += 1;
                }
                self.live_count += 1;
                return slot_idx + 1;
            }
            self.free_head = None;
        }

        let Ok(raw) = u32::try_from(self.slots.len()) else {
            return 0;
        };
        let Some(id) = raw.checked_add(1) else {
            return 0;
        };
        let mut extra = CellExtra::default();
        f(&mut extra);
        if extra.hyperlink_id.is_some() {
            self.hyperlink_extra_count += 1;
        }
        self.slots.push(ExtraSlot {
            occupied: true,
            next_free: None,
            extra,
        });
        self.live_count += 1;
        id
    }

    /// Looks up the [`CellExtra`] for a 1-based `extra_id` (`0` always returns `None`).
    #[must_use]
    pub fn get(&self, extra_id: u32) -> Option<&CellExtra> {
        let idx = extra_id.checked_sub(1)? as usize;
        self.slots
            .get(idx)
            .and_then(|slot| slot.occupied.then_some(&slot.extra))
    }

    /// Looks up a mutable reference to the [`CellExtra`] for a 1-based `extra_id`.
    pub fn get_mut(&mut self, extra_id: u32) -> Option<&mut CellExtra> {
        self.hyperlink_count_dirty = true;
        let idx = extra_id.checked_sub(1)? as usize;
        self.slots
            .get_mut(idx)
            .and_then(|slot| slot.occupied.then_some(&mut slot.extra))
    }

    /// Ensures `cell` has a valid [`CellExtra`] slot and returns a mutable reference to it.
    pub fn ensure_for_cell(&mut self, cell: &mut Cell) -> Option<&mut CellExtra> {
        if cell.extra_id == 0 || self.get(cell.extra_id).is_none() {
            cell.extra_id = self.insert_with(|_| {});
        }
        self.get_mut(cell.extra_id)
    }

    /// Appends `ch` to `cell`'s combining buffer, reusing or allocating its [`CellExtra`] slot
    /// without marking hyperlink tracking dirty.
    pub(crate) fn push_combining_for_cell(&mut self, cell: &mut Cell, ch: char) -> bool {
        if let Some(idx) = cell.extra_id.checked_sub(1)
            && let Some(slot) = self.slots.get_mut(idx as usize)
            && slot.occupied
        {
            slot.extra.push_combining(ch);
            return true;
        }
        let id = self.insert_with(|ext| ext.push_combining(ch));
        if id == 0 {
            return false;
        }
        cell.extra_id = id;
        true
    }

    /// Clears combining marks, hyperlink, and text sizing on `cell` while preserving
    /// `underline_color`, freeing the slot if it becomes empty.
    pub(crate) fn clear_non_underline_for_selective_erase(&mut self, cell: &mut Cell) {
        let Some(idx) = cell.extra_id.checked_sub(1) else {
            return;
        };
        let Some(slot) = self.slots.get_mut(idx as usize) else {
            cell.extra_id = 0;
            return;
        };
        if !slot.occupied {
            cell.extra_id = 0;
            return;
        }
        slot.extra.combining.clear();
        slot.extra.text_sizing = None;
        if slot.extra.hyperlink_id.take().is_some() {
            self.hyperlink_extra_count = self.hyperlink_extra_count.saturating_sub(1);
        }
        if slot.extra.is_empty() {
            let id = cell.extra_id;
            cell.extra_id = 0;
            self.free(id);
        }
    }

    /// Frees `extra_id` back to the free list (no-op for `0` or already-freed slots).
    pub fn free(&mut self, extra_id: u32) {
        let Some(slot_idx) = extra_id.checked_sub(1) else {
            return;
        };
        let Some(slot) = self.slots.get_mut(slot_idx as usize) else {
            return;
        };
        if slot.occupied {
            if slot.extra.hyperlink_id.is_some() {
                self.hyperlink_extra_count = self.hyperlink_extra_count.saturating_sub(1);
            }
            slot.occupied = false;
            slot.next_free = self.free_head;
            slot.extra.clear();
            self.free_head = Some(slot_idx);
            self.live_count = self.live_count.saturating_sub(1);

            if self.live_count == 0 {
                self.hyperlink_extra_count = 0;
                self.hyperlink_count_dirty = false;
                if self.slots.len() > 256 {
                    self.slots.clear();
                    self.free_head = None;
                    if self.slots.capacity() > 1024 {
                        self.slots.shrink_to(256);
                    }
                }
            }
        }
    }

    /// Frees `cell.extra_id` if the referenced [`CellExtra`] is empty, resetting `cell.extra_id = 0`.
    pub fn prune_if_empty(&mut self, cell: &mut Cell) {
        if cell.extra_id != 0 && self.get(cell.extra_id).is_some_and(CellExtra::is_empty) {
            let id = cell.extra_id;
            cell.extra_id = 0;
            self.free(id);
        }
    }

    /// Clones the [`CellExtra`] at `extra_id` into a newly allocated slot (`0` yields `0`).
    pub fn duplicate(&mut self, extra_id: u32) -> u32 {
        let Some(cloned) = self.get(extra_id).cloned() else {
            return 0;
        };
        self.insert(cloned)
    }

    /// Marks every live [`HyperlinkId`](crate::cell::HyperlinkId) index referenced by occupied
    /// slots into `live_bits`. Returns in O(1) when no hyperlink extras are present.
    #[cfg(any(test, feature = "hyperlinks"))]
    pub(crate) fn collect_live_hyperlink_ids(&self, live_bits: &mut [u64]) {
        if (self.hyperlink_extra_count == 0 && !self.hyperlink_count_dirty) || live_bits.is_empty()
        {
            return;
        }
        for slot in &self.slots {
            if slot.occupied
                && let Some(hid) = slot.extra.hyperlink_id
            {
                let idx = hid.index();
                if let Some(word) = live_bits.get_mut(idx / 64) {
                    *word |= 1_u64 << (idx % 64);
                }
            }
        }
    }

    /// Clears all entries and resets the free list.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free_head = None;
        self.live_count = 0;
        self.hyperlink_extra_count = 0;
        self.hyperlink_count_dirty = false;
        if self.slots.capacity() > 1024 {
            self.slots.shrink_to(256);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::{HyperlinkPool, TextSizing};
    use crate::color::PackedColor;

    #[test]
    fn extra_slot_retains_combining_capacity_across_free_reuse() {
        let mut table = CellExtraTable::new();
        let id1 = table.insert_with(|ext| {
            ext.combining.reserve(32);
            ext.combining.push('\u{0301}');
            ext.underline_color = Some(PackedColor::rgb(10, 20, 30));
        });
        let cap1 = table.get(id1).unwrap().combining.capacity();
        assert!(cap1 >= 32);

        table.free(id1);
        assert_eq!(table.live_count(), 0);

        let id2 = table.insert_with(|ext| {
            ext.combining.push('\u{0302}');
        });
        assert_eq!(id2, id1);
        let reused = table.get(id2).unwrap();
        assert_eq!(reused.combining, vec!['\u{0302}']);
        assert_eq!(reused.underline_color, None);
        assert_eq!(reused.combining.capacity(), cap1);
    }

    #[test]
    fn extra_table_hyperlink_bitset_collection_and_selective_erase() {
        let mut pool = HyperlinkPool::new();
        let h0 = pool.try_intern(None, "https://a.test").unwrap();
        let h1 = pool.try_intern(None, "https://b.test").unwrap();

        let mut table = CellExtraTable::new();
        let mut bits = vec![0_u64; 1];
        table.collect_live_hyperlink_ids(&mut bits);
        assert_eq!(bits[0], 0);

        let e0 = table.insert(CellExtra {
            combining: vec!['\u{0301}'],
            underline_color: Some(PackedColor::rgb(1, 2, 3)),
            hyperlink_id: Some(h0),
            text_sizing: Some(TextSizing::default()),
        });
        let e1 = table.insert_with(|ext| {
            ext.hyperlink_id = Some(h1);
        });

        bits.fill(0);
        table.collect_live_hyperlink_ids(&mut bits);
        assert_eq!(bits[0], (1 << h0.index()) | (1 << h1.index()));

        let mut cell0 = Cell::BLANK;
        cell0.extra_id = e0;
        table.clear_non_underline_for_selective_erase(&mut cell0);
        assert_eq!(cell0.extra_id, e0);
        let remaining = table.get(e0).unwrap();
        assert!(remaining.combining.is_empty());
        assert_eq!(remaining.text_sizing, None);
        assert_eq!(remaining.hyperlink_id, None);
        assert_eq!(remaining.underline_color, Some(PackedColor::rgb(1, 2, 3)));

        let mut cell1 = Cell::BLANK;
        cell1.extra_id = e1;
        table.clear_non_underline_for_selective_erase(&mut cell1);
        assert_eq!(cell1.extra_id, 0);
        assert_eq!(table.live_count(), 1);

        bits.fill(0);
        table.collect_live_hyperlink_ids(&mut bits);
        assert_eq!(bits[0], 0);
    }
}
