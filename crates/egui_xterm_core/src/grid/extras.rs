//! Per-grid sparse `CellExtra` free-list slab with deterministic reclamation.

use crate::cell::{Cell, CellExtra};

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
enum ExtraSlot {
    Occupied(CellExtra),
    Free { next_free: Option<u32> },
}

/// Sparse per-grid table storing [`CellExtra`] entries indexed by 1-based `extra_id`.
///
/// Every non-zero [`Cell::extra_id`] in a [`Grid`](super::Grid) is uniquely owned by a single
/// grid cell, enabling O(1) lookup, zero-cost row movement during scrolling and reflow, and
/// deterministic reclamation without tracing GC.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CellExtraTable {
    slots: Vec<ExtraSlot>,
    free_head: Option<u32>,
    live_count: usize,
}

impl CellExtraTable {
    /// Creates an empty [`CellExtraTable`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_head: None,
            live_count: 0,
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

    /// Inserts `extra` into the slab and returns its 1-based `extra_id`.
    ///
    /// # Panics
    ///
    /// Panics if more than `u32::MAX` distinct extra entries are simultaneously live.
    pub fn insert(&mut self, extra: CellExtra) -> u32 {
        self.live_count += 1;
        if let Some(slot_idx) = self.free_head {
            let slot = &mut self.slots[slot_idx as usize];
            let next = match *slot {
                ExtraSlot::Free { next_free } => next_free,
                ExtraSlot::Occupied(_) => unreachable!("free list pointed to occupied slot"),
            };
            *slot = ExtraSlot::Occupied(extra);
            self.free_head = next;
            return slot_idx + 1;
        }

        let raw =
            u32::try_from(self.slots.len()).expect("cell extra table exceeded u32::MAX entries");
        self.slots.push(ExtraSlot::Occupied(extra));
        raw + 1
    }

    /// Looks up the [`CellExtra`] for a 1-based `extra_id` (`0` always returns `None`).
    #[must_use]
    pub fn get(&self, extra_id: u32) -> Option<&CellExtra> {
        let idx = extra_id.checked_sub(1)? as usize;
        match self.slots.get(idx) {
            Some(ExtraSlot::Occupied(extra)) => Some(extra),
            _ => None,
        }
    }

    /// Looks up a mutable reference to the [`CellExtra`] for a 1-based `extra_id`.
    pub fn get_mut(&mut self, extra_id: u32) -> Option<&mut CellExtra> {
        let idx = extra_id.checked_sub(1)? as usize;
        match self.slots.get_mut(idx) {
            Some(ExtraSlot::Occupied(extra)) => Some(extra),
            _ => None,
        }
    }

    /// Ensures `cell` has a valid [`CellExtra`] slot and returns a mutable reference to it.
    ///
    /// # Panics
    ///
    /// Panics if more than `u32::MAX` distinct extra entries are simultaneously live.
    pub fn ensure_for_cell(&mut self, cell: &mut Cell) -> &mut CellExtra {
        if cell.extra_id == 0 || self.get(cell.extra_id).is_none() {
            cell.extra_id = self.insert(CellExtra::default());
        }
        self.get_mut(cell.extra_id)
            .expect("just inserted extra slot must exist")
    }

    /// Frees `extra_id` back to the free list (no-op for `0` or already-freed slots).
    pub fn free(&mut self, extra_id: u32) {
        let Some(slot_idx) = extra_id.checked_sub(1) else {
            return;
        };
        let Some(slot) = self.slots.get_mut(slot_idx as usize) else {
            return;
        };
        if matches!(slot, ExtraSlot::Occupied(_)) {
            *slot = ExtraSlot::Free {
                next_free: self.free_head,
            };
            self.free_head = Some(slot_idx);
            self.live_count = self.live_count.saturating_sub(1);

            if self.live_count == 0 {
                self.slots.clear();
                self.free_head = None;
                if self.slots.capacity() > 1024 {
                    self.slots.shrink_to(256);
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

    /// Clears all entries and resets the free list.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free_head = None;
        self.live_count = 0;
        if self.slots.capacity() > 1024 {
            self.slots.shrink_to(256);
        }
    }
}
