//! Single-line syntax highlighting for the diff body.
//!
//! Each diff line is highlighted independently — we don't carry parser
//! state across lines because diffs interleave old and new versions in
//! ways that break stateful highlighters anyway. Single-line is what
//! `delta` and `bat`'s diff mode end up doing too, and it's good
//! enough for human reading: keywords, strings, numbers, identifiers
//! all still pop.
//!
//! syntect's `SyntaxSet` and `ThemeSet` are bundled-in (no I/O at
//! runtime) and shared across all calls via `OnceLock`.
//!
//! Theme: `base16-ocean.dark` — picked to harmonise with the existing
//! dark palette without fighting our red/green diff backgrounds.

use ratatui::style::{Color, Modifier, Style};
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SynStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();

fn syntaxes() -> &'static SyntaxSet {
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    THEME.get_or_init(|| {
        let set = ThemeSet::load_defaults();
        // base16-ocean.dark sits well on our existing dark backdrop;
        // fall back to whatever's first if the bundled set ever
        // changes upstream.
        set.themes
            .get("base16-ocean.dark")
            .cloned()
            .or_else(|| set.themes.values().next().cloned())
            .expect("syntect bundled at least one theme")
    })
}

/// One highlighted span — text plus its ratatui style. Stored on
/// each `DiffLine` so rendering doesn't redo the syntect pass on
/// every frame.
#[derive(Debug, Clone)]
pub struct HighlightedSpan {
    pub style: Style,
    pub text: String,
}

/// Highlight one line of inner code (no leading `+`/`-`/` ` sigil).
/// Returns spans whose foreground colors come from the theme; the
/// caller owns background tinting. `path` is used to pick the syntax
/// — we look at the extension and fall back to plain text.
pub fn highlight_inner(path: &str, code: &str) -> Vec<HighlightedSpan> {
    let ss = syntaxes();
    let theme = theme();
    let syntax = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|ext| ss.find_syntax_by_extension(ext))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let mut h = HighlightLines::new(syntax, theme);
    // syntect wants a trailing newline on each line for the
    // line-end-anchored regexes in some grammars to match.
    let with_nl;
    let input: &str = if code.ends_with('\n') {
        code
    } else {
        with_nl = format!("{code}\n");
        &with_nl
    };
    let regions = match h.highlight_line(input, ss) {
        Ok(r) => r,
        Err(_) => {
            return vec![HighlightedSpan {
                style: Style::default(),
                text: code.to_string(),
            }];
        }
    };
    regions
        .into_iter()
        .map(|(syn, frag)| HighlightedSpan {
            style: to_ratatui_style(syn),
            // Trim the trailing newline we added so it doesn't
            // double-render.
            text: frag.trim_end_matches('\n').to_string(),
        })
        .filter(|s| !s.text.is_empty())
        .collect()
}

fn to_ratatui_style(s: SynStyle) -> Style {
    let fg = Color::Rgb(s.foreground.r, s.foreground.g, s.foreground.b);
    let mut style = Style::default().fg(fg);
    if s.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}
