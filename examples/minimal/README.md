# minimal example

The smallest croft project that does something meaningful. No container
backend, no scripts directory, no env files — just a couple of recipes
that run on the host.

```sh
cd examples/minimal
croft list
croft greet
croft check          # array of steps, run sequentially
croft --explain greet
```

This is what `croft init` produces in an empty directory (with
`backend = "none"`).
