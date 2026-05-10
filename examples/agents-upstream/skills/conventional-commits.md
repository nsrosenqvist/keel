---
name: conventional-commits
description: Apply Conventional Commits style to all commit messages.
---

Use Conventional Commits for every commit:

- `<type>(<scope>): <subject>`
- Types: feat / fix / refactor / docs / test / chore / build / ci / perf
- Subject: imperative, lowercase, no trailing period, under 70 chars
- Body explains *why* (the diff shows the *what*)
- Breaking changes: `!` after scope plus a `BREAKING CHANGE:` footer

Examples:

- `feat(config): support env var expansion in run strings`
- `fix(runtime): propagate non-zero exit codes from compose exec`
