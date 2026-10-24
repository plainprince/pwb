# Fuzz targets

Mutation-driven tests for the parsers that consume untrusted network or
artifact bytes. The targets are deliberately outside the main workspace and
need a nightly toolchain:

```bash
cargo install cargo-fuzz
cargo +nightly fuzz run onion_cell
cargo +nightly fuzz run network_message
cargo +nightly fuzz run bundle
cargo +nightly fuzz run pml
```

Each target asserts only that decoding terminates without a panic; the normal
`cargo test` suite covers the semantic accept/reject cases.

## Continuous runs

[`workflow`](../.github/workflows/fuzz.yml) runs every target:

- on pull requests that touch the fuzzers or the decoders they cover, for
  30 seconds per target;
- weekly (and on manual dispatch) for five minutes per target.

Targets cover onion cells, JSON network messages, `.pwb` bundles, and the PML
markup/PSS stylesheet parsers.

The workflow installs nightly Rust and `cargo-fuzz`, caches each target's
corpus under `fuzz/corpus/`, and uploads `fuzz/artifacts/<target>/` when a run
fails. Add a minimized crash input to the target's corpus (or, better, a
semantic unit-test regression) before fixing it, so the same bug stays covered.
