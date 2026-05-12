//! Control-center sidebar items.
//!
//! `Item` is the unit row the home view iterates over; `ItemKind`
//! classifies what kind of thing the row points at. Per-kind
//! presentation methods (glyph, label, activation semantics) live
//! on [`ItemKind`] itself — centralising them here means adding a
//! new kind is one variant + one match arm per method, instead of
//! discovering five scattered `match item.kind { … }` blocks across
//! `ui.rs`, `terminal.rs`, `palette.rs`, and `app.rs`.

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

impl ItemKind {
    /// Sidebar glyph for this kind. Runtime + Service share `●`
    /// ("is this thing alive?" mental model); Recipe + Script share
    /// `▸` ("runnable command"); Watcher is `◇` for "passive
    /// listener".
    pub fn glyph(self) -> &'static str {
        match self {
            ItemKind::Runtime | ItemKind::Service => "●",
            ItemKind::Watcher => "◇",
            ItemKind::Recipe | ItemKind::Script => "▸",
        }
    }

    /// Lowercase kind tag for the detail pane and palette rows.
    pub fn label(self) -> &'static str {
        match self {
            ItemKind::Runtime => "runtime",
            ItemKind::Service => "service",
            ItemKind::Watcher => "watcher",
            ItemKind::Recipe => "recipe",
            ItemKind::Script => "script",
        }
    }

    /// True when the user can launch this kind from the palette /
    /// keymap. Services attach (via a different code path), watchers
    /// run on file change — neither is "launch a one-shot run."
    pub fn is_launchable(self) -> bool {
        matches!(self, ItemKind::Recipe | ItemKind::Script)
    }
}

/// A single sidebar entry.
#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub kind: ItemKind,
}
