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
- [ ] **Unified System Protocol Resolver**
  - Centralize native `.plain` system domain handlers (`search.plain`, `devtools.plain`, `register.plain`, `gossip.plain`, `nodes.plain`) inside `pwb-core::system`.

---

## 2. Onion Routing & Encryption Layer (`pwb-core::onion`)

- [ ] **Layered Sphinx-Style Onion Encapsulation**
  - Implement fixed-size packet cell framing (e.g. 512 bytes) to obscure query payloads and packet types.
  - Encrypt packet headers and payloads with multi-layer ChaCha20-Poly1305 symmetric ciphers using ephemeral X25519 keys per relay hop.
- [ ] **Cryptographically Random Hop Counts & Dummy Nodes**
  - Generate randomized relay path lengths for every request ($N \ge 3$ active hops + randomized dummy relay hops).
  - Intermediate nodes cannot determine if the next hop is another relay or the final destination.
- [ ] **IP & Transport Metadata Protection**
  - Mask source IP headers at intermediate hops; unencryptable metadata (like raw sockets) yields no actionable tracking information.
  - Implement cover traffic (decoy onion packets) dispatched at randomized intervals.

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
