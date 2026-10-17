# pwb — PlainWeb

A **serverless web** with native decentralized search and optional identifiability.

No central servers. No pools. No duplicate work. Indexing, ranking, verification, and domain allocation are all done by the network itself, sharded across nodes.

Build and browse `.pwb` sites with built-in decentralized search and domain registration — powered by Rust.

## What it is

PlainWeb replaces legacy URLs and central search engines with peer-to-peer system protocols: `search.plain`, `register.plain`, `gossip.plain`, `nodes.plain`, and `devtools.plain`. User sites live on `.pwb` domains claimed by the network using sparse credits.

## Security

Privacy here is precise. Read the [security model](docs/security.md) for what the onion design protects against — and what it does not.

## Three layers

- **[pwb-web](pwb-web/README.md)** — author `.pml`, `.pss`, and `.pjs` sites and bundle them into `.pwb` artifacts.
- **[pwb-browser](pwb-browser/README.md)** — open `.pwb` bundles, build the DOM, run PlainJS, and render in a native window.
- **[pwb-node](pwb-node/README.md)** — run a serverless node: host sites, index search, verify proofs, hold credits, claim domains, and **join the network** via UDP discovery and connection links.

Shared primitives live in **[pwb-core](pwb-core/README.md)** (wire format, crypto, transport).

## Quick start

```sh
# Layer 1 — compile a site folder into a .pwb bundle (demo_site is generated if absent)
cargo run -p pwb-web -- demo_site demo.pwb

# Layer 2 — browse a native system domain
cargo run -p pwb-browser -- "search.plain?q=untrackable"

# Layer 2 — browse the built-in welcome page
cargo run -p pwb-browser -- welcome.pwb

# Layer 3 — run a serverless node (joins the network via UDP discovery and
# prints its own connection link and a scan-to-pair QR on startup)
cargo run -p pwb-node
```

To join a peer: run a node as above, copy the `plain://nodes.plain?connect=...`
link it prints, and dial it from a second terminal (replace the sample link
with the one your peer printed):

```sh
cargo run -p pwb-node -- --connect "plain://nodes.plain?connect=127.0.0.1:9000/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
```

See the layer READMEs above for usage; each links into the protocol and design docs.
