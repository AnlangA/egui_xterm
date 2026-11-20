//! Bounded CSI/DCS parameter storage supporting `;` parameters and `:` subparameters.

use std::iter::FusedIterator;

/// Maximum total `u16` parameter and subparameter slots retained per CSI/DCS sequence.
pub const MAX_CSI_SLOTS: usize = 64;

/// Maximum intermediate/private-marker bytes retained per sequence.
pub const MAX_INTERMEDIATES: usize = 4;

/// Default maximum `OSC` payload buffer size (4 MiB).
pub const DEFAULT_MAX_OSC_BYTES: usize = 4 * 1024 * 1024;

/// Zero-allocation bounded parameter list supporting both `;` parameters and `:` subparameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsiParams {
    values: [u16; MAX_CSI_SLOTS],
    starts: [u8; MAX_CSI_SLOTS + 1],
    param_count: u8,
    current_slot: u8,
    in_param: bool,
    truncated: bool,
}

impl Default for CsiParams {
    fn default() -> Self {
        Self::new()
    }
}

impl CsiParams {
    /// Creates an empty [`CsiParams`] buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            values: [0; MAX_CSI_SLOTS],
            starts: [0; MAX_CSI_SLOTS + 1],
            param_count: 0,
            current_slot: 0,
            in_param: false,
            truncated: false,
        }
    }

    /// Resets all accumulated parameters for the next sequence.
    pub fn clear(&mut self) {
        self.param_count = 0;
        self.starts[0] = 0;
        self.current_slot = 0;
        self.in_param = false;
        self.truncated = false;
    }

    /// Appends a decimal digit (`0..=9`) to the active slot with `u16` saturation.
    pub(crate) fn push_digit(&mut self, digit: u8) {
        if self.truncated {
            return;
        }
        if !self.in_param {
            self.values[self.current_slot as usize] = 0;
            self.in_param = true;
        }
        let slot = &mut self.values[self.current_slot as usize];
        *slot = slot.saturating_mul(10).saturating_add(u16::from(digit));
    }

    /// Commits the current parameter and advances to the next `;`-delimited parameter.
    pub(crate) fn next_param(&mut self) {
        if self.truncated {
            return;
        }
        if !self.in_param {
            self.values[self.current_slot as usize] = 0;
        }
        let next = self.current_slot + 1;
        self.param_count += 1;
        self.starts[self.param_count as usize] = next;
        if usize::from(next) < MAX_CSI_SLOTS {
            self.current_slot = next;
            self.values[usize::from(next)] = 0;
            self.in_param = true;
        } else {
            self.in_param = false;
            self.truncated = true;
        }
    }

    /// Advances to the next `:`-delimited subparameter within the current parameter.
    pub(crate) fn next_subparam(&mut self) {
        if self.truncated {
            return;
        }
        if !self.in_param {
            self.values[self.current_slot as usize] = 0;
            self.in_param = true;
        }
        let next = self.current_slot + 1;
        if usize::from(next) < MAX_CSI_SLOTS {
            self.current_slot = next;
            self.values[usize::from(next)] = 0;
        } else {
            self.param_count += 1;
            self.starts[self.param_count as usize] = next;
            self.in_param = false;
            self.truncated = true;
        }
    }

    /// Finalizes any trailing active parameter before dispatch.
    pub(crate) fn finish(&mut self) {
        if self.in_param {
            let next = self.current_slot + 1;
            self.param_count += 1;
            self.starts[self.param_count as usize] = next;
            self.in_param = false;
        }
    }

    /// Returns the number of top-level `;`-separated parameters.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.param_count as usize
    }

    /// Returns `true` if no parameters were supplied.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.param_count == 0
    }

    /// Returns `true` if the sequence exceeded [`MAX_CSI_SLOTS`] and trailing slots were dropped.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Returns `true` if any top-level parameter contains `:` subparameters.
    #[must_use]
    pub const fn has_subparams(&self) -> bool {
        self.starts[self.param_count as usize] > self.param_count
    }

    /// Returns the subparameter slice for top-level parameter `idx` (`&[val]` when unsplit).
    #[must_use]
    pub fn subparams(&self, idx: usize) -> Option<&[u16]> {
        if idx >= self.len() {
            return None;
        }
        let start = usize::from(self.starts[idx]);
        let end = usize::from(self.starts[idx + 1]);
        Some(&self.values[start..end])
    }

    /// Returns the primary (first) value for top-level parameter `idx`.
    #[must_use]
    pub fn get(&self, idx: usize) -> Option<u16> {
        self.subparams(idx).and_then(|s| s.first().copied())
    }

    /// Returns the primary value for parameter `idx`, or `default` if absent (`None`).
    #[must_use]
    pub fn get_or(&self, idx: usize, default: u16) -> u16 {
        self.get(idx).unwrap_or(default)
    }

    /// Returns the primary value for parameter `idx`, or `default` if absent (`None`) or `0`.
    #[must_use]
    pub fn get_nonzero_or(&self, idx: usize, default: u16) -> u16 {
        match self.get(idx) {
            Some(v) if v != 0 => v,
            _ => default,
        }
    }

    /// Returns an iterator over each top-level parameter's subparameter slice.
    #[must_use]
    pub fn iter(&self) -> CsiParamsIter<'_> {
        CsiParamsIter {
            params: self,
            front: 0,
            back: self.len(),
        }
    }
}

impl<'a> IntoIterator for &'a CsiParams {
    type Item = &'a [u16];
    type IntoIter = CsiParamsIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Borrowing iterator over top-level parameter slices in [`CsiParams`].
#[derive(Debug, Clone)]
pub struct CsiParamsIter<'a> {
    params: &'a CsiParams,
    front: usize,
    back: usize,
}

impl<'a> Iterator for CsiParamsIter<'a> {
    type Item = &'a [u16];

    fn next(&mut self) -> Option<Self::Item> {
        if self.front >= self.back {
            return None;
        }
        let item = self.params.subparams(self.front);
        self.front += 1;
        item
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.back - self.front;
        (len, Some(len))
    }
}

impl ExactSizeIterator for CsiParamsIter<'_> {}

impl DoubleEndedIterator for CsiParamsIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.front >= self.back {
            return None;
        }
        self.back -= 1;
        self.params.subparams(self.back)
    }
}

impl FusedIterator for CsiParamsIter<'_> {}
