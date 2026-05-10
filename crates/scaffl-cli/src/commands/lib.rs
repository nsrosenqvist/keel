//! `scaffl lib <verb>` — small interactive shell utilities exposed as
//! subcommands so install scripts (and any other shell consumer) can
//! ask the user a question without depending on a separate prompting
//! tool.
//!
//! Conventions shared by every subcommand:
//!
//! - The prompt UI is written to **stderr**, the answer is written to
//!   **stdout**. This makes `$(scaffl lib ask …)` capture cleanly in
//!   POSIX shells.
//! - When stdin is not a terminal, the commands try to be useful
//!   anyway: `ask` / `password` consume a single piped line if one is
//!   available, otherwise emit the `--default` (or exit non-zero when
//!   none is set). `confirm` honours `--default`. `select` / `filter`
//!   emit `--default` (or the first positional choice) and otherwise
//!   exit non-zero.
//! - Cancellation (Ctrl-C, ESC) exits with code 130, matching the
//!   POSIX convention for SIGINT.

use anyhow::{Context, Result, bail};
use dialoguer::theme::ColorfulTheme;
use std::io::{BufRead, IsTerminal, Read};
use std::path::Path;

/// `scaffl lib ask <prompt> [--default V]`
pub fn ask(prompt: &str, default: Option<&str>) -> Result<i32> {
    let answer = if std::io::stdin().is_terminal() {
        let theme = ColorfulTheme::default();
        let mut input = dialoguer::Input::<String>::with_theme(&theme)
            .with_prompt(prompt)
            .allow_empty(default.is_some());
        if let Some(d) = default {
            input = input.default(d.to_string());
        }
        match input.interact_text() {
            Ok(s) => s,
            Err(e) => return Ok(map_dialog_error(e)),
        }
    } else {
        read_piped_line_or_default(default)?
    };
    println!("{answer}");
    Ok(0)
}

/// `scaffl lib confirm <prompt> [--default yes|no]`. Exit 0 = yes, 1 = no.
pub fn confirm(prompt: &str, default: Option<bool>) -> Result<i32> {
    let yes = if std::io::stdin().is_terminal() {
        let theme = ColorfulTheme::default();
        let mut conf = dialoguer::Confirm::with_theme(&theme).with_prompt(prompt);
        if let Some(d) = default {
            conf = conf.default(d);
        }
        match conf.interact() {
            Ok(b) => b,
            Err(e) => return Ok(map_dialog_error(e)),
        }
    } else {
        default.ok_or_else(|| anyhow::anyhow!("no tty and no --default; cannot confirm"))?
    };
    Ok(if yes { 0 } else { 1 })
}

/// `scaffl lib password <prompt>` — no-echo input. Answer to stdout.
pub fn password(prompt: &str) -> Result<i32> {
    let answer = if std::io::stdin().is_terminal() {
        let theme = ColorfulTheme::default();
        match dialoguer::Password::with_theme(&theme)
            .with_prompt(prompt)
            .interact()
        {
            Ok(s) => s,
            Err(e) => return Ok(map_dialog_error(e)),
        }
    } else {
        read_piped_line_or_default(None)?
    };
    println!("{answer}");
    Ok(0)
}

/// `scaffl lib select <prompt> <choice>... [--multi] [--default N] [--from <path|->]`
/// Single-select prints one line (the chosen option) to stdout.
/// Multi-select prints one line per chosen option.
pub fn select(
    prompt: &str,
    choices: Vec<String>,
    multi: bool,
    default: Option<usize>,
    from: Option<&Path>,
) -> Result<i32> {
    let items = collect_items(choices, from)?;
    if items.is_empty() {
        bail!("no choices to select from");
    }
    if !std::io::stdin().is_terminal() {
        return select_non_tty(items, multi, default);
    }
    let theme = ColorfulTheme::default();
    if multi {
        let selections = dialoguer::MultiSelect::with_theme(&theme)
            .with_prompt(prompt)
            .items(&items)
            .interact();
        match selections {
            Ok(indices) => {
                for i in indices {
                    if let Some(item) = items.get(i) {
                        println!("{item}");
                    }
                }
                Ok(0)
            }
            Err(e) => Ok(map_dialog_error(e)),
        }
    } else {
        let mut sel = dialoguer::Select::with_theme(&theme)
            .with_prompt(prompt)
            .items(&items);
        if let Some(d) = default
            && d < items.len()
        {
            sel = sel.default(d);
        }
        match sel.interact() {
            Ok(i) => {
                if let Some(item) = items.get(i) {
                    println!("{item}");
                }
                Ok(0)
            }
            Err(e) => Ok(map_dialog_error(e)),
        }
    }
}

