//! `keel completions <shell>` — emit shell completion scripts to stdout.
//!
//! Standard install pattern, per shell:
//!
//! ```sh
//! keel completions bash       > /etc/bash_completion.d/keel
//! keel completions zsh        > ~/.zsh/completions/_keel
//! keel completions fish       > ~/.config/fish/completions/keel.fish
//! keel completions elvish     > ~/.config/elvish/lib/keel.elv
//! keel completions powershell > $PROFILE.keel.ps1
//! ```

use crate::cli::app::Cli;
use anyhow::Result;
use clap::CommandFactory;
use clap_complete::Shell;

pub fn run(shell: Shell) -> Result<()> {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "keel", &mut std::io::stdout());
    Ok(())
}
