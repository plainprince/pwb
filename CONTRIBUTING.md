# Contributing to PlainWeb (pwb)

PlainWeb is a serverless web stack written in Rust
(`pwb-core` / `pwb-web` / `pwb-browser` / `pwb-node`). Contributions are
welcome — bug fixes, features, docs, tests, and protocol clarifications.

## 1. Build

```sh
# Full workspace check (fast, no test binaries)
cargo check --workspace

# Full build including test targets
cargo build --workspace --all-targets

# Run a layer
cargo run -p pwb-node          # serverless node
cargo run -p pwb-browser -- welcome.plain   # browser
cargo run -p pwb-web -- demo_site demo.pwb   # bundler
```

The workspace resolver is `2`; all crates use edition 2021. No network is
required for `cargo check`/`cargo test` — dependencies are vendored/locked
via `Cargo.lock`.

Linux, macOS, and Windows are supported hosts, and CI runs the zero-warning
gate on Linux and Windows. Default state paths go through
`pwb_core::paths::home_dir()` (`$HOME`, else `$USERPROFILE`, else
`%HOMEDRIVE%%HOMEPATH%`), so a shell that never sets `$HOME` — stock
`cmd.exe` and PowerShell — still resolves `~/.pwb` instead of scattering data
directories into the working directory. New default paths must use that
helper rather than reading `$HOME` directly.

## 2. Format

Run before every commit:

```sh
cargo fmt --all          # apply formatting
cargo fmt --all --check  # verify (CI gate)
```

## 3. Lint

```sh
cargo clippy --workspace --all-targets
```

The CI gate is **zero warnings**. Fix every clippy diagnostic rather than
suppressing it.

## 4. Test

```sh
cargo test --workspace
```

Target: every test passes, every time. New features must include unit tests
in the crate they touch (`#[cfg(test)] mod tests`) and integration tests
in `<crate>/tests/` when they span multiple modules.

### Coverage

```sh
cargo llvm-cov --workspace --json --output-path /tmp/opencode/cov.json
```

The workspace baseline is **~87% lines / ~88% regions** (see `docs/protocol.md`
for how coverage is measured). New code should not drag the average down;
prefer adding tests for uncovered lines over leaving dead code.

## 5. No purely AI-generated code

Do not submit code that was written wholesale by an AI assistant without
human review, understanding, and ownership. Specifically:

- Every line you submit must be explainable by you if asked.
- AI assistance is fine as a drafting aid, but you must read, test, and
  take responsibility for the final result.
- If a file or function is predominantly AI-generated, label it as such in
  the commit message and be prepared to defend it in review.

Patches that look like unreviewed AI output will be rejected.

## 6. Protocol values

Critical constants live in source and are pinned by tests:

- `pwb-core/src/onion.rs` — `CELL_SIZE=512`, `MIN_HOPS=3`, `MAX_LAYER_SLOTS=6`, `PAYLOAD_MAX=118`
- `pwb-core/src/discovery.rs` — `DISCOVERY_PORT=7922`, `BEACON_INTERVAL=30s`, `BEACON_MAX_AGE=120s`, `ADDR_REBIND_GRACE=300s`, multicast `239.255.0.1`
- `pwb-node/src/domain.rs` — `INITIAL_CLAIM_PRICE=10`, `MIN_CLAIM_PRICE=10`, `MAX_CLAIM_PRICE=10_000`, `PRICING_WINDOW=8`
- `pwb-node/src/system.rs` — `.plain` TLD is the reserved **official** namespace (built-in system pages + shipped content pages); the **unofficial** user namespace is the `pwb-core::system::UNOFFICIAL_TLDS` list (`.pwb`, `.non`, `.yep`, `.lam`, `.arch`, `.vio`, `.meh`, `.son`, `.isuearchbtw`, `.org`, `.net`, `.com`, mirrored in `docs/tlds.md`)

Do not change any of these without updating `docs/protocol.md`, the related
tests, and the CHANGELOG in the same PR.

## 7. Submitting

1. Fork the repo, create a feature branch.
2. Make your change + tests.
3. Run `cargo fmt --all`, `cargo clippy --workspace --all-targets` (0
   warnings), and `cargo test --workspace` (0 failures).
4. Open a PR describing what changed, why, and how you verified it.

License: GNU GPLv3 — see `LICENSE`. Third-party notices are in `NOTICE`.
