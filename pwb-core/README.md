# pwb-core — shared primitives

The shared foundation every other PlainWeb layer builds on. `pwb-core` defines the *shapes* common to authoring, the runtime, and the network — the plain-language spec, the wire/bundle format, crypto, and transport — so neither `pwb-web`, `pwb-browser`, nor `pwb-node` has to depend on another to agree on them.

It is pure protocol and pure data structures: no rendering, no sockets-driven policy, no central server. Everything here is consumed in-process by the other crates.

## Modules

- `spec` — PlainWeb Language Specification v0: the tag model, selector/style model, and document surface.
- `wire` — the versioned `.pwb` bundle format: header, symbol table, binary DOM tree, and optional script payload.
- `crypto` — Ed25519 identity + signatures, BLAKE3 content addressing, X25519 ECDH, and unlinkable blind-signature credit vouchers with ZK ownership proofs and replay-rejecting spend tracking.
- `net` — serverless transport framing and system-domain message types; the shared `NodeEngine` peer table, routing logic, and socket event loops.
- `onion` — fixed-size (512-byte) Sphinx-style onion cells: layered ChaCha20-Poly1305 over per-hop X25519 ECDH keys.
- `dht` — Kademlia XOR-metric routing table with ping/pong health checks and latency-weighted contact selection.
- `discovery` — LAN peer discovery via Ed25519-signed UDP beacons, fanned out over IPv4 broadcast/multicast and the IPv6 all-nodes/site-scoped multicast groups.
- `qr` — byte-mode QR encoding (v1–10, level M) and `nodes.plain` pairing-payload parsing for out-of-band node exchange.
- `system` — unified system-protocol resolver: the reserved `.plain` domain list, `plain://` URL parsing, and the `nodes.plain` connection-link codec.
- `components` — reusable UI component primitives (sidebars, navbars, tabs) shared by `pwb-web` and `pwb-browser`.

## Relationship to the other layers

- **pwb-web** — uses `spec` and `wire` to compile sites into `.pwb` bundles.
- **pwb-browser** — uses `wire`, `spec`, and `components` to decompile and render bundles.
- **pwb-node** — uses `crypto`, `net`, `discovery`, `qr`, and `system` to participate in the network. The `onion` and `dht` primitives are implemented and tested but are not yet wired into the live transport/routing path.

## Documentation

- [Protocol Reference](../docs/protocol.md) — pinned wire/crypto/transport constants.
- [Security Model — Onion Routing](../docs/security.md) — what the onion design protects against and what it does not.
- [§1 Core Architecture](../docs/design/01-core-architecture.md) — the shared `NodeEngine`, `crypto`, and `system` consolidation.
- [§2 Onion Routing & Encryption](../docs/design/02-onion-routing.md) — `pwb-core::onion` cells, path rules, cover traffic.
- [§3 Decentralized Node Discovery](../docs/design/03-node-discovery.md) — `discovery` beacons, connection links, QR pairing, and the `dht`.

Browse everything from the [docs index](../docs/README.md).

## Tests

```sh
cargo test -p pwb-core
```
