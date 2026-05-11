# minimal example

The smallest keel project that does something meaningful. No container
backend, no scripts directory, no env files — just a couple of recipes
that run on the host.

```sh
cd examples/minimal
keel list
keel greet
keel check          # array of steps, run sequentially
keel --explain greet
```

This is what `keel init` produces in an empty directory (with
`backend = "none"`).
