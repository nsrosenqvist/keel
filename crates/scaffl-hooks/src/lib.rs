//! Git hook installation and `.pre-commit-config.yaml` compatibility.
//!
//! Phase 4 work. The intended shape of this crate:
//!
//! - Parse `.pre-commit-config.yaml` natively into a typed model.
//! - Run `language: system` and `language: script` hooks directly.
//! - Bridge `language: python | node | ruby | golang | rust | docker` to the
//!   `pre-commit` binary when present.
//! - Install `.git/hooks/<stage>` shims that call `scaffl hooks run <stage>`.

/// Placeholder type so the workspace builds.
pub struct HookRunner;

impl HookRunner {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HookRunner {
    fn default() -> Self {
        Self::new()
    }
}
