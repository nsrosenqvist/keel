//! Minimal ANSI-SGR → ratatui converter.
//!
//! Scope: SGR (`ESC [ ... m`) only — fg/bg colors (8-color, bright,
//! 256-indexed, truecolor), and bold / dim / italic / underlined /
//! reversed / blink / strike modifiers. Other CSI sequences (cursor
//! moves, screen erases) are silently skipped — content that needs
//! those won't render correctly here, but that's acceptable for our
//! capture sources (`tmux capture-pane -e` normalises panes into a
//! flat SGR-only stream, and subprocess buffers we render are line-
//! at-a-time).
//!
//! Why no `ansi-to-tui` dep: this is ~120 lines, has no transitive
//! deps to manage, and gives us tighter control over the base-style
//! fallback (used so stderr lines without their own ANSI still get
//! the red tint we always painted them with).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Parse a single line of (possibly ANSI-tagged) text into a ratatui
/// `Line`, starting from `base` as the initial style.
///
/// `base` doubles as the "reset" target: SGR code `0` resets back to
/// `base` rather than `Style::default()`, so callers can layer their
/// own intent (e.g. stderr-red) underneath any ANSI the line carries.
pub fn ansi_to_line(input: &str, base: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut style = base;
    let mut buf = String::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            let mut params = String::new();
            let mut final_byte = '\0';
            for c2 in chars.by_ref() {
                if c2.is_ascii_digit() || c2 == ';' {
                    params.push(c2);
                } else {
                    final_byte = c2;
                    break;
                }
            }
            if final_byte == 'm' {
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), style));
                }
                style = apply_sgr(style, base, &params);
            }
            continue;
        }
        // Strip other control chars that would mangle row width
        // (tabs are passed through; everything else < 0x20 dropped).
        if (c as u32) < 0x20 && c != '\t' {
            continue;
        }
        buf.push(c);
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, style));
    }
    if spans.is_empty() {
        // Preserve empty-line height for downstream layout.
        spans.push(Span::styled(String::new(), base));
    }
    Line::from(spans)
}

fn apply_sgr(mut style: Style, base: Style, params: &str) -> Style {
    if params.is_empty() {
        return base;
    }
    let codes: Vec<u32> = params
        .split(';')
        .map(|s| s.parse::<u32>().unwrap_or(0))
        .collect();
    let mut i = 0;
    while i < codes.len() {
        let code = codes[i];
        match code {
            0 => style = base,
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            5 | 6 => style = style.add_modifier(Modifier::SLOW_BLINK),
            7 => style = style.add_modifier(Modifier::REVERSED),
            9 => style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            25 => {
                style = style.remove_modifier(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK);
            }
            27 => style = style.remove_modifier(Modifier::REVERSED),
            29 => style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style = style.fg(ansi_color(code as u8 - 30)),
            38 => {
                let (color, advance) = parse_extended(&codes[i + 1..]);
                if let Some(c) = color {
                    style = style.fg(c);
                }
                i += advance;
            }
            39 => style = style.fg(Color::Reset),
            40..=47 => style = style.bg(ansi_color(code as u8 - 40)),
            48 => {
                let (color, advance) = parse_extended(&codes[i + 1..]);
                if let Some(c) = color {
                    style = style.bg(c);
                }
                i += advance;
            }
            49 => style = style.bg(Color::Reset),
            90..=97 => style = style.fg(ansi_bright(code as u8 - 90)),
            100..=107 => style = style.bg(ansi_bright(code as u8 - 100)),
            _ => {}
        }
        i += 1;
    }
    style
}

/// Decode the parameters that follow a `38` (fg) or `48` (bg) intro:
/// `5;N` (256-indexed) or `2;R;G;B` (truecolor). Returns the parsed
/// color (if any) and how many extra params to skip past.
fn parse_extended(rest: &[u32]) -> (Option<Color>, usize) {
    match rest.first() {
        Some(&5) => match rest.get(1) {
            Some(&n) if n <= 255 => (Some(Color::Indexed(n as u8)), 2),
            _ => (None, 1),
        },
        Some(&2) => match (rest.get(1), rest.get(2), rest.get(3)) {
            (Some(&r), Some(&g), Some(&b)) if r <= 255 && g <= 255 && b <= 255 => {
                (Some(Color::Rgb(r as u8, g as u8, b as u8)), 4)
            }
            _ => (None, 1),
        },
        _ => (None, 0),
    }
}

fn ansi_color(n: u8) -> Color {
    match n {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        _ => Color::Reset,
    }
}

fn ansi_bright(n: u8) -> Color {
    match n {
        0 => Color::DarkGray,
        1 => Color::LightRed,
        2 => Color::LightGreen,
        3 => Color::LightYellow,
        4 => Color::LightBlue,
        5 => Color::LightMagenta,
        6 => Color::LightCyan,
        7 => Color::White,
        _ => Color::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_texts(line: &Line<'_>) -> Vec<String> {
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn plain_text_yields_one_span() {
        let line = ansi_to_line("hello world", Style::default());
        assert_eq!(span_texts(&line), vec!["hello world".to_string()]);
    }

    #[test]
    fn fg_color_starts_a_new_span() {
        let line = ansi_to_line("a\x1b[31mred\x1b[0m b", Style::default());
        assert_eq!(span_texts(&line), vec!["a", "red", " b"]);
        assert_eq!(line.spans[1].style.fg, Some(Color::Red));
        // After reset, third span returns to base (no fg).
        assert_eq!(line.spans[2].style.fg, None);
    }

    #[test]
    fn bold_and_color_compose() {
        let line = ansi_to_line("\x1b[1;32mok\x1b[0m", Style::default());
        assert_eq!(span_texts(&line), vec!["ok"]);
        assert_eq!(line.spans[0].style.fg, Some(Color::Green));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn truecolor_24bit_fg() {
        let line = ansi_to_line("\x1b[38;2;10;20;30mhi", Style::default());
        assert_eq!(line.spans[0].style.fg, Some(Color::Rgb(10, 20, 30)));
    }

    #[test]
    fn indexed_256_fg() {
        let line = ansi_to_line("\x1b[38;5;208mhi", Style::default());
        assert_eq!(line.spans[0].style.fg, Some(Color::Indexed(208)));
    }

    #[test]
    fn reset_returns_to_base_not_default() {
        // Stderr-red base; ANSI flips green; reset (0) goes back to red.
        let base = Style::default().fg(Color::Red);
        let line = ansi_to_line("\x1b[32mok\x1b[0m bad", base);
        assert_eq!(line.spans[0].style.fg, Some(Color::Green));
        assert_eq!(line.spans[1].style.fg, Some(Color::Red));
    }

    #[test]
    fn unknown_csi_sequence_is_dropped() {
        // Cursor-up (CSI 2A) — not SGR; we strip it without polluting output.
        let line = ansi_to_line("a\x1b[2Ab", Style::default());
        assert_eq!(span_texts(&line), vec!["ab".to_string()]);
    }

    #[test]
    fn empty_input_yields_an_empty_styled_span() {
        // The renderer relies on every line producing at least one
        // span so blank rows still consume vertical space.
        let line = ansi_to_line("", Style::default());
        assert_eq!(line.spans.len(), 1);
        assert!(line.spans[0].content.is_empty());
    }

    #[test]
    fn standalone_reset_collapses_to_base() {
        let line = ansi_to_line("\x1b[mhi", Style::default().fg(Color::Cyan));
        assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn bright_colors_map_to_light_variants() {
        let line = ansi_to_line("\x1b[91mhi", Style::default());
        assert_eq!(line.spans[0].style.fg, Some(Color::LightRed));
    }
}
