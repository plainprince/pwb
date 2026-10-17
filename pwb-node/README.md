# pwb-node — layer 3: hoster + miner & system domain protocols

A **serverless** network participant. There are no servers, no work pools, and no duplicated work: indexing, ranking, verification, and domain allocation are all done *by the network* and sharded across nodes.

## Native System Domain Handlers

`pwb-node` serves native system protocols and dynamic interfaces:
- **`search.plain`**: Sharded decentralized search engine index & ranking math. Querying `search.plain` returns verified index search results.
- **`register.plain`**: Sparse credit domain registry interface.
- **`gossip.plain`**: P2P gossip engine for artifact propagation.
- **`nodes.plain`**: Peer routing table and network health.

## Module Structure

- `host`      — ingest `.pwb` artifacts from layer 1; serve them to peers.
- `search`    — decentralized inverted index + ranking (TF-IDF / PageRank-in-spirit).
- `verifier`  — confirm peers' search/credit work is genuine (anti-Sybil layer).
- `identity`  — keypair identity + credit account, on `pwb-core::crypto`.
- `domain`    — credit → domain allocation registry.
- `system`    — native system domain protocols (`search.plain`, `register.plain`).

## Documentation

- [Protocol Reference](../docs/protocol.md) — pinned network parameters and the `.plain` security rule.
- [§1 Core Architecture](../docs/design/01-core-architecture.md) — the shared `pwb-core::net::NodeEngine` this node runs on.
- [§3 Decentralized Node Discovery](../docs/design/03-node-discovery.md) — LAN beacons, connection links, and QR pairing.
- [§4 Anti-Sybil Credit System](../docs/design/04-credit-system.md) — `credit`, `ledger`, and `blind` minting/attestation.
- [§5 Pricing & Gossip Merging](../docs/design/05-pricing-and-merge.md) — `domain` rolling-average pricing and merge reconciliation.

Browse everything from the [docs index](../docs/README.md).
