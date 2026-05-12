//! `ampelos completions <shell>` — emit shell completion scripts to stdout.
//!
//! Standard install pattern, per shell:
//!
//! ```sh
//! ampelos completions bash       > /etc/bash_completion.d/ampelos
//! ampelos completions zsh        > ~/.zsh/completions/_ampelos
//! ampelos completions fish       > ~/.config/fish/completions/ampelos.fish
//! ampelos completions elvish     > ~/.config/elvish/lib/ampelos.elv
//! ampelos completions powershell > $PROFILE.ampelos.ps1
//! ```

use crate::cli::app::Cli;
use anyhow::Result;
use clap::CommandFactory;
use clap_complete::Shell;

pub fn run(shell: Shell) -> Result<()> {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "ampelos", &mut std::io::stdout());
    Ok(())
}
