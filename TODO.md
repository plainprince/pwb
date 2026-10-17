# PlainWeb — Project Status (TODO.md)

## Status: Roadmap complete

All architectural refactoring and feature work tracked in this document is
finished. Every section (§1–§6) is marked complete:

- §1 Core Architecture Restructuring (`pwb-core` consolidation)
- §2 Onion Routing & Encryption Layer (`pwb-core::onion`)
- §3 Decentralized Node Discovery (`pwb-core::discovery`)
- §4 Anti-Sybil Defense & Peer-Attested Credit System (`pwb-node::credit`)
- §5 Dynamic Rolling Average Domain Pricing & Gossip Merging
- §6 Testing, Validation & Verification

## Where the design lives

The design decisions that used to be tracked here have been moved into
`docs/design/`:

- `docs/design/01-core-architecture.md` — §1 `pwb-core` consolidation (net engine, crypto, system resolver)
- `docs/design/02-onion-routing.md` — §2 onion routing, Sphinx encapsulation, cover traffic, CellChannel
- `docs/design/03-node-discovery.md` — §3 UDP broadcast/multicast discovery, connection links, QR codes, Kademlia DHT
- `docs/design/04-credit-system.md` — §4 credit minting, sharded peer-attested ledger, blind balance & privacy
- `docs/design/05-pricing-and-merge.md` — §5 rolling average pricing, CRDT gossip partition & network merge

Critical protocol constants (cell size, hop counts, discovery port, pricing
parameters, `.plain` reserve rule) are in `docs/protocol.md`.

License: GNU GPLv3 — see `LICENSE`. Third-party notices are in `NOTICE`.

---

## Next steps (open, not tracked here)

The roadmap is done; the following are suggested directions, not obligations:

### Make the browser not ugly as fuck

The current renderer is functional but bare-bones. Obvious cosmetic work:
- Real CSS-like styling beyond the minimal PSS subset (borders, rounded corners, shadows, flexbox gaps, proper typography scale)
- A default theme that doesn't look like a debug terminal (sensible colors, spacing, component chrome)
- Smooth transitions / animations instead of instant layout jumps
- Window chrome, tabs, address bar, bookmarks — a real browser shell
- HiDPI / scaling awareness beyond the viewport-relative font hack

### Add more elements

Current tag coverage is minimal. Gaps:
- `<input type="checkbox">`, `<select>`, `<textarea>` — form widgets
- `` / image rendering (currently text-only canvas)
- `<a>` hyperlinks with navigation
- `<video>` / `<audio>` placeholders at minimum
- `<table>` / `<list>` structures
- Components / `<import>` reuse across bundles (partially wired in `pwb-web` but not fully exposed)

### Test on larger networks

Everything is validated on loopback / tiny clusters. Real-world validation:
- Multi-node LAN / internet deployments (NAT traversal, not just `127.0.0.1`)
- Larger DHT peer tables (hundreds of nodes, not 3–4)
- Gossip merge with many partitions, not just pairs
- Pricing engine under sustained registration load, not synthetic epochs
- Cover traffic and onion routing over real network latency / packet loss

---

## Implemented: Network Join (P2P message transport)

The network is now actually joinable. `pwb-core::net::NodeEngine` runs a full
UDP transport stack:

- **NodeEngine::start** binds a UDP socket and spawns a sender loop, a
  receiver loop (JSON-deserializing inbound `NetworkMessage` datagrams), a
  peer maintenance loop, and a [`DiscoveryService`] sharing the engine's peer
  table for LAN beacon discovery.
- **NodeEngine::send_to / broadcast** serialize [`NetworkMessage`] as JSON and
  transmit it to registered peers over UDP, in addition to local enqueue.
- **DiscoveryService** (signed beacons over broadcast + multicast on port
  7922) merges discovered peers into the same [`PeerInfo`] table the engine
  uses, so LAN peers appear immediately.
- **Connection links** (`plain://nodes.plain?connect=<ip:port>/<pubkey>`)
  generated with the node's actual bound address; [`NodeEngine::launch_connection`]
  registers the peer and sends a [`NetworkMessage::PeerDiscovery`] so the
  connection is bidirectional.
- **Integration tests** in `pwb-node/tests/network_join.rs` prove two live
  nodes on loopback UDP can connect via links and exchange `PeerDiscovery`,
  `SearchQuery`, and `GossipAnnounce` messages end-to-end.

[`NodeEngine`]: pwb_core::net::NodeEngine
[`PeerInfo`]: pwb_core::net::PeerInfo

---

## Contributing

See `CONTRIBUTING.md` for build, lint, test, and coverage requirements, plus
the no-purely-AI-generated-code policy.
