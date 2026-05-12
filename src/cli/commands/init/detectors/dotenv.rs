//! Dotenv detector. Owns `[env_files]`.
//!
//! `.env.example` / `.env.sample` are intentionally NOT added — they're
//! conventionally the template, not the working file. `.env.local`
//! often holds developer overrides and is included when present.

use crate::cli::commands::init::detector::{Detector, Finding, Fragment};
use std::path::Path;

pub struct DotEnv;

const FILES: &[&str] = &[".env", ".env.local"];

impl Detector for DotEnv {
    fn detect(&self, root: &Path) -> Option<Finding> {
        let found: Vec<&'static str> = FILES
            .iter()
            .copied()
            .filter(|f| root.join(f).exists())
            .collect();
        if found.is_empty() {
            return None;
        }
        let fragments = found
            .iter()
            .map(|p| Fragment::EnvFile((*p).to_string()))
            .collect();
        Some(Finding {
            ecosystem: "dotenv",
            tool: None,
            fragments,
            notes: vec![format!(
                "Detected {}; added to env_files.",
                found.join(", ")
            )],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn detects_dotenv() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join(".env"), "").unwrap();
        let f = DotEnv.detect(d.path()).unwrap();
        assert_eq!(f.fragments.len(), 1);
        assert!(matches!(&f.fragments[0], Fragment::EnvFile(s) if s == ".env"));
    }

    #[test]
    fn detects_dotenv_and_dotenv_local() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join(".env"), "").unwrap();
        std::fs::write(d.path().join(".env.local"), "").unwrap();
        let f = DotEnv.detect(d.path()).unwrap();
        assert_eq!(f.fragments.len(), 2);
    }

    #[test]
    fn ignores_env_example() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join(".env.example"), "").unwrap();
        assert!(DotEnv.detect(d.path()).is_none());
    }
}