/// `scaffl lib filter <prompt> [--from <path|->]` — fuzzy-filter
/// picker. Same I/O contract as single-select.
pub fn filter(prompt: &str, choices: Vec<String>, from: Option<&Path>) -> Result<i32> {
    let items = collect_items(choices, from)?;
    if items.is_empty() {
        bail!("no choices to filter");
    }
    if !std::io::stdin().is_terminal() {
        return select_non_tty(items, false, None);
    }
    let theme = ColorfulTheme::default();
    match dialoguer::FuzzySelect::with_theme(&theme)
        .with_prompt(prompt)
        .items(&items)
        .interact()
    {
        Ok(i) => {
            if let Some(item) = items.get(i) {
                println!("{item}");
            }
            Ok(0)
        }
        Err(e) => Ok(map_dialog_error(e)),
    }
}

/// Pick option items from either the `--from` source (`-` for stdin
/// or a path) or the positional `choices`. The `--from` form wins
/// when present, even if positional choices are also passed.
fn collect_items(choices: Vec<String>, from: Option<&Path>) -> Result<Vec<String>> {
    let Some(source) = from else {
        return Ok(choices);
    };
    let raw = if source == Path::new("-") {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("read --from stdin")?;
        buf
    } else {
        std::fs::read_to_string(source).with_context(|| format!("read {}", source.display()))?
    };
    Ok(raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Non-tty fallback for `select` / `filter`: emit `--default` if set,
/// else the first positional item. Multi-select with no default emits
/// nothing (matches "user picked zero options").
fn select_non_tty(items: Vec<String>, multi: bool, default: Option<usize>) -> Result<i32> {
    if multi {
        if let Some(d) = default
            && let Some(item) = items.get(d)
        {
            println!("{item}");
        }
        return Ok(0);
    }
    let idx = default.unwrap_or(0);
    if let Some(item) = items.get(idx) {
        println!("{item}");
        return Ok(0);
    }
    bail!("no items to select")
}

fn read_piped_line_or_default(default: Option<&str>) -> Result<String> {
    use std::io::BufReader;
    let mut reader = BufReader::new(std::io::stdin().lock());
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n > 0 {
        // Trim a single trailing newline; `read_line` includes it.
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        }
        return Ok(line);
    }
    if let Some(d) = default {
        return Ok(d.to_string());
    }
    bail!("no tty, no piped input, and no --default; aborting")
}

/// Convert a dialoguer `io::Error` into an exit code. Cancellation
/// (Ctrl-C / ESC) maps to 130 (POSIX SIGINT convention); other
/// failures map to 1 and the error is logged on stderr.
fn map_dialog_error(e: dialoguer::Error) -> i32 {
    use dialoguer::Error as DErr;
    match e {
        // `Interrupted` is dialoguer's name for "user cancelled".
        DErr::IO(io) if io.kind() == std::io::ErrorKind::Interrupted => 130,
        other => {
            eprintln!("scaffl lib: {other}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn collect_items_reads_file_lines() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "alpha").unwrap();
        writeln!(f, "beta").unwrap();
        writeln!(f).unwrap();
        writeln!(f, "  gamma  ").unwrap();
        let items = collect_items(vec![], Some(f.path())).unwrap();
        assert_eq!(items, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn collect_items_uses_positional_when_from_absent() {
        let items = collect_items(vec!["a".into(), "b".into()], None).unwrap();
        assert_eq!(items, vec!["a", "b"]);
    }

    #[test]
    fn select_non_tty_emits_default_for_single_select() {
        let items = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        // Should not panic and should return Ok.
        let _ = select_non_tty(items, false, Some(1));
    }

    #[test]
    fn select_non_tty_emits_nothing_for_multi_without_default() {
        let items = vec!["a".to_string()];
        let _ = select_non_tty(items, true, None);
    }
}
