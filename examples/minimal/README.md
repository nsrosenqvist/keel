# minimal example

The smallest scaffl project that does something meaningful. No container
backend, no scripts directory, no env files — just a couple of recipes
that run on the host.

```sh
cd examples/minimal
scaffl list
scaffl greet
scaffl check          # array of steps, run sequentially
scaffl --explain greet
```

This is what `scaffl init` produces in an empty directory (with
`backend = "none"`).
