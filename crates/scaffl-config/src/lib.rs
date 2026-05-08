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
pub use loader::{load_from_path, load_project, parse_str};
pub use model::{Config, EnvSpec, HooksConfig, Recipe, RecipeProfile, Run, RuntimeConfig};
pub use scripts::ScriptCommand;
