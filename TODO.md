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

- [x] **Local UDP Broadcast & IP Multicast**
  - Auto-discover local peers on LAN via UDP broadcast beacons and IP multicast groups (`239.255.0.1:7922`).
    - Implemented in `pwb-core::discovery`: signed 112-byte beacons (`"PWBD"` magic + version + kind + listen port + unix timestamp + Ed25519 public key + signature over the header) exchanged over a single bound UDP socket at `DISCOVERY_PORT=7922`.
    - `DiscoveryService::announce` fans out each beacon to both `255.255.255.255:7922` (UDP broadcast) and `239.255.0.1:7922` (IP multicast, `join_multicast_v4` best-effort with loopback enabled), plus optional direct unicast to known peers; success is "≥1 datagram sent" so a host without multicast routing still works via broadcast.
    - Trust model: `Beacon::verify` checks framing/size, freshness (`BEACON_MAX_AGE=120s` clock-skew window, checked *before* the signature to reject unauthenticated old data cheaply), then the Ed25519 signature — garbage, tampered, forged, stale, and future-dated beacons are all rejected with typed `BeaconError`/`DiscoveryError` reasons.
    - `merge` is address-sticky within `ADDR_REBIND_GRACE=300s`: a replayed old beacon cannot redirect an existing peer's advertised port until the grace window expires (anti-replay redirect).
    - `spawn()` runs a tokio loop: periodic announce every `BEACON_INTERVAL=30s` + continuous recv, both merging into a shared `Arc<Mutex<HashMap<DhPublicKey, PeerInfo>>>`; all sockets bind port 0 in tests (no SO_REUSEPORT/new deps).
- [x] **Connection Links & URI Handlers**
  - Support parsing and launching node connections via `plain://nodes.plain?connect=<ip:port>/<pubkey>` URIs.
    - Parsing pre-existing in `pwb-core::system` (`build_connection_link` / `parse_connection_link`, §1) and `NodeEngine::parse_connection_link`.
    - Added in `pwb-core::net::NodeEngine`: `connect_to(addr, pubkey)` — validates address (non-zero port, not unspecified, not self-key), registers/refreshes `PeerInfo` in the engine's peer table (preserving reputation on refresh), returns the stored `PeerInfo`. `launch_connection(link)` — parses a `plain://nodes.plain?connect=...` URI via `system::parse_connection_link`, delegates to `connect_to`, returns `PeerInfo` or `NetworkError::InvalidConnectionLink`.
    - `NetworkError::InvalidConnectionLink(String)` variant added for malformed links / invalid addresses / self-connection attempts.
    - Tests: 10 new `test_launch_connection_*` / `test_connect_to_*` covering valid links, garbage, self-link, port-zero, unspecified address, reputation preservation on refresh.
    - CLI URI handler in `pwb-browser/src/main.rs`: when the positional argument parses as a connection link, the binary launches a `NodeEngine`, calls `launch_connection`, and reports the registered peer.
    - CLI `--connect <link>` flag in `pwb-node/src/main.rs`: accepts a connection link after `--connect` / `-c`, launches the same flow, prints the connected peer.
    - Help text in both binaries updated with `plain://nodes.plain?connect=...` example.
- [x] **QR Code Encoding & Scanning Support**
  - Add QR code binary generator and string payload parser for out-of-band node pairing (mobile/desktop peer exchange).
    - Implemented in `pwb-core::qr`: from-scratch byte-mode QR encoder for versions 1..=10 at error-correction level M (no new dependencies) — spec tables for Reed–Solomon block layouts, alignment centres, BCH(15,5)/BCH(18,6) format & version codes, the eight data masks, and ISO penalty rules 1–4 (best mask wins). `QrCode::encode`/`encode_bytes`/`encode_connection_link` pick the smallest fitting version (level-M capacities 14..=213 bytes, 16-bit count field from v10); renderers: `bitmap()` (binary generator incl. `QUIET_ZONE=4`), `to_svg()`, `to_ascii()`.
    - Correctness pinned by golden spec vectors (all 8 level-M format strings, v7/v10 version-info strings), the free-module-count identity (non-function cells == total-codeword bits + remainder bits, versions 1–10), block-table consistency, and a test-only full decoder that re-derives the function mask, verifies both format copies' BCH remainder (level M + mask), re-checks version info, streams the zigzag order back, de-interleaves, validates every block's Reed–Solomon remainder, and parses the byte-mode segment — roundtrips across v1/v5/v7/v8/v9/v10, UTF-8 payloads, and IPv4/IPv6 connection links.
    - `qr::parse_pairing_payload` maps a scanned string back to `(SocketAddr, PublicKey)`: canonical `plain://nodes.plain?connect=...` links plus the compact `<ip:port>/<hex>` form (whitespace-tolerant); garbage/short keys/bad addresses → `None`.
    - CLI: `pwb-node` prints a scan-to-pair ASCII QR of its own connection link, and `--connect` accepts either a URI or a scanned QR payload (canonical parse first, payload fallback, typed `InvalidConnectionLink` otherwise).
    - Tests: 16 new `qr::tests::*`; `qr.rs` coverage 99.61% lines / 99.80% regions.
- [x] **Distributed Kademlia / DHT Peer Table**
  - Maintain peer routing tables with ping/pong health checks and latency-weighted bucket selection.
    - Implemented in `pwb-core::dht`: classic XOR-metric Kademlia table over 256-bit `NodeId`s (BLAKE3 of the Ed25519 pubkey) — 256 k-buckets (`K_BUCKET_SIZE=8`), bucket index = common-prefix length with the local ID, head = least-recently-seen / tail = most-recent, `closest()` = XOR-distance-ordered contacts.
    - Ping/pong health checks are a closed loop over caller-supplied time (no sleeps/sockets; transport stays with the caller): `update` never silently displaces a live node — a full bucket surfaces its LRU head as `UpdateOutcome::PingLeastRecent`; the caller pings and answers with `mark_alive` (pong: refresh, move to tail, EWMA RTT sample α=1/8, reset failures) or `mark_failed` (timeout: after `MAX_FAILURES=3` consecutive failures the entry is evicted and the pending contact re-inserts). `stale_for_check` returns entries quiet for `STALE_AFTER=900s` for proactive ping sweeps.
    - Latency-weighted bucket selection: `select_contacts` walks buckets outward from the target's bucket (Kademlia nearest-first order) and samples without replacement with weight ∝ 1/(1+RTT-ms) (`select_contacts_with` takes an RNG for deterministic tests/simulations).
    - Tests: 14 new `dht::tests::*` — XOR/prefix metrics, tail-move refresh, LRU-ping surface + full pong/pong-replacement cycle, stale sweep threshold, capacity invariant, outward candidate walk, seeded 500-draw statistical weighting (1ms vs 900ms peers), and a real UDP ping/pong over loopback sockets whose measured RTT seeds the table and EWMA. `dht.rs` coverage 99.39% lines / 99.71% regions.

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
