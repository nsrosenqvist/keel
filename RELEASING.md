# Releasing keel

The release pipeline lives in [`.github/workflows/release.yml`](.github/workflows/release.yml)
and triggers on any tag matching `v[0-9]+.[0-9]+.[0-9]+*`. Pre-release
tags (anything containing a hyphen, e.g. `v0.1.0-rc.1`) only build
binaries and create a GitHub pre-release; stable tags additionally
publish to crates.io, update the Homebrew tap, and force-push the
floating `vMAJOR` / `vMAJOR.MINOR` tags.

This document tracks what must be in place **before** the first tag of
each kind goes out.

---

## 1. Before the first pre-release (`vX.Y.Z-rc.N`)

The pre-release path only needs the build matrix and GitHub release —
no external accounts. Useful for proving the pipeline end-to-end before
risking a stable tag.

- [ ] Workflow file is on `main`.
- [ ] On the dev machine, `cargo build --release --bin keel` succeeds
      and `env!("KEEL_TARGET")` resolves to a known target triple.
- [ ] Push a throwaway tag to test:

  ```sh
  git tag v0.0.2-rc.1
  git push origin v0.0.2-rc.1
  ```

  Verify in the Actions tab that **only** `build` + `release` ran, the
  GitHub release is marked "Pre-release", and four `.tar.gz` files plus
  `SHA256SUMS` are attached.

- [ ] From the freshly built binary:

  ```sh
  keel update                # → "already on the latest version"
  keel update --prerelease   # → upgrades to v0.0.2-rc.1
  ```

- [ ] Delete the test tag + GitHub release once the pipeline is green:

  ```sh
  git push --delete origin v0.0.2-rc.1
  git tag -d v0.0.2-rc.1
  gh release delete v0.0.2-rc.1
  ```

---

## 2. Before the first stable release (`vX.Y.Z`)

### crates.io

The `publish` job runs `cargo workspaces publish --from-git
--skip-published --no-git-commit --yes`, which publishes all eight
member crates in topological order.

- [ ] Reserve every crate name on crates.io (use a stub
      `cargo publish` from a placeholder repo, or `cargo owner --add`
      after first publish):

  - `keel-cli`
  - `keel-config`
  - `keel-runtime`
  - `keel-container`
  - `keel-tui`
  - `keel-cache`
  - `keel-agents`
  - `keel-hooks`

- [ ] Generate a crates.io API token with **publish-new** + **publish-update**
      scope on all eight crates.

- [ ] Add it as a repo secret named `CARGO_REGISTRY_TOKEN`
      (Settings → Secrets and variables → Actions).

- [ ] Local dry-run from a clean checkout to catch any missing
      `description` / `license` / `readme` fields:

  ```sh
  cargo install cargo-workspaces --locked
  cargo workspaces publish --dry-run --from-git
  ```

### Homebrew tap

The `update-homebrew` job checks out `nsrosenqvist/homebrew-keel`,
patches `Formula/keel.rb` with the new version + four sha256 sums,
and pushes a single commit per release.

- [ ] Create the public repo `nsrosenqvist/homebrew-keel`.

- [ ] Seed `Formula/keel.rb` with **four** `sha256 "PLACEHOLDER"`
      entries in this exact order — the workflow's `re.sub` walks them
      top-to-bottom: macOS arm, macOS x86, Linux arm, Linux x86. A
      minimal starting point:

  ```ruby
  class Keel < Formula
    desc "Dev-loop wrapper that adapts to your project"
    homepage "https://github.com/nsrosenqvist/keel"
    version "0.0.0"
    license any_of: ["MIT", "Apache-2.0"]

    on_macos do
      on_arm do
        url "https://github.com/nsrosenqvist/keel/releases/download/v#{version}/keel-aarch64-apple-darwin.tar.gz"
        sha256 "PLACEHOLDER"
      end
      on_intel do
        url "https://github.com/nsrosenqvist/keel/releases/download/v#{version}/keel-x86_64-apple-darwin.tar.gz"
        sha256 "PLACEHOLDER"
      end
    end

    on_linux do
      on_arm do
        url "https://github.com/nsrosenqvist/keel/releases/download/v#{version}/keel-aarch64-unknown-linux-gnu.tar.gz"
        sha256 "PLACEHOLDER"
      end
      on_intel do
        url "https://github.com/nsrosenqvist/keel/releases/download/v#{version}/keel-x86_64-unknown-linux-gnu.tar.gz"
        sha256 "PLACEHOLDER"
      end
    end

    def install
      bin.install "keel"
    end

    test do
      assert_match "keel", shell_output("#{bin}/keel --version")
    end
  end
  ```

- [ ] Create a fine-grained PAT with `Contents: Read and write` on the
      tap repo (only). Add it as a repo secret on the keel repo named
      `HOMEBREW_TAP_TOKEN`.

### Repo hygiene

- [ ] Fix the pre-existing format diff in `crates/keel-tui/`
      (`cargo fmt -p keel-tui`) so `cargo fmt --all --check` in CI
      stays green when the release workflow shells out to `cargo` on
      the runner.

---

## 3. Cutting a release

```sh
# Pre-release — pipeline runs build + release only
git tag v0.1.0-rc.1
git push origin v0.1.0-rc.1

# Stable — pipeline runs all five jobs
git tag v0.1.0
git push origin v0.1.0
```

The build job patches `[workspace.package] version` from the tag, so
`Cargo.toml` does **not** need to be bumped before tagging.

After a stable release, verify:

- GitHub release: contains four tarballs + `SHA256SUMS`, not marked pre-release.
- crates.io: all eight crates show the new version.
- Homebrew tap: a new commit on `main` titled `keel vX.Y.Z`.
- Floating tags: `git ls-remote --tags origin` shows `vX` and `vX.Y`
  pointing at the same commit as `vX.Y.Z`.
- `keel update` (without `--prerelease`) on a binary from the previous
  release upgrades cleanly.

---

## 4. Rollback

If a stable tag was cut by mistake:

- crates.io publishes are **permanent**. Yank with `cargo yank --vers
  X.Y.Z -p <crate>` for each affected crate; you cannot un-publish.
  Bump the patch and re-release.
- Delete the GitHub release + tag (`gh release delete vX.Y.Z`,
  `git push --delete origin vX.Y.Z`).
- Force-push the floating tags back to the previous stable
  (`git tag -f vX vX.Y.Z-1 && git push -f origin vX`).
- Revert the Homebrew tap commit and push.
