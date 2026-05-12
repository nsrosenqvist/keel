//! Ruby detector. Bundler-based, with a Rails sniff that swaps in
//! `bin/rails` flavoured commands.

use crate::cli::commands::init::detector::{CommandFragment, Detector, Finding, Fragment};
use std::path::Path;

pub struct Ruby;

fn is_rails(root: &Path) -> bool {
    root.join("bin/rails").exists() || root.join("config/application.rb").exists()
}

impl Detector for Ruby {
    fn detect(&self, root: &Path) -> Option<Finding> {
        if !root.join("Gemfile").exists() {
            return None;
        }
        let rails = is_rails(root);
        let (tool, fragments) = if rails {
            (
                "rails",
                vec![
                    Fragment::Command(CommandFragment::shell(
                        "install",
                        "Install gems",
                        "bundle install",
                    )),
                    Fragment::Command(
                        CommandFragment::shell("console", "Rails console", "bin/rails console")
                            .with_tty(),
                    ),
                    Fragment::Command(
                        CommandFragment::shell("migrate", "Run migrations", "bin/rails db:migrate")
                            .with_forward_args(),
                    ),
                    Fragment::Command(
                        CommandFragment::shell("test", "Run tests", "bin/rails test")
                            .with_forward_args(),
                    ),
                ],
            )
        } else {
            (
                "bundler",
                vec![
                    Fragment::Command(CommandFragment::shell(
                        "install",
                        "Install gems",
                        "bundle install",
                    )),
                    Fragment::Command(
                        CommandFragment::shell("test", "Run tests", "bundle exec rake test")
                            .with_forward_args(),
                    ),
                ],
            )
        };
        Some(Finding {
            ecosystem: "ruby",
            tool: Some(tool.into()),
            fragments,
            notes: vec![format!(
                "Detected Gemfile{}.",
                if rails { " + Rails" } else { "" }
            )],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn detects_bundler() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("Gemfile"), "").unwrap();
        let f = Ruby.detect(d.path()).unwrap();
        assert_eq!(f.tool.as_deref(), Some("bundler"));
    }

    #[test]
    fn detects_rails_via_application_rb() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("Gemfile"), "").unwrap();
        std::fs::create_dir_all(d.path().join("config")).unwrap();
        std::fs::write(d.path().join("config/application.rb"), "").unwrap();
        let f = Ruby.detect(d.path()).unwrap();
        assert_eq!(f.tool.as_deref(), Some("rails"));
        assert!(f.fragments.iter().any(
            |fr| matches!(fr, Fragment::Command(c) if c.name == "console" || c.name == "migrate")
        ));
    }

    #[test]
    fn detects_rails_via_bin_rails() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("Gemfile"), "").unwrap();
        std::fs::create_dir_all(d.path().join("bin")).unwrap();
        std::fs::write(d.path().join("bin/rails"), "").unwrap();
        let f = Ruby.detect(d.path()).unwrap();
        assert_eq!(f.tool.as_deref(), Some("rails"));
    }

    #[test]
    fn none_without_gemfile() {
        let d = TempDir::new().unwrap();
        assert!(Ruby.detect(d.path()).is_none());
    }
}
