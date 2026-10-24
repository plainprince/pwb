# pwb — PlainWeb

A **serverless web** with native decentralized search and optional identifiability.

No central servers. No pools. No duplicate work. Indexing, ranking, verification, and domain allocation are all done by the network itself, sharded across nodes.

Browse official built-in pages on `.plain` and publish unofficial `.pwb` sites, with built-in decentralized search and domain registration — powered by Rust.

## What it is

PlainWeb replaces legacy URLs and central search engines with peer-to-peer system protocols: `search.plain`, `register.plain`, `gossip.plain`, `nodes.plain`, `network.plain`, `help.plain`, and `devtools.plain`. The `.plain` TLD is the **official** namespace — the browser-shipped system pages plus the built-in content pages `welcome.plain` and `demo.plain`. The `.pwb` TLD is the **unofficial** namespace: user/network sites claimed by participants using sparse credits.

## Security

Privacy here is precise. Read the [security model](docs/security.md) for what the onion design protects against — and what it does not.

## Three layers

- **[pwb-web](pwb-web/README.md)** — author `.pml`, `.pss`, and `.pjs` sites and bundle them into `.pwb` artifacts.
- **[pwb-browser](pwb-browser/README.md)** — open `.pwb` bundles, build the DOM, run PlainJS, and render in a native window. To open sites it starts an **embedded node**: it joins configured bootstrap peers, the published join descriptor, or LAN peers, and hosts/joins the network just enough to browse. Search, content fetch, and `.pwb` resolution go over onion circuits by default (`--direct` opts out).
- **[pwb-node](pwb-node/README.md)** — run a **full-fledged node**: host sites, index search, hold credits, claim domains, relay onion cells, verify peers' work, and run LAN discovery beacons. See [production readiness](docs/production-readiness.md) for the remaining operational gaps.

Shared primitives live in **[pwb-core](pwb-core/README.md)** (wire format, crypto, transport).

## Quick start

New here? [Join the network](docs/join-the-network.md) is the short version:
start a node, connect to a peer, serve a site, query it, and check that it
worked.

```sh
# Layer 1 — compile a site folder into a .pwb bundle (demo_site is generated if absent)
cargo run -p pwb-web -- demo_site demo.pwb

# Layer 2 — browse a native system domain. Search, content fetch, and .pwb
# resolution run over onion circuits by default; --direct opts out (and
# reveals this machine's address to the peer it talks to).
cargo run -p pwb-browser -- "search.plain?q=untrackable"

# Layer 2 — browse the built-in welcome page
cargo run -p pwb-browser -- welcome.plain

# Layer 3 — run a serverless node. With no join flags it fetches the published
# join descriptor (deploy/join.json) and joins those seeds, so no hand-copied
# link is needed. --no-join stays on LAN discovery; --bootstrap /
# PWB_BOOTSTRAP pins a specific seed (a DNS name is fine). It prints its own
# connection link and a scan-to-pair QR.
cargo run -p pwb-node
```

To join a peer: run a node as above, copy the `plain://nodes.plain?connect=...`
link it prints, and dial it from a second terminal (replace the sample link
with the one your peer printed):

```sh
cargo run -p pwb-node -- --connect "plain://nodes.plain?connect=127.0.0.1:9000/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
```

See the layer READMEs above for usage; each links into the protocol and design docs.
