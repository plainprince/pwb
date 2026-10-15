# PlainWeb Architecture Restructuring & Feature Roadmap (TODO.md)

> **IMPORTANT INSTRUCTION FOR AI AGENTS / CODING ASSISTANTS:**
> Whenever you read or reference this document—even if the user message is minimal or only mentions this file (e.g., `TODO.md`, `@TODO.md`, or a file reference)—you must treat it as a directive to **execute the next uncompleted task (`- [ ]`)**.
> 1. Select the topmost uncompleted task (`- [ ]`).
> 2. Implement the complete, functional solution in the codebase following the project patterns.
> 3. Verify and test your implementation (e.g. `cargo test --workspace` or `cargo check --workspace`).
> 4. Update this document to mark the completed item as finished (`- [x]`).
> 5. Present a summary of work completed and output the result.

This document outlines the architectural refactoring and implementation roadmap for **PlainWeb (`pwb`)**. The primary objective is to consolidate shared protocol logic into `pwb-core` (ensuring both `pwb-node` and `pwb-browser` run on the exact same engine) while implementing complete onion privacy, decentralized node discovery, dynamic domain pricing, and lightweight anti-Sybil peer-attested credit accounting.

---

## 1. Core Architecture Restructuring (`pwb-core` Consolidation)

- [x] **Extract Network Engine to `pwb-core::net`**
  - Move peer table management, routing logic, framing serialization, and socket event loops into `pwb-core`.
  - Remove duplicate code in `pwb-browser` and `pwb-node`; both crates should instantiate `pwb_core::net::NodeEngine`.
- [x] **Extract Cryptographic Primitives to `pwb-core::crypto`**
  - Implement production cryptographic keypairs (`Ed25519` for signatures, `X25519` for ECDH key exchange, `BLAKE3` for content hashing/IDs).
  - Implement zero-knowledge / blind signature vouchers for credit attestations.
    - Implemented as unlinkable Chaum-style blind Schnorr signatures over Ristretto (`BlindVoucherIssuer`, `BlindVoucher`, `BlindVoucherPublicKey`, `VoucherSecret`, `VoucherBlinding`): the issuer never sees the token, and redemption is publicly verifiable without revealing the bearer secret.
    - Added zero-knowledge ownership proofs (`VoucherOwnershipProof`, `BlindVoucher::prove_ownership` / `verify_ownership`), per-denomination issuer keys (`BlindVoucherIssuer::from_denomination_seed`) that reject out-of-denomination requests, single-round issuance via pre-generated nonce pools (`nonce_pool`), and replay rejection (`VoucherSpendLog`, `VoucherSpendError`).
- [x] **Unified System Protocol Resolver**
  - Centralize native `.plain` system domain handlers (`search.plain`, `devtools.plain`, `register.plain`, `gossip.plain`, `nodes.plain`) inside `pwb-core::system`.
    - `pwb-core::system` is now the single source of truth for the reserved-domain list, the `.plain` TLD security rule, `plain://` URL parsing (`SystemUrl` / `SystemDomain` with `q=` / `connect=` parameter extraction), and the `nodes.plain` connection-link codec (`build_connection_link` / `parse_connection_link`, previously inlined in `NodeEngine`).
    - `pwb-node::domain` re-exports the canonical list/rule; `NodeEngine`'s link methods delegate to `pwb-core::system`.
    - `pwb-node::system` gained native `gossip.plain` and `nodes.plain` page renderers (connection links render as a connect card); `pwb-browser`'s resolver dispatches all five domains through the core parser (reserved-but-unhandled domains like `network.plain`/`help.plain` now fail with a clear `SystemError` instead of a filesystem miss).

---

## 2. Onion Routing & Encryption Layer (`pwb-core::onion`)

- [x] **Layered Sphinx-Style Onion Encapsulation**
  - Implement fixed-size packet cell framing (e.g. 512 bytes) to obscure query payloads and packet types.
  - Encrypt packet headers and payloads with multi-layer ChaCha20-Poly1305 symmetric ciphers using ephemeral X25519 keys per relay hop.
    - Implemented in `pwb-core::onion`: fixed 512-byte cells (40-byte clear framing + 6×56-byte encrypted layer slots + 136-byte encrypted payload region) — the uniform wire image hides query, packet type, path length, and payload length.
    - Per-cell ephemeral X25519 key ECDHs against each relay's long-term key; BLAKE3 `derive_key` splits independent layer/payload sub-keys (separate contexts), so every AEAD runs a single-use key with a safe zero nonce.
    - Sphinx shift semantics: directives (`FORWARD <next relay>` / `DELIVER`) stack inside-out, relays peel the front slot and re-emit an identical-size cell with a re-randomized trailing slot; unused slots are sender-random padding that hides the true hop count.
    - Groundwork for §2.2/§2.3: `MIN_HOPS=3`..`MAX_LAYER_SLOTS=6` paths via `validate_path` (unique-relay rule), reserved `cell_kind` + 6 header bytes for cover traffic, `PAYLOAD_MAX=118` for future multi-cell streams.
