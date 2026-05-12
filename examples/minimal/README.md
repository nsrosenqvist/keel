# minimal example

The smallest ampelos project that does something meaningful. No container
backend, no scripts directory, no env files — just a couple of recipes
that run on the host.

```sh
cd examples/minimal
ampelos list
ampelos greet
ampelos check          # array of steps, run sequentially
ampelos --explain greet
```

This is what `ampelos init` produces in an empty directory (with
`backend = "none"`).
