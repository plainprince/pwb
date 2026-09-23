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
