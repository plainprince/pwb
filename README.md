# pwb — PlainWeb

A **serverless web** with native decentralized search and optional identifiability.

No central servers. No pools. No duplicate work. Indexing, ranking, verification, and domain allocation are all done by the network itself, sharded across nodes.

Browse official built-in pages on `.plain` and publish unofficial `.pwb` sites, with built-in decentralized search and domain registration — powered by Rust.

## The network needs a seed — help start it

PlainWeb has no built-in bootstrap list. For a stranger to join from anywhere on
the public internet, someone has to run a **seed** (a long-running `pwb-node`
with a public address) and publish a signed join descriptor.

**Want to help? Run a seed and open a PR (or issue) with the published
descriptor.** The two steps:

1. Run a seed node: follow [docs/seed-node.md](docs/seed-node.md). Open UDP
   `7921` on a public VPS, install the systemd unit, and grab the printed
   connection link.
2. Publish the signed join descriptor with [deploy/publish-join.sh](deploy/publish-join.sh).
   It writes `deploy/join.json`, verifies the signature, and prints the signer
   public key. Commit that file and open a PR — or just open an issue with the
   seed link, the signer key, and the `deploy/join.json` contents.

Once merged, every new clone of this repository bootstraps from your seed with
no flags. The full procedure (rotation, withdrawal, emergency shutdown) is in
[docs/runbooks/publish-a-seed.md](docs/runbooks/publish-a-seed.md).

## What it is

PlainWeb replaces legacy URLs and central search engines with peer-to-peer system protocols: `search.plain`, `register.plain`, `gossip.plain`, `nodes.plain`, `network.plain`, `chat.plain`, `help.plain`, and `devtools.plain`. The `.plain` TLD is the **official** namespace — the browser-shipped system pages plus the built-in content pages `welcome.plain` and `demo.plain`. The **unofficial** namespace (`.pwb`, `.non`, `.yep`, `.lam`, `.arch`, `.vio`, `.meh`, `.org`, `.net`, `.com` — [full list](docs/tlds.md)) holds user/network sites claimed by participants using sparse credits.

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

Once paired, talk to that peer: open `chat.plain` in the browser (sends are
sealed, and ride onion circuits when the browser runs in onion mode), or
reply from the daemon itself with an E2EE chat REPL over stdin (transcripts
persist under `<data>/chat.json`, and `--chat-via direct|onion|auto` picks
how lines travel):

```sh
cargo run -p pwb-node -- --connect "<link>" --chat 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef --chat-via direct
```

Every sent line is acknowledged: the page marks it ✓ when the peer
confirms it within 8 seconds and ⚠ when nothing does, and the REPL prints
the same outcome.

Rooms are the public side of the same page: `chat.plain` lists them, and
every node joins `lobby` by default. A post is signed by its author and
flooded to the whole network through whatever peers are around — members
keep the history, bystanders relay it — so anyone can read along at
`chat.plain?room=lobby`, or from the daemon:

```sh
cargo run -p pwb-node -- --connect "<link>" --room lobby
```

See the layer READMEs above for usage; each links into the protocol and design docs.
