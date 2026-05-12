//! Ports — small read-only traits that let the runtime depend on
//! abstractions rather than the concrete `Config` schema.
//!
//! [`crate::config::Config`] impls each of these so today's call
//! sites work unchanged; the value is in the dependency direction.
//! When a future test wants to drive [`crate::runtime::Executor`]
//! against synthetic recipes, it can hand in a mock impl of
//! [`RecipeProvider`] + [`ScriptProvider`] without having to
//! construct an entire `Config` value.
//!
//! Names follow the "port" naming from hexagonal architecture: each
//! trait is a port into the runtime's domain, and `Config` is one
//! adapter that supplies them all.

use crate::config::{Recipe, ScriptCommand};

/// Recipe lookup port. Backs [`crate::runtime::Executor`]'s
/// recipe-resolution paths (`run_recipe`, dependency walks, step
/// references) without exposing the full `Config` schema.
pub trait RecipeProvider {
    /// Resolve a recipe by name, or `None` when no such recipe is
    /// defined.
    fn get_recipe(&self, name: &str) -> Option<&Recipe>;

    /// Convenience: does a recipe with this name exist? Default
    /// implementation is `get_recipe(name).is_some()`.
    fn has_recipe(&self, name: &str) -> bool {
        self.get_recipe(name).is_some()
    }
}

/// Script lookup port. Same shape as [`RecipeProvider`] but for
/// `.ampelos/commands/<name>` script entries.
pub trait ScriptProvider {
    fn get_script(&self, name: &str) -> Option<&ScriptCommand>;

    fn has_script(&self, name: &str) -> bool {
        self.get_script(name).is_some()
    }
}

impl RecipeProvider for crate::config::Config {
    fn get_recipe(&self, name: &str) -> Option<&Recipe> {
        self.commands.get(name)
    }
}

impl ScriptProvider for crate::config::Config {
    fn get_script(&self, name: &str) -> Option<&ScriptCommand> {
        self.scripts.get(name)
    }
}
