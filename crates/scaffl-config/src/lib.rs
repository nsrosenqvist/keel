//! scaffl configuration domain.
//!
//! Bounded context: parsing and validating user-authored configuration into
//! immutable value objects consumed by the runtime. No I/O orchestration,
//! no process supervision — just config-in, domain-out.

pub mod error;
pub mod loader;
pub mod model;
pub mod scripts;

pub use error::ConfigError;
pub use loader::{load_from_path, load_project, load_project_with_slug, parse_str};
pub use model::{
    Config, ContainersConfig, CustomService, EnvSpec, HooksConfig, Recipe, RecipeProfile, Run,
    ServicesConfig, SystemdScope, SystemdService, WorktreesConfig,
};
pub use scripts::ScriptCommand;
