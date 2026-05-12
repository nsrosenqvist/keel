//! keel configuration domain.
//!
//! Bounded context: parsing and validating user-authored configuration into
//! immutable value objects consumed by the runtime. No I/O orchestration,
//! no process supervision — just config-in, domain-out.

pub mod agents;
pub mod error;
pub mod install;
pub mod loader;
pub mod managed_block;
pub mod model;
pub mod scripts;

pub use agents::{
    AgentsConfig, MappingOverride, MappingOverrideKind, ResolvedOverride, SourceSpec,
};
pub use error::ConfigError;
pub use install::{InlineStep, InstallConfig, InstallStepRef, InstallStepScript};
pub use loader::{load_from_path, load_project, load_project_with_slug, parse_str};
pub use model::{
    Config, CustomService, DiffConfig, EditorConfig, EnvSpec, HooksConfig, Recipe, RecipeProfile,
    Run, RuntimeConfig, ServicesConfig, SystemdScope, SystemdService, WorktreesConfig,
};
pub use scripts::ScriptCommand;
