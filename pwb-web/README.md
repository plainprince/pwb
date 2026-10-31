# pwb-web — layer 1: authoring & site compilation

`pwb-web` takes a site folder containing:
- **`.pml`** — Plain Markup Language (element tags like `<view>`, `<text>`, `<button>`).
- **`.pss`** — Plain Stylesheets (`.class`, `#id`, or tag selectors).
- **`.pjs`** — PlainJS / Plain JavaScript (optional opaque script payload).

and compiles them into a single, compact, losslessly compressed binary **`.pwb`** bundle.

## Usage

```sh
cargo run -p pwb-web -- <site_directory> [output.pwb]
```

Pass `--author-key <path>` (or set `PWB_AUTHOR_KEY`) to sign the artifact with
an Ed25519 identity. The key may be a `pwb-node` identity JSON file
(`secret_key_hex`) or a bare 64-hex seed:

```sh
cargo run -p pwb-web -- mysite mysite.pwb --author-key ~/.pwb/identity.json
```

The signature covers the canonical bundle bytes (with a `PWB-AUTHOR-V1` domain
prefix) and is carried inside the artifact after the script-module map.
`Bundle::deserialize` verifies it, so a bundle that names an author but was
tampered with, or has no signature, is rejected instead of being trusted.

## Documentation

The `spec` and `wire` formats this layer compiles against are defined by the [`pwb-core` shared primitives](../pwb-core/README.md). For the protocol constants and full design set, browse the [documentation index](../docs/README.md) — start with the [Protocol Reference](../docs/protocol.md).
