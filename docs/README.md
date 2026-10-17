# PlainWeb docs

Everything you need to understand how PlainWeb works, linked in one place.

## Protocol

- [Protocol Reference](protocol.md) — pinned constants, network parameters, and the `.plain` security rule, across every layer.

## Security

- [Security Model — Onion Routing](security.md) — what the onion design protects against and what it does not.

## Design

- [§1 Core Architecture](design/01-core-architecture.md) — `pwb-core` consolidation; the shared network engine used by both `pwb-browser` and `pwb-node`.
- [§2 Onion Routing & Encryption](design/02-onion-routing.md) — fixed-size Sphinx-style cells, path rules, and cover traffic.
- [§3 Decentralized Node Discovery](design/03-node-discovery.md) — LAN beacons, connection links, QR pairing, and the Kademlia DHT.
- [§4 Anti-Sybil Credit System](design/04-credit-system.md) — proof-of-time credit minting, peer-attested ledger, and blind vouchers.
- [§5 Pricing & Gossip Merging](design/05-pricing-and-merge.md) — rolling-average domain pricing and partition merge reconciliation.

## Layer references

- [pwb-core](../pwb-core/README.md) — shared primitives (spec, wire, crypto, transport).
- [pwb-web](../pwb-web/README.md) — authoring & site compilation.
- [pwb-browser](../pwb-browser/README.md) — runtime & renderer.
- [pwb-node](../pwb-node/README.md) — hoster, miner, and system domains.
