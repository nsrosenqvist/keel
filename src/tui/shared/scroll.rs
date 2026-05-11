//! Unified scroll state for the diff view's body panes.
//!
//! Before Phase 8 the diff view carried four parallel `HashMap`s
//! (`body_scroll`, `read_scroll`, `body_h_scroll`, `read_h_scroll`)
//! plus eight near-duplicate methods working in pairs (`scroll_by` /
//! `h_scroll_by`, `to_top` / `to_bottom`, …). This consolidates the
//! pair-per-mode into one [`BodyScroll`] structure with an [`Axis`]
//! parameter; the diff view holds one per `BodyMode`.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// Per-file scroll offsets for one body mode (either Diff or Read).
/// Both axes live in the same struct because they're tied to the
/// same file path — switching files preserves both pan and scroll;
/// the maps don't need separate locality.
#[derive(Debug, Clone, Default)]
pub struct BodyScroll {
    v: HashMap<String, usize>,
    h: HashMap<String, usize>,
}

impl BodyScroll {
    /// Current offset for `path` on `axis`. 0 when no entry exists
    /// for the path — the safe pre-render default.
    pub fn get(&self, path: &str, axis: Axis) -> usize {
        match axis {
            Axis::Vertical => self.v.get(path).copied().unwrap_or(0),
            Axis::Horizontal => self.h.get(path).copied().unwrap_or(0),
        }
    }

    /// Shift the offset for `path` on `axis` by `delta`, clamped to
    /// `[0, max]`. Negative `delta` scrolls toward 0.
    pub fn scroll_by(&mut self, path: &str, axis: Axis, delta: i32, max: usize) {
        let map = self.map_mut(axis);
        let cur = map.get(path).copied().unwrap_or(0) as i64;
        let next = (cur + delta as i64).max(0).min(max as i64) as usize;
        map.insert(path.to_string(), next);
    }

    /// Set the offset for `path` on `axis` directly, clamped at the
    /// non-negative side (callers supply a pre-clamped value when
    /// they have a `max` to enforce).
    pub fn set(&mut self, path: &str, axis: Axis, value: usize) {
        self.map_mut(axis).insert(path.to_string(), value);
    }

    /// Drop the offset for `path` on `axis`. Used by the wrap toggle
    /// to clear h-scroll for a file when wrap mode is enabled (a
    /// non-zero offset under wrap would silently chop columns).
    pub fn remove(&mut self, path: &str, axis: Axis) {
        self.map_mut(axis).remove(path);
    }

    /// Forget every entry. Used by `mark_stale` on refresh.
    pub fn clear(&mut self) {
        self.v.clear();
        self.h.clear();
    }

    fn map_mut(&mut self, axis: Axis) -> &mut HashMap<String, usize> {
        match axis {
            Axis::Vertical => &mut self.v,
            Axis::Horizontal => &mut self.h,
        }
    }
}
