# Design — §3 Decentralized Node Discovery (`pwb-core::discovery`)

## Objective

Auto-discover local peers on LAN without centralized servers, and support
out-of-band node pairing via connection links and QR codes.

## Components

### `pwb-core::discovery` — Local UDP Broadcast & IP Multicast

Signed 112-byte beacons (`"PWBD"` magic + version + kind + listen port + unix
timestamp + Ed25519 public key + signature over the header) exchanged over a
single bound UDP socket at `DISCOVERY_PORT=7922`.

#### Transport

- **Broadcast fanout** — beacons go to `255.255.255.255:7922` (UDP broadcast), reaching every host on the LAN subnet
- **Multicast group** — beacons are also sent to (and received from) `239.255.0.1:7922` (`join_multicast_v4` best-effort with loopback enabled); only sockets that joined the group exchange this traffic
- **Optional direct unicast** to known peers
- Success criterion: "≥1 datagram sent" — a host without multicast routing still works via broadcast

#### Trust model (`Beacon::verify`)

1. Framing/size check
2. Freshness (`BEACON_MAX_AGE=120s` clock-skew window), checked *before* the signature to reject unauthenticated old data cheaply
3. Ed25519 signature

Garbage, tampered, forged, stale, and future-dated beacons are all rejected with typed `BeaconError`/`DiscoveryError` reasons.

#### Peer table

- `merge` is address-sticky within `ADDR_REBIND_GRACE=300s`: a replayed old beacon cannot redirect an existing peer's advertised port until the grace window expires (anti-replay redirect)
- `spawn()` runs a tokio loop: periodic announce every `BEACON_INTERVAL=30s` + continuous recv, both merging into a shared `Arc<Mutex<HashMap<DhPublicKey, PeerInfo>>>`
- All sockets bind port 0 in tests (no SO_REUSEPORT / new deps)

### Connection Links & URI Handlers

Support parsing and launching node connections via
`plain://nodes.plain?connect=<ip:port>/<pubkey>` URIs.

- Parsing pre-existing in `pwb-core::system` (`build_connection_link` / `parse_connection_link`) and `NodeEngine::parse_connection_link`
- `NodeEngine::connect_to(addr, pubkey)`:
  - Validates address (non-zero port, not unspecified, not self-key)
  - Registers/refreshes `PeerInfo` in the engine's peer table (preserving reputation on refresh)
  - Returns the stored `PeerInfo`
- `NodeEngine::launch_connection(link)`:
  - Parses a `plain://nodes.plain?connect=...` URI via `system::parse_connection_link`
  - Delegates to `connect_to`; returns `PeerInfo` or `NetworkError::InvalidConnectionLink`
- `NetworkError::InvalidConnectionLink(String)` variant for malformed links / invalid addresses / self-connection attempts

#### CLI handlers

- `pwb-browser/src/main.rs`: positional argument that parses as a connection link launches a `NodeEngine`, calls `launch_connection`, and reports the registered peer
- `pwb-node/src/main.rs`: `--connect <link>` flag accepts a connection link after `--connect` / `-c`, launches the same flow, prints the connected peer, and sends a `PeerDiscovery` announcement so the connection is bidirectional
- Help text in both binaries updated with `plain://nodes.plain?connect=...` example

### NodeEngine integration

`pwb-core::net::NodeEngine::start()` now binds a UDP socket and spawns:
- a **sender loop** that transmits queued `(SocketAddr, bytes)` datagrams to peers,
- a **receiver loop** that deserializes inbound `NetworkMessage` JSON datagrams and hands them to the handler loop,
- a **peer maintenance loop** that evicts stale peers after 5 minutes,
- a [`DiscoveryService`] sharing the engine's [`PeerInfo`] table, so LAN beacons (broadcast + multicast on port 7922) immediately populate the peer table.

`NodeEngine::send_to` and `NodeEngine::broadcast` serialize [`NetworkMessage`] as JSON and transmit it to every registered peer's UDP address, in addition to local in-process enqueue (preserving existing behavior). `NodeEngine::generate_connection_link` uses the engine's actual bound socket address when started, so connection links always contain a dialable endpoint.

### QR Code Encoding & Scanning Support

From-scratch byte-mode QR encoder for versions 1..=10 at error-correction level M (no new dependencies).

#### Encoder

- Spec tables for Reed–Solomon block layouts, alignment centres, BCH(15,5)/BCH(18,6) format & version codes, the eight data masks, and ISO penalty rules 1–4 (best mask wins)
- `QrCode::encode` / `encode_bytes` / `encode_connection_link` pick the smallest fitting version (level-M capacities 14..=213 bytes, 16-bit count field from v10)
- Renderers: `bitmap()` (binary generator incl. `QUIET_ZONE=4`), `to_svg()`, `to_ascii()`

#### Correctness

Pinned by golden spec vectors (all 8 level-M format strings, v7/v10 version-info strings), the free-module-count identity (non-function cells == total-codeword bits + remainder bits, versions 1–10), block-table consistency, and a test-only full decoder that:
- Re-derives the function mask
- Verifies both format copies' BCH remainder (level M + mask)
- Re-checks version info
- Streams the zigzag order back
- De-interleaves
- Validates every block's Reed–Solomon remainder
- Parses the byte-mode segment

Roundtrips across v1/v5/v7/v8/v9/v10, UTF-8 payloads, and IPv4/IPv6 connection links.

#### Payload parser

`qr::parse_pairing_payload` maps a scanned string back to `(SocketAddr, PublicKey)`:
- Canonical `plain://nodes.plain?connect=...` links plus the compact `<ip:port>/<hex>` form (whitespace-tolerant)
- Garbage / short keys / bad addresses → `None`

#### CLI

- `pwb-node` prints a scan-to-pair ASCII QR of its own connection link
- `--connect` accepts either a URI or a scanned QR payload (canonical parse first, payload fallback, typed `InvalidConnectionLink` otherwise)

### `pwb-core::dht` — Distributed Kademlia / DHT Peer Table

Classic XOR-metric Kademlia table over 256-bit `NodeId`s (BLAKE3 of the Ed25519 pubkey):
- 256 k-buckets (`K_BUCKET_SIZE=8`)
- Bucket index = common-prefix length with the local ID
- Head = least-recently-seen / tail = most-recent
- `closest()` = XOR-distance-ordered contacts

#### Ping/pong health checks

Closed loop over caller-supplied time (no sleeps/sockets; transport stays with the caller):
- `update` never silently displaces a live node — a full bucket surfaces its LRU head as `UpdateOutcome::PingLeastRecent`
- Caller pings and answers with:
  - `mark_alive` (pong: refresh, move to tail, EWMA RTT sample α=1/8, reset failures)
  - `mark_failed` (timeout: after `MAX_FAILURES=3` consecutive failures the entry is evicted and the pending contact re-inserts)
- `stale_for_check` returns entries quiet for `STALE_AFTER=900s` for proactive ping sweeps

#### Latency-weighted bucket selection

`select_contacts` walks buckets outward from the target's bucket (Kademlia nearest-first order) and samples without replacement with weight ∝ 1/(1+RTT-ms). `select_contacts_with` takes an RNG for deterministic tests/simulations.
