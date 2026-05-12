//! PHP / Composer detector. Sniffs for Laravel (`artisan`) and Symfony
//! (`symfony.lock`) to add framework-specific commands.

use crate::cli::commands::init::detector::{CommandFragment, Detector, Finding, Fragment};
use std::path::Path;

pub struct Php;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Laravel,
    Symfony,
    Vanilla,
}

fn flavor(root: &Path) -> Flavor {
    if root.join("artisan").exists() {
        return Flavor::Laravel;
    }
    if root.join("symfony.lock").exists() || root.join("bin/console").exists() {
        return Flavor::Symfony;
    }
    Flavor::Vanilla
}

impl Detector for Php {
    fn detect(&self, root: &Path) -> Option<Finding> {
        if !root.join("composer.json").exists() && !root.join("artisan").exists() {
            return None;
        }
        let flv = flavor(root);
        let mut fragments = vec![
            Fragment::Command(CommandFragment::shell(
                "install",
                "Install dependencies",
                "composer install",
            )),
            Fragment::Command(
                CommandFragment::shell("test", "Run tests", "composer test").with_forward_args(),
            ),
        ];
        match flv {
            Flavor::Laravel => {
                fragments.push(Fragment::Command(
                    CommandFragment::shell("artisan", "Run an artisan command", "php artisan")
                        .with_tty()
                        .with_forward_args(),
                ));
                fragments.push(Fragment::Command(
                    CommandFragment::shell(
                        "migrate",
                        "Run database migrations",
                        "php artisan migrate",
                    )
                    .with_forward_args(),
                ));
            }
            Flavor::Symfony => {
                fragments.push(Fragment::Command(
                    CommandFragment::shell(
                        "console",
                        "Run a Symfony console command",
                        "php bin/console",
                    )
                    .with_tty()
                    .with_forward_args(),
                ));
            }
            Flavor::Vanilla => {}
        }
        let tool = match flv {
            Flavor::Laravel => "laravel",
            Flavor::Symfony => "symfony",
            Flavor::Vanilla => "composer",
        };
        Some(Finding {
            ecosystem: "php",
            tool: Some(tool.into()),
            fragments,
            notes: vec![format!("Detected PHP / {tool}.")],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn detects_vanilla_composer() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("composer.json"), "{}").unwrap();
        let f = Php.detect(d.path()).unwrap();
        assert_eq!(f.tool.as_deref(), Some("composer"));
    }

    #[test]
    fn detects_laravel() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("composer.json"), "{}").unwrap();
        std::fs::write(d.path().join("artisan"), "").unwrap();
        let f = Php.detect(d.path()).unwrap();
        assert_eq!(f.tool.as_deref(), Some("laravel"));
        assert!(
            f.fragments
                .iter()
                .any(|fr| matches!(fr, Fragment::Command(c) if c.name == "artisan"))
        );
    }

    #[test]
    fn detects_symfony() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("composer.json"), "{}").unwrap();
        std::fs::write(d.path().join("symfony.lock"), "{}").unwrap();
        let f = Php.detect(d.path()).unwrap();
        assert_eq!(f.tool.as_deref(), Some("symfony"));
    }

    #[test]
    fn none_without_php_signals() {
        let d = TempDir::new().unwrap();
        assert!(Php.detect(d.path()).is_none());
    }
}
