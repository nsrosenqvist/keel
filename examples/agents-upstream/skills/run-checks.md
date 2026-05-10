---
name: run-checks
description: Run the full verification ladder before reporting work as done.
---

Before claiming any code change is finished, run:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Failures at any step block the change. Don't `#[allow]` clippy
lints without a written justification in the commit body.
