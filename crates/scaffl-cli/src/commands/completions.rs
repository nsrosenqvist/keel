//! `scaffl completions <shell>` — emit shell completion scripts to stdout.
//!
//! Standard install pattern, per shell:
//!
//! ```sh
//! scaffl completions bash       > /etc/bash_completion.d/scaffl
//! scaffl completions zsh        > ~/.zsh/completions/_scaffl
//! scaffl completions fish       > ~/.config/fish/completions/scaffl.fish
//! scaffl completions elvish     > ~/.config/elvish/lib/scaffl.elv
//! scaffl completions powershell > $PROFILE.scaffl.ps1
//! ```

use crate::app::Cli;
use anyhow::Result;
use clap::CommandFactory;
use clap_complete::Shell;

pub fn run(shell: Shell) -> Result<()> {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "scaffl", &mut std::io::stdout());
    Ok(())
}
