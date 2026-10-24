# Design — §2 Onion Routing & Encryption Layer (`pwb-core::onion`)

## Objective

Implement onion privacy so intermediate nodes cannot determine query contents,
packet types, path length, payload length, or whether their next hop is another
relay or the final destination.

## Components

### `pwb-core::onion` — Layered Sphinx-Style Onion Encapsulation

- Fixed-size packet cell framing (512 bytes) to obscure query payloads and packet types
- Multi-layer ChaCha20-Poly1305 symmetric ciphers using ephemeral X25519 keys per relay hop

#### Cell format

Fixed 512-byte cells:
- 40-byte clear framing
- 6 × 56-byte encrypted layer slots
- 136-byte encrypted payload region

The uniform wire image hides query, packet type, path length, and payload length.

#### Key derivation

Per-cell ephemeral X25519 key ECDHs against each relay's long-term key.
BLAKE3 `derive_key` splits independent layer/payload sub-keys (separate contexts),
so every AEAD runs a single-use key with a safe zero nonce.

#### Sphinx shift semantics

- Directives (`FORWARD <next relay>` / `DELIVER`) stack inside-out
- Relays peel the front slot and re-emit an identical-size cell with a re-randomized trailing slot
- Unused slots are sender-random padding that hides the true hop count

### Path rules

- `MIN_HOPS=3` .. `MAX_LAYER_SLOTS=6` paths via `validate_path` (unique-relay rule)
- Reserved `cell_kind` + 6 header bytes for cover traffic
- `PAYLOAD_MAX=118` for future multi-cell streams

### `pwb-core::onion` — Random Hop Counts & Dummy Nodes

- Randomized relay path lengths for every request (N ≥ 3 active hops + randomized dummy relay hops)
- `randomize_path` / `randomize_path_with`:
  - Take a caller-supplied active route (3..=6 distinct hops, enforced by `validate_path`) plus a candidate pool
  - Draw a uniform `0..=` room count of dummy relays
  - Scatter them over the route's interior gaps — entry/exit preserved, every relay unique, route survives as an ordered subsequence
  - Returns `RandomizedPath { relays, active_hops }` + `dummy_hops()`

### Indistinguishability

Proven by `test_intermediate_cannot_tell_if_next_hop_is_destination`:
- Two worlds where the middle relay's next hop is/isn't the final destination produce a byte-identical observable (only `PeeledCell`'s action + forward framing exist — no position, path length, or "final hop" flag)
- Downstream genuinely diverges (Deliver vs Forward), and the forwarded cell no longer authenticates for the middle relay
- 256-sample test locks in that lengths actually vary across requests over the same active route
- Validation tests cover short/duplicate/oversized routes, capacity-exact routes (0 dummies), and candidate pools overlapping or duplicating the route

### `pwb-core::net::CellChannel` — IP & Transport Metadata Protection

- Bound UDP socket carrying onion cells hop-by-hop
- Each relay re-originates the datagram from its own socket toward the next hop's registered address
- Envelope is exactly the cell bytes — no source-address field exists to carry or forge
- `recv_cell` enforces exactly `CELL_SIZE` datagrams (constant size)
- Unknown next hops refused with `PeerNotFound`

### Return circuits (`pwb-core::net::onion_transport`)

- One logical message is split into `PON1` cells (independent 512-byte onions) and reassembled at the exit with bounded state.
- An inner `PON4` message carries `kind` (request/reply), a random request id, the originator's Ed25519 key, an optional return path, and the real payload length.
- The inner message is padded to a fixed 4 KiB envelope, or to the 48 KiB maximum when it does not fit, so the cell count reveals only that coarse size class and never a bulk transfer's exact length. The signature covers the padding, which is discarded after verification.
- The return path lists the relays back to the originator (entry first, ending at its own onion key) plus the entry hop's dial address; the originator derives it from the outbound path without the exit (the responder).
- `NodeEngine::send_onion_request` registers the request id, retransmits across the timeout (UDP may drop any fragment), and resolves the correlated `OnionKind::Reply`; `send_onion_reply` answers a delivered `OnionRequest`.
- The exit only ever sees its previous relay: it sends the reply there, and each relay re-originates it toward the originator.
- `pwb-node --private-search` uses a three-relay circuit for `search.plain`, and the node answers onion-delivered `SearchQuery`/`FetchArtifact` requests.

### Client defaults

- `pwb-browser` sends `search.plain` queries, `.pwb` registry resolution, and
  content/artifact fetches over three-hop onion circuits by default. There is
  no silent direct fallback: a failed circuit reports the failure instead of
  leaking the request.
- `--direct` switches those lookups to the direct authenticated transport,
  which reveals the browser's address to the peer it talks to. It exists for
  LAN debugging and for users who accept that trade-off.
- `pwb-node --private-search` uses the same circuit and return-path code, so a
  query originated by either binary is answered through the same exit
  behaviour (the exit never learns the originator's address).

### Cover traffic

- `CoverDispatch` schedules decoys at exponentially distributed, clamped intervals:
  - Inverse-CDF sampling, memoryless
  - One decoy per due poll, fresh delay each firing — no fixed period for an analyst to lock onto
- `CoverTraffic`/`CoverConfig` build decoys over `randomize_path` paths with uniformly random payloads of random length:
  - Ordinary `cell_kind` 0 cells — wire-indistinguishable from real requests
- Typed `NetworkError::Onion` for decode failures
