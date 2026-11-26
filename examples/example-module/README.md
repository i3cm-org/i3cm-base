# i3cm example module

A complete i3cm module: types, a profile, a carrier, operations, a hook and a schedule, with its
code as a WebAssembly component written in Rust.

```
i3cm/          module.ts, types/, i18n/   (declarative part, read by typecat)
ops/           Rust crate, built with: cargo build --release --target wasm32-wasip2
wit/           the kernel's WIT world (copy of i3cm-base/wit/module.wit)
```

Releases: a tag `v*` builds and signs `example-<version>.tar.gz` (see
`.github/workflows/release.yml`); the operator installs it with
`i3cm-server module install <package> --trusted <public key>`.

License: Apache-2.0.
