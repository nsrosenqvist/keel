//! `scaffl env` — print the resolved project environment.

use anyhow::Result;
use scaffl_config::Config;
use scaffl_runtime::Env;
use std::path::Path;

/// Resolve the project env (process + .env files + `[env]` section) and
/// print sorted `KEY=VALUE` pairs.
///
/// Values are printed verbatim — no shell-quoting, no escaping. The output
/// is intended for human inspection, not `eval $(scaffl env)`. A future
/// `--shell` flag could add quoting if a use case appears.
pub async fn run(config: &Config, project_root: &Path) -> Result<()> {
    let env = Env::resolve(config, project_root).await?;
    for (k, v) in env.iter() {
        println!("{k}={v}");
    }
    Ok(())
}
