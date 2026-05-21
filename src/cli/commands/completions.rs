//! `croft completions <shell>` — emit shell completion scripts to stdout.
//!
//! Standard install pattern, per shell:
//!
//! ```sh
//! croft completions bash       > /etc/bash_completion.d/croft
//! croft completions zsh        > ~/.zsh/completions/_croft
//! croft completions fish       > ~/.config/fish/completions/croft.fish
//! croft completions elvish     > ~/.config/elvish/lib/croft.elv
//! croft completions powershell > $PROFILE.croft.ps1
//! ```

use crate::cli::app::Cli;
use anyhow::Result;
use clap::CommandFactory;
use clap_complete::Shell;

pub fn run(shell: Shell) -> Result<()> {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "croft", &mut std::io::stdout());
    Ok(())
}