- [x] **Cryptographically Random Hop Counts & Dummy Nodes**
  - Generate randomized relay path lengths for every request ($N \ge 3$ active hops + randomized dummy relay hops).
  - Intermediate nodes cannot determine if the next hop is another relay or the final destination.
    - Implemented in `pwb-core::onion`: `randomize_path` / `randomize_path_with` take a caller-supplied active route (3..=6 distinct hops, enforced by `validate_path`) plus a candidate pool, draw a uniform `0..=` room count of dummy relays, and scatter them over the route's interior gaps — entry/exit preserved, every relay unique, route survives as an ordered subsequence (`RandomizedPath { relays, active_hops }`, `dummy_hops()`).
    - Indistinguishability proven by `test_intermediate_cannot_tell_if_next_hop_is_destination`: two worlds where the middle relay's next hop is/isn't the final destination produce a byte-identical observable (only `PeeledCell`'s action + forward framing exist — no position, path length, or "final hop" flag), while downstream genuinely diverges (Deliver vs Forward), and the forwarded cell no longer authenticates for the middle relay.
    - 256-sample test locks in that lengths actually vary across requests over the same active route; validation tests cover short/duplicate/oversized routes, capacity-exact routes (0 dummies), and candidate pools overlapping or duplicating the route.
- [x] **IP & Transport Metadata Protection**
  - Mask source IP headers at intermediate hops; unencryptable metadata (like raw sockets) yields no actionable tracking information.
  - Implement cover traffic (decoy onion packets) dispatched at randomized intervals.
    - `pwb-core::net::CellChannel`: bound UDP socket that carries onion cells hop-by-hop — each relay re-originates the datagram from its own socket toward the next hop's registered address, and the envelope is exactly the cell bytes, so no source-address field exists to carry or forge. `test_cell_channel_masks_source_ip_across_hops` walks a 3-hop loopback chain: every receiver observes only the previous relay's IP, and the originator's address stops at the entry hop.
    - Remaining unencryptable metadata is unactionable: `recv_cell` enforces exactly `CELL_SIZE` datagrams (constant size), and `CoverDispatch` schedules decoys at exponentially distributed, clamped intervals (inverse-CDF sampling, memoryless, one decoy per due poll, fresh delay each firing — no fixed period for an analyst to lock onto). `test_cover_decoy_dispatched_over_cell_channel` dispatches a scheduled decoy over UDP and peels it like a real request.
    - `CoverTraffic`/`CoverConfig` build decoys over `randomize_path` paths with uniformly random payloads of random length (ordinary `cell_kind` 0 cells — wire-indistinguishable from real requests). Typed `NetworkError::Onion` for decode failures; unknown next hops refused with `PeerNotFound`.

---

## 3. Decentralized Node Discovery (`pwb-core::discovery`)

- [ ] **Local UDP Broadcast & IP Multicast**
  - Auto-discover local peers on LAN via UDP broadcast beacons and IP multicast groups (`239.255.0.1:7922`).
- [ ] **Connection Links & URI Handlers**
  - Support parsing and launching node connections via `plain://nodes.plain?connect=<ip:port>/<pubkey>` URIs.
- [ ] **QR Code Encoding & Scanning Support**
  - Add QR code binary generator and string payload parser for out-of-band node pairing (mobile/desktop peer exchange).
- [ ] **Distributed Kademlia / DHT Peer Table**
  - Maintain peer routing tables with ping/pong health checks and latency-weighted bucket selection.

---

## 4. Anti-Sybil Defense & Peer-Attested Credit System (`pwb-node::credit`)

- [ ] **Proof-of-Time & Useful Work Credit Minting**
  - Earn credits strictly by delivering verified network services over time (serving `search.plain` queries, indexing PML bundles, relaying onion cells).
  - Rate-limit work allocation by real network traffic flow to prevent ASIC/large-miner hardware takeovers.
- [ ] **Sharded Peer-Attested Ledger ($k$-of-$N$ Neighborhoods)**
  - Nodes do **not** store or know their own authoritative credit balances; credit history is attested and stored by $N$ deterministic peer nodes ($\text{BLAKE3}(\text{NodeID} \parallel \text{Epoch})$).
  - Request threshold signatures ($k$-of-$N$) from peer attestor neighborhood when claiming domains on `register.plain`.
- [ ] **Blind Balance & Privacy Preservation**
  - Use blind signatures/commitments so peer attestors log credit volume without linking node owners to specific browsing actions or domain claims.

---

## 5. Dynamic Rolling Average Domain Pricing & Gossip Merging (`pwb-node::domain`)

- [ ] **Rolling Average Pricing Engine**
  - Replace fixed constant `DOMAIN_CLAIM_COST` with a dynamic pricing formula based on rolling network metrics:
    - Total active domain registrations in recent epochs.
    - Global network work capacity and total node count.
    - Credit velocity and demand curve.
- [ ] **Gossip Partition & Network Merge State Reconciliation**
  - Implement deterministic state reconciliation when two isolated P2P network clusters merge (gossip sync).
  - Compute weighted moving averages for domain base costs and merge domain ownership registries using Conflict-Free Replicated Data Types (CRDTs).

---

## 6. Testing, Validation & Verification

- [ ] **Unit & Integration Tests**
  - Write test suite in `pwb-core` covering onion encryption/decryption layers and dummy hop generation.
  - Test peer discovery via mock UDP sockets and link/QR payload parsers.
  - Test credit neighborhood threshold signature verification and domain claim spending.
  - Test gossip partition merge algorithms for rolling average domain prices.
- [ ] **Live Workspace Verification**
  - Run `cargo test --workspace` and `cargo check --workspace` to ensure clean build across all crates (`pwb-core`, `pwb-node`, `pwb-browser`, `pwb-web`).
