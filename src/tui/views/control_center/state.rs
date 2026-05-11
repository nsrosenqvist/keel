//! Control-center sidebar items.
//!
//! `Item` is the unit row the home view iterates over; `ItemKind`
//! classifies what kind of thing the row points at.

/// What kind of thing a sidebar item points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ItemKind {
    /// The configured container runtime itself — a single synthetic
    /// row that hosts backend lifecycle output (`U` / `D` / `R` /
    /// `S` for compose). One such row exists when the configured
    /// backend is non-`none`.
    Runtime,
    Service,
    Watcher,
    Recipe,
    Script,
}

/// A single sidebar entry.
#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub kind: ItemKind,
}
