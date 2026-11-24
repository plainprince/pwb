# pwb-web — layer 1: authoring & site compilation

`pwb-web` turns a folder of source files into one compact `.pwb` bundle that
`pwb-browser` opens and `pwb-node` hosts.

A site folder contains:

- **`.pml`** — Plain Markup Language: elements like `<view>`, `<text>`, `<button>`.
- **`.pss`** — Plain Stylesheets: `.class`, `#id`, or tag selectors. `.css` is
  accepted too and bundled through `lightningcss`.
- **`.pjs`** — PlainJS: the page script, run by the browser's embedded QuickJS.

Everything is resolved at compile time — markup, styles, components, and script
bundles — into a single losslessly compressed artifact addressed by its BLAKE3
content ID.

## Quickstart

```sh
# 1. Build the compiler once
cargo build --release
export PATH="$PWD/target/release:$PATH"

# 2. Generate and compile the demo site (writes demo_site/ then demo.pwb)
pwb-web

# 3. See it in the window
pwb-browser --file demo.pwb
```

For a real site, write the three files and compile them:

```sh
pwb-web mysite mysite.pwb
```

Then either keep browsing it locally (`pwb-browser --file mysite.pwb`) or host
it (`pwb-node --host ./mysite`, or `pwb-node --host mysite.pwb`).

Full walkthrough: **[Author a PlainWeb site](docs/quickstart.md)** — from empty
folder to compiled bundle, with a page that has navigation, inputs, components,
and a script, plus troubleshooting for each compile error.

## Command line

```sh
pwb-web [<site_directory> [output.pwb]] [--author-key <path>]

# Defaults: demo_site → demo.pwb; demo_site/ is generated when absent.
pwb-web
pwb-web mysite
pwb-web mysite dist/mysite.pwb

# Compile a single page fragment
pwb-web page.pml page.pwb
```

`--author-key` signs the artifact with an Ed25519 identity. It accepts a
`pwb-node` identity JSON file (`secret_key_hex`) or a bare 64-hex seed; the
environment variable `PWB_AUTHOR_KEY` is read when the flag is omitted:

```sh
pwb-web mysite mysite.pwb --author-key ~/.pwb/identity.json
```

The signature covers the canonical bundle bytes (with a `PWB-AUTHOR-V1` domain
prefix) and is carried inside the artifact after the script-module map.
`Bundle::deserialize` verifies it, so a bundle that names an author but was
tampered with, or has no signature, is rejected instead of being trusted.
Unsigned bundles still work.

## What gets bundled

| Source | Compiled into |
|---|---|
| All top-level `.pml` files, alphabetical | One merged page tree |
| All top-level `.pss` files, alphabetical | One stylesheet, applied at compile time |
| All top-level `.css` files, bundled | Same stylesheet, with `@import` resolved |
| All top-level `.pjs`/`.js` files, entry last | One non-minified script payload + named module map |
| `<component>` / `<import>` | Expanded into the page; definitions are not shipped |
| `<script src>` / inline `<script>` | Extracted into the script payload |

Only top-level files are read (hidden files and subfolders are skipped). One
bundle is one page: there is no router and no per-file page mapping.

## Language reference

**[PML, PSS, and PlainJS reference](docs/language.md)** covers:

- every supported tag and its HTML alias
- attributes the renderer honors (`id`, `class`, `href`, `src`, `placeholder`,
  `value`, `icon`, `style`)
- selector support, cascade, specificity, and inheritance
- the property and unit subset the renderer implements
- components, imports, `{placeholder}` substitution, and nesting limits
- script bundling, the supported ESM-to-script rewrite, and `pwb.import`
- known limits (one page per bundle, PNG only, single-value box properties)

The PlainJS API (`document`, `pwb.*`, fetch, storage, timers, WebSocket) lives
in the [browser README](../pwb-browser/README.md#plainjs-pjs-document-api).

## Publishing the result

Compiling is all this layer does. Hosting and naming are `pwb-node`'s job:

- [Host a site — node operator quickstart](../pwb-node/docs/hosting-quickstart.md)
- [Join the network](../docs/join-the-network.md) — start a node, serve a site,
  query it, and verify it worked.
- [pwb-node README](../pwb-node/README.md) — every hosting, claim, and
  operations flag.

## Documentation

The `spec` and `wire` formats this layer compiles against are defined by the
[`pwb-core` shared primitives](../pwb-core/README.md). For protocol constants
and the full design set, browse the [documentation index](../docs/README.md) —
start with the [Protocol Reference](../docs/protocol.md).
