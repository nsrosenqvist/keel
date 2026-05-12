//! The detector registry.
//!
//! Adding a new ecosystem = adding a file under `detectors/` and an entry
//! to [`registry`]. Order matters in exactly two places:
//!
//! - **Single-owner sections** (`[runtime]`, `[devcontainer]`): the first
//!   detector to claim them wins. Container detectors come first.
//! - **Duplicate command names**: every matching detector's command is
//!   emitted (commented), grouped under a "pick one" header — order
//!   determines the visual order under the header. Not load-bearing.
//!
//! All detector structs are zero-sized unit structs so the registry can
//! be a `'static` slice of `'static` references.

use super::detector::Detector;

mod compose;
mod devcontainer;
mod dotenv;
mod go;
mod node;
mod php;
mod python;
mod ruby;
mod rust;

pub fn registry() -> &'static [&'static dyn Detector] {
    static DETECTORS: &[&dyn Detector] = &[
        &compose::Compose,
        &devcontainer::Devcontainer,
        &dotenv::DotEnv,
        &go::Go,
        &node::Node,
        &php::Php,
        &python::Python,
        &ruby::Ruby,
        &rust::Rust,
    ];
    DETECTORS
}
