# pwb — PlainWeb

A **serverless, onion-inspired web** that stays *untrackable by default* while offering optional identifiability and native decentralized search.

No central servers. No pools. No duplicate work. Indexing, ranking, verification, and domain allocation are all done by the network itself, sharded across nodes.

Build and browse `.pwb` sites with built-in decentralized search, domain registration, and end-to-end privacy — powered by Rust.

## What it is

PlainWeb replaces legacy URLs and central search engines with peer-to-peer system protocols: `search.plain`, `register.plain`, `gossip.plain`, `nodes.plain`, and `devtools.plain`. User sites live on `.pwb` domains claimed by the network using sparse credits.

## Three layers

- **pwb-web** — author `.pml`, `.pss`, and `.pjs` sites and bundle them into `.pwb` artifacts.
- **pwb-browser** — open `.pwb` bundles, build the DOM, run PlainJS, and render in a native window.
- **pwb-node** — run a serverless node: host sites, index search, verify proofs, hold credits, claim domains, and **join the network** via UDP discovery and connection links.

Shared primitives live in **pwb-core** (wire format, crypto, transport).

## Quick start

```sh
# Compile a site folder into a .pwb bundle
cargo run -p pwb-web -- demo_site demo.pwb

# Browse a system domain
cargo run -p pwb-browser -- "search.plain?q=untrackable"

# Browse a registered user domain
cargo run -p pwb-browser -- welcome.pwb

# Run a serverless node (joins the network via UDP discovery)
cargo run -p pwb-node

# Connect to another peer via a connection link
cargo run -p pwb-node -- --connect "plain://nodes.plain?connect=<ip:port>/<pubkey_hex>"
```

See `pwb-browser/README.md` for the full technical reference.
