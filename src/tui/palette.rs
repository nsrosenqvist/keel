//! Command palette state.
//!
//! The palette is a fuzzy launcher over recipes and scripts. Service
//! items are excluded — they're for log viewing, not for running.
//! Matching is delegated to [`nucleo_matcher`].
//!
//! Pure state machine: rendering and key handling live in their
//! respective modules. The terminal layer mutates a [`Palette`] in
//! response to key events; the renderer reads from it.

use crate::tui::views::control_center::state::{Item, ItemKind};
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use ratatui::layout::Rect;
use std::cell::RefCell;

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
    /// Per-row rects for the visible match list. Populated by the
    /// renderer each frame; hit-tested by the mouse handler.
    pub row_rects: RefCell<Vec<Rect>>,
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
            row_rects: RefCell::new(Vec::new()),
        };
        palette.recompute();
        palette
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    /// The fuzzy-match query — everything up to the first whitespace.
    /// Args after the first space don't influence matching, so typing
    /// `:echo-args foo bar` keeps `echo-args` matched while `foo bar`
    /// becomes the launch args.
    pub fn query(&self) -> &str {
        match self.input.find(char::is_whitespace) {
            Some(idx) => &self.input[..idx],
            None => &self.input,
        }
    }

    /// Tokens after the first whitespace, parsed shell-style. So
    /// `:cmd "with spaces" plain` yields `["with spaces", "plain"]`.
    /// Falls back to an empty Vec when the input has no args portion
    /// or the shell-tokeniser rejects it (unbalanced quotes, etc.).
    pub fn parsed_args(&self) -> Vec<String> {
        let Some(idx) = self.input.find(char::is_whitespace) else {
            return Vec::new();
        };
        let rest = self.input[idx..].trim_start();
        if rest.is_empty() {
            return Vec::new();
        }
        shell_words::split(rest).unwrap_or_default()
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

    pub fn select_at(&mut self, idx: usize) {
        if self.matches.is_empty() {
            return;
        }
        self.selected = idx.min(self.matches.len() - 1);
    }

    fn recompute(&mut self) {
        self.matches.clear();
        self.selected = 0;

        // Match against the query portion only — anything after the
        // first whitespace is launch args, not part of the name.
        let query = self.query();
        if query.is_empty() {
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

        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
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
        scored.sort_by_key(|m| std::cmp::Reverse(m.score));
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
    fn query_strips_args_portion() {
        let mut palette = Palette::new(&items());
        for c in "test foo bar".chars() {
            palette.push_char(c);
        }
        // Query for matching is just `test`; args are everything after.
        assert_eq!(palette.query(), "test");
        assert_eq!(palette.parsed_args(), vec!["foo", "bar"]);
        // The match still resolves because the query (not the full
        // input) drove fuzzy matching.
        let names: Vec<_> = palette
            .matches()
            .iter()
            .map(|m| items()[m.item_index].name.clone())
            .collect();
        assert!(names.contains(&"test".to_string()));
    }

    #[test]
    fn parsed_args_handles_quoted_tokens() {
        let mut palette = Palette::new(&items());
        for c in r#"test "with space" plain"#.chars() {
            palette.push_char(c);
        }
        assert_eq!(palette.parsed_args(), vec!["with space", "plain"]);
    }

    #[test]
    fn parsed_args_empty_when_no_args_portion() {
        let mut palette = Palette::new(&items());
        for c in "test".chars() {
            palette.push_char(c);
        }
        assert_eq!(palette.query(), "test");
        assert!(palette.parsed_args().is_empty());
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
