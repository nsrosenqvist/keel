//! Command palette state.
//!
//! The palette is a fuzzy launcher over recipes and scripts. Service
//! items are excluded — they're for log viewing, not for running.
//! Matching is delegated to [`nucleo_matcher`].
//!
//! Pure state machine: rendering and key handling live in their
//! respective modules. The terminal layer mutates a [`Palette`] in
//! response to key events; the renderer reads from it.

use crate::app::{Item, ItemKind};
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

/// Maximum number of matches kept in the visible list. Matches beyond
/// this rank are filtered out before rendering — keeps redraws fast on
/// projects with hundreds of recipes.
pub const MAX_MATCHES: usize = 50;

/// One scored match. `item_index` is an index into `App::items()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub item_index: usize,
    pub score: u32,
}

/// Palette state.
pub struct Palette {
    input: String,
    matches: Vec<Match>,
    selected: usize,
    matcher: Matcher,
    /// Snapshot of item kinds so we can recompute matches without holding
    /// a back-reference to App.
    candidates: Vec<(usize, String)>,
}

impl Palette {
    pub fn new(items: &[Item]) -> Self {
        let candidates = items
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item.kind, ItemKind::Recipe | ItemKind::Script))
            .map(|(idx, item)| (idx, item.name.clone()))
            .collect();
        let mut palette = Self {
            input: String::new(),
            matches: Vec::new(),
            selected: 0,
            matcher: Matcher::new(Config::DEFAULT),
            candidates,
        };
        palette.recompute();
        palette
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selected_match(&self) -> Option<Match> {
        self.matches.get(self.selected).copied()
    }

    /// Resync candidate snapshot. Call when the underlying item list could
    /// have changed (e.g. config reload — not implemented yet, but cheap
    /// to add later).
    pub fn refresh_candidates(&mut self, items: &[Item]) {
        self.candidates = items
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item.kind, ItemKind::Recipe | ItemKind::Script))
            .map(|(idx, item)| (idx, item.name.clone()))
            .collect();
        self.recompute();
    }

    pub fn push_char(&mut self, c: char) {
        self.input.push(c);
        self.recompute();
    }

    pub fn pop_char(&mut self) {
        self.input.pop();
        self.recompute();
    }

    pub fn select_next(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.matches.len();
    }

    pub fn select_prev(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.matches.len() - 1
        } else {
            self.selected - 1
        };
    }

    fn recompute(&mut self) {
        self.matches.clear();
        self.selected = 0;

        if self.input.is_empty() {
            // Empty query → show all candidates in their original order so
            // the user can scroll-launch anything.
            self.matches.extend(
                self.candidates
                    .iter()
                    .take(MAX_MATCHES)
                    .map(|(idx, _)| Match {
                        item_index: *idx,
                        score: 0,
                    }),
            );
            return;
        }

        let pattern = Pattern::parse(&self.input, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<Match> = self
            .candidates
            .iter()
            .filter_map(|(idx, name)| {
                let haystack = Utf32Str::new(name, &mut buf);
                pattern
                    .score(haystack, &mut self.matcher)
                    .map(|score| Match {
                        item_index: *idx,
                        score,
                    })
            })
            .collect();
        scored.sort_by(|a, b| b.score.cmp(&a.score));
        scored.truncate(MAX_MATCHES);
        self.matches = scored;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<Item> {
        vec![
            Item {
                name: "test".into(),
                kind: ItemKind::Recipe,
            },
            Item {
                name: "migrate".into(),
                kind: ItemKind::Recipe,
            },
            Item {
                name: "seed".into(),
                kind: ItemKind::Script,
            },
            Item {
                name: "app".into(),
                kind: ItemKind::Service,
            },
        ]
    }

    #[test]
    fn services_are_excluded_from_candidates() {
        let palette = Palette::new(&items());
        // Empty input shows all non-service candidates.
        assert_eq!(palette.matches().len(), 3);
        let names: Vec<_> = palette
            .matches()
            .iter()
            .map(|m| items()[m.item_index].name.clone())
            .collect();
        assert!(!names.contains(&"app".to_string()));
        assert_eq!(names, vec!["test", "migrate", "seed"]);
    }

    #[test]
    fn fuzzy_search_filters_and_ranks() {
        let mut palette = Palette::new(&items());
        for c in "mig".chars() {
            palette.push_char(c);
        }
        let names: Vec<_> = palette
            .matches()
            .iter()
            .map(|m| items()[m.item_index].name.clone())
            .collect();
        assert!(names.contains(&"migrate".to_string()));
        // Top match should be migrate, not seed (which doesn't contain mig).
        assert_eq!(names.first().map(String::as_str), Some("migrate"));
    }

    #[test]
    fn navigation_wraps() {
        let mut palette = Palette::new(&items());
        let n = palette.matches().len();
        assert_eq!(palette.selected(), 0);
        palette.select_prev();
        assert_eq!(palette.selected(), n - 1);
        palette.select_next();
        assert_eq!(palette.selected(), 0);
    }

    #[test]
    fn pop_char_recomputes() {
        let mut palette = Palette::new(&items());
        palette.push_char('z');
        assert!(palette.matches().is_empty());
        palette.pop_char();
        assert_eq!(palette.matches().len(), 3);
    }

    #[test]
    fn empty_after_unmatched_pattern() {
        let mut palette = Palette::new(&items());
        for c in "xyzqq".chars() {
            palette.push_char(c);
        }
        assert!(palette.matches().is_empty());
        assert!(palette.selected_match().is_none());
    }
}
