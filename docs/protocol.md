# PlainWeb Protocol Reference

Critical protocol constants, network parameters, and security rules. These
values are defined in source (`pwb-core/src/onion.rs`, `pwb-core/src/discovery.rs`,
`pwb-node/src/domain.rs`, `pwb-node/src/credit.rs`) and pinned by tests.

---

## 0. Artifact container (`pwb-core::wire`)

A `.pwb` artifact is one compressed file:

| Field | Size | Meaning |
|-------|------|---------|
| magic | 4 | `PWBZ` |
| compression version | 1 | `1` = zlib-deflate payload |
| uncompressed length | 4 | `u32` little-endian |
| digest | 32 | BLAKE3 of the uncompressed `PWB1` bytes |
| payload | variable | zlib stream of the `PWB1` bundle |

Decompression is bounded at 64 MiB and verifies the digest, so a corrupted or
hostile artifact fails instead of inflating without limit. Legacy uncompressed
`PWB1` artifacts are accepted for backwards compatibility. Content IDs are
computed over the artifact bytes as stored (the `PWBZ` container when compressed).

---

## 1. Onion routing layer (`pwb-core::onion`)

Sphinx-style fixed-size onion cells over UDP. Every cell has an identical wire
image so traffic analysis cannot distinguish query type, path length, hop count,
or payload size.

| Constant | Value | Meaning |
|----------|-------|---------|
| `CELL_SIZE` | `512` bytes | Fixed onion cell size. Stays below IP/UDP MTU (no fragmentation); every datagram is exactly this size. |
| `ONION_VERSION` | `1` | Cell format version written into the first byte. |
| `MIN_HOPS` | `3` | Minimum relays on a built path (entry .. exit inclusive). |
| `MAX_LAYER_SLOTS` | `6` | Maximum layer slots (and therefore onion hops) per cell. |
| `PAYLOAD_MAX` | `118` bytes | Maximum payload bytes that fit in one cell. Larger payloads are split across multiple cells. |

### Path rules

- Every active path contains **3..=6 distinct** relays (enforced by `validate_path`).
- A uniformly random number of dummy relays is scattered over the route's interior gaps; entry and exit are preserved.
- Intermediate relays cannot determine whether their next hop is another relay or the final destination — the only observable is the peeled cell's action (`FORWARD`/`DELIVER`) plus forward framing; no position, path length, or "final hop" flag exists.

### Cover traffic

Decoy onion packets are dispatched at exponentially distributed, clamped intervals (inverse-CDF sampling, memoryless, fresh delay each firing — no fixed period). Decoys use ordinary `cell_kind` 0 cells with uniformly random payloads of random length (up to `PAYLOAD_MAX`), wire-indistinguishable from real requests.

### Multi-cell framing and return circuits

A single cell carries at most `PAYLOAD_MAX=118` bytes, so one logical message is
split into independently encapsulated cells:

```
cell payload = "PON1" | message id (16) | total len (2) | index (2) | count (2) | chunk
inner message = "PON4" | kind (1) | request id (16) | sender key (32)
              | return-hop count (1) + hops | payload len (4)
              | Ed25519 signature (64) | JSON message | random padding
return hop   = family (1) | address | port (2) | relay X25519 key (32)
```

The originator signs the reassembled inner message (domain prefix
`PWBONION1`), including the declared payload length and its random padding, so
the exit attributes the request to a key without learning an address.
Reassembly is bounded (256 assemblies, 2 MiB buffered, 30 s TTL) and the outer
fragment id is independent of the request id.

Every message is padded before fragmentation to a fixed 4 KiB envelope, or to
the 48 KiB maximum when it does not fit. The fragment count therefore takes
only one of two values per circuit and cannot fingerprint a bulk transfer's
size; recipients read `payload len` and discard the authenticated padding.

A request may carry a **return path**: the relays to traverse (entry first,
ending at the originator's own onion key) plus the entry hop's dial address.
The originator derives it from the outbound path without its exit (the
responder), reversed, so an exit replies to its previous hop and never sees the
origin. `OnionKind::Reply` frames reuse the request id and carry no return
path; `NodeEngine::send_onion_request` waits for the correlated reply, and
`NodeEngine::send_onion_reply` answers an `OnionRequest`. `pwb-node
--private-search` uses this circuit for `search.plain` queries, and the node
answers onion-delivered `SearchQuery`/`FetchArtifact` requests.

---


## 2. Network message transport (pwb-core::net)

PlainWeb nodes exchange NetworkMessage datagrams over UDP. Each
NodeEngine binds a UDP socket and runs four loops:

- Sender — transmits queued (SocketAddr, Vec<u8>) datagrams to registered peers.
- Receiver — verifies the datagram's signed envelope, reassembles fragments, drops replays, and enqueues the decoded NetworkMessage for the handler loop. Invalid framing, bad signatures, and replayed messages are silently dropped.
- Handler — forwards authenticated messages to application subscribers and answers `Ping` with `Pong`.
- Peer maintenance — pings every peer each heartbeat and evicts peers quiet for more than `PEER_TIMEOUT` (5 minutes).

### Envelope and fragmentation

Every datagram is `header (70 bytes) | Ed25519 signature (64) | payload`.
The header carries magic/version `PWBNET01`, fragmentation fields (index,
count, total length), a monotonically increasing per-sender sequence number,
a unix timestamp, and the sender's Ed25519 public key. The signature covers
the header and the payload, so sender identity, sequence, and contents are all
authenticated. Receivers attribute messages to that key (never to an identity
field inside the JSON payload) and reject unsigned variants whose embedded
sender/publisher/owner field disagrees with it.

Replay protection is a per-sender 64-sequence sliding window. Messages larger
than one datagram are split into chunks of at most
`1200 − 134 = 1066` payload bytes and reassembled with bounded state:
4 MiB per message, 4096 fragments, 30 s reassembly TTL, 8 MiB global budget.
Timestamps more than 10 minutes in the future are rejected; old timestamps are
not, because the sequence window already rejects replays.

Messages are serialized with serde_json inside the envelope. NetworkError::PeerNotFound
is returned when sending to a peer not in the peer table. NodeEngine::broadcast
fans a message out to every registered peer's UDP address. Sending never
delivers locally, and messages signed with the local key are ignored on receipt.

**Payloads are authenticated and encrypted.** Each datagram is sealed to the
recipient with a fresh ephemeral X25519 key and ChaCha20-Poly1305; only the
holder of the recipient identity can decrypt it, and the Ed25519 signature over
the header plus ciphertext authenticates the sender. Onion cells share the UDP
socket as fixed 512-byte datagrams, are detected before envelope decoding, and
are relayed hop-by-hop (see §1).

### Signed payloads

The envelope authenticates the *sender*, but a relay could still rewrite
content while keeping a truthful envelope identity. Payloads that carry data
attributed to a peer are therefore signed separately and verified on receipt:

- `GossipAnnounce` — publisher signature over
  `gossip_announce_sign_message(content_id, title, publisher)`.
- `ShardQuery` — requester signature over
  `shard_query_sign_message(term, query_id, sender, challenge)` (prefix
  `PWBSHARDQ1`). The serving node verifies it before recording the challenge
  nonce, so a server cannot manufacture a challenge for itself and then mint
  against it. The challenged nonce is TTL-bound and claim-once.
- `StorePosting` — publisher signature over `posting_sign_message(posting, publisher)`;
  unsigned or wrongly signed postings are never indexed.
- `ShardResponse` — responder signature over
  `shard_response_sign_message(query_id, term, postings, challenge, responder)`.
  The requester also requires the responder to be a responsible node for the
  term, and when a `QueryReceiptWire` is supplied it checks it against the
  challenge, term, and result set.
- `DomainRecordWire` / `ClaimDomain` — owner signature over the canonical claim,
  which includes `credits_spent` and `registered_at_epoch`. Registry-sync and
  domain-resolution responses reject unsigned or forged records. Claim epochs
  are unix seconds; receivers reject epochs more than 300 s in the future and
  merges keep the highest epoch (ties on the smaller owner key).

### Kademlia lookups

The engine keeps a 256-bucket Kademlia routing table populated from
authenticated peers and `PeerExchange`/`Nodes` messages. `FindNode` returns the
responder's 8 closest contacts for a target; `NodeEngine::find_node` runs an
iterative walk (`ALPHA=3` closest unqueried contacts per round, `K=20` result
set) until the closest known contacts have all answered, no closer unseen
contact appears, or the caller's timeout expires. Shard placement in
`DistributedSearch` reads the same live table instead of the flat peer list.

### Connection links and liveness

NodeEngine::generate_connection_link produces a plain://nodes.plain?connect=<ip:port>/<pubkey_hex> URI using the engine's actual bound socket address. NodeEngine::launch_connection parses such a link, validates it (non-zero port, specified address, not self-key), and registers the peer in the engine's peer table. Use NodeEngine::dial to prove reachability: it sends a signed `Ping` and waits for a matching `Pong` from that address. `connect_bootstrap_verified` dials every configured bootstrap in parallel and returns only peers that answered; plain `connect_bootstrap` only registers them.

`PeerDiscovery` carries keys without addresses, so receivers do not register
those keys at the sender's address; use `PeerExchange`, which carries
dialable addresses.


## 2. Node discovery layer (`pwb-core::discovery`)

LAN auto-discovery via UDP broadcast beacons and IP multicast.

| Constant | Value | Meaning |
|----------|-------|---------|
| `DISCOVERY_PORT` | `7922` | UDP port for broadcast and multicast beacons. |
| Multicast group | `239.255.0.1` | Only sockets that joined the group exchange this traffic. |
| Broadcast address | `255.255.255.255` | Reaches every host on the LAN subnet. |
| `BEACON_INTERVAL` | `30s` | Period between a node's own beacon announcements. |
| `BEACON_MAX_AGE` | `120s` | Beacon timestamps outside this window are rejected (stale replays and future-dated floods). |
| `ADDR_REBIND_GRACE` | `300s` | A replayed old beacon cannot redirect an existing peer's advertised port until this grace window elapses (anti-replay redirect). |

### Beacon format

112-byte signed beacons: `"PWBD"` magic + version + kind + listen port + unix timestamp + Ed25519 public key + Ed25519 signature over the header. Verification order: framing/size → freshness → signature (rejects unauthenticated old data cheaply).

---

## 4. Domain claim pricing (`pwb-node::domain::RollingPricing`)

Rolling average pricing engine replacing any fixed claim cost. Each epoch the price moves one `ADJUSTMENT_DIVISOR`-th of the way toward `price × demand / supply`, all integer math, clamped to bounds.

| Constant | Value | Meaning |
|----------|-------|---------|
| `INITIAL_CLAIM_PRICE` | `10` credits | Fresh engine starting price; indexing a hosted bundle of at least 640 compressed bytes clears the traffic floor and covers a first claim. |
| `MIN_CLAIM_PRICE` | `10` credits | Lower bound on the claim price. |
| `MAX_CLAIM_PRICE` | `10,000` credits | Upper bound on the claim price. |
| `PRICING_WINDOW` | `8` epochs | Per-epoch observations retained in the rolling window; stale/duplicate epochs are ignored. |
| `TARGET_REGS_PER_EPOCH` | `4` | Registration flow that reads as balanced demand (`PRICE_SCALE` = balanced). |
| `BASELINE_WORK_PER_NODE` | `1,000` | Per-node credits minted per epoch at balanced conditions. |
| `BASELINE_NODES` | `10` | Node count at balanced conditions. |
| `PRICE_SCALE` | `1,000` | Fixed-point scale: `1,000` = "balanced". |
| `ADJUSTMENT_DIVISOR` | `4` | Share of the gap to the pressure target closed per epoch. |

### Price formula

```
avg_regs     = sum(registrations) / window_len
avg_work     = sum(network_work) / window_len
avg_nodes    = max(sum(node_count) / window_len, 1)

demand       = avg_regs × PRICE_SCALE / TARGET_REGS_PER_EPOCH
work_per_node= avg_work / avg_nodes
capacity     = work_per_node × PRICE_SCALE / BASELINE_WORK_PER_NODE
breadth      = avg_nodes × PRICE_SCALE / BASELINE_NODES
supply       = max(capacity × breadth / PRICE_SCALE, 1)

pressure     = demand × PRICE_SCALE / supply
target       = price × pressure / PRICE_SCALE
new_price    = clamp(price + (target - price) / ADJUSTMENT_DIVISOR,
                     MIN_CLAIM_PRICE, MAX_CLAIM_PRICE)
```

- Demand > supply raises the price over epochs; demand < supply lowers it.
- A fresh engine starts at `INITIAL_CLAIM_PRICE`; balanced demand and supply keep the price stable.
- `DomainRegistry::record_pricing_epoch()` folds one epoch of network-wide stats into the rolling price; `claim_cost()` returns the current price.

---

## 5. Official (`.plain`) vs unofficial (TLD list) namespaces

`.plain` is the **official** namespace and is **strictly reserved**. It holds the built-in system protocols and the browser-shipped content pages. No user, miner, or node can claim, register, or overwrite `.plain` domains.

| Domain | Role |
|--------|------|
| `*.plain` | Official built-in pages only: system endpoints (`search.plain`, `devtools.plain`, `register.plain`, `gossip.plain`, `nodes.plain`, `network.plain`, `help.plain`) and shipped content pages (`welcome.plain`, `demo.plain`). Registration attempts by network participants are rejected. |
| `*.pwb`, `*.non`, `*.yep`, `*.lam`, `*.arch`, `*.vio`, `*.org`, `*.net`, `*.com` | **Unofficial** user/network sites, claimed by participants using sparse credits (e.g. `mysite.pwb`, `club.non`). The list lives in `UNOFFICIAL_TLDS` (`pwb-core::system`); names outside it are refused by every claim path. Full list and rules: [docs/tlds.md](tlds.md). |

### 5.1 Networked domain service (`pwb-node::domain::DomainService`)

`register.plain` is backed by a real, decentralized namespace rather than a local table:

- **Signed claims.** A claim is a canonical statement `domain → content-id` + owner + `credits_spent`, signed by the owner key (`domain_claim_sign_message`, prefix `PWBDOMAIN1`) and broadcast as `ClaimDomain`. Receivers re-derive the statement, verify the signature, reject reserved names, names outside the unofficial TLD list, and underpaid claims (`credits_spent < claim_cost()`), apply the registry's last-writer-wins merge, and re-gossip (flood with per-claim digest dedup, so propagation terminates).
- **Registry sync.** On startup a node broadcasts `RegistrySyncRequest`; peers answer with their full record set (`RegistrySyncResponse`), so late joiners converge without replaying history.
- **On-demand resolution.** An unofficial name not held locally is resolved by broadcasting `DomainQuery`; the first valid `DomainResponse` wins and is cached.
- **Persistence.** Records are mirrored to disk and reloaded on start.

Receivers verify the owner's signature and the declared price but not the signer's earned balance, so this is a functional namespace gated by the local spend; the `k`-of-`N` attestation path above is the Sybil-resistant extension.

---

## 6. Credit & attestation summary (`pwb-node::credit`, `pwb-node::ledger`)

- Credits are earned strictly by delivering verified network services (serving `search.plain` queries, indexing PML bundles, relaying onion cells).
- Minting is rate-limited by real network traffic flow (`traffic_per_credit`, default 64 verified bytes per credit) — credits can never outrun observed network flow (anti-ASIC floor). The traffic counted for a query is the serialized `ShardResponse` actually sent; indexing counts the bundle bytes; artifact serving counts the bytes sent.
- A node keeps its *local* balance in a signed, hash-chained ledger (`credits.json` beside `domains.json`). Entries sign `seq ‖ prev ‖ kind ‖ amount ‖ spent ‖ detail` with prefix `PWBCREDIT1`; `prev` is the digest of the previous signed entry. Loading stops at the first bad signature, broken link, or overdraft, so editing an amount cannot create spendable credits.
- Peer-attested balances are exchanged over the wire: `AttestBalance` / `BalanceAttest` gather a `k`-of-`N` `QuorumCertificate`, and `ClaimDomainAttested` carries it so every receiver re-verifies the attestors' signatures before applying the claim.
- Blind vouchers use a two-round network protocol (`WithdrawCommit` → `WithdrawNonces` → `WithdrawSign` → `WithdrawSignatures`). The issuer records the volume spend on its `AttestorLedger`; `ClaimDomainBlind` presents the unlinkable voucher plus its ownership proof, and receivers keep a spent-token log so a bearer voucher cannot be redeemed twice.
- A node can ask a peer to verify index work with `CheckIndexProof`; the verifier re-hashes the bundle bytes, re-derives the term hash, answers `ProofVerdict`, and records the verified earning in its attestor ledger. Every peer that relays an onion cell records that work, and `pwb-node` seals relayed-cell windows into signed `RelayReceipt` windows to mint relay credits into the signed ledger.
- Domain claims on `register.plain` still have a local signed-ledger path (`ClaimDomain`); the attested and blind paths add the Sybil-resistant claim variants described above.

---

## 7. Connection links & STUN (`plain://nodes.plain?connect=<ip:port>/<pubkey>`)

A connection link advertises a dialable address plus the node's Ed25519 public key, so another node can dial it directly.

### 7.1 Why links used to be `localhost`

The bind address is what STUN learns from. Nodes previously bound their UDP socket to `127.0.0.1`, which cannot reach a public STUN server, so `public_addr` stayed `None` and the link fell back to the loopback bound address — not dialable from the internet.

Nodes now bind to `0.0.0.0` by default (`--listen` overrides, e.g. `127.0.0.1:0` for localhost-only). On startup they query the STUN server for the public IP:port; the link uses that when available. When STUN is unavailable, the engine probes the OS routing table for a routable LAN IP for the bound port, so the link is at least dialable on the local network.

### 7.2 Persistent identity (`--identity`)

A node's peer ID is its Ed25519 public key, persisted to `$PWB_IDENTITY` (default `$HOME/.pwb/identity.json`) so it survives restarts. The identity file contains only the keypair: credits live in the signed ledger beside the domain registry under `--data` (`<data>/credits.json`), and `domains.json` no longer stores a `balance` field. A fresh node starts at zero credits and earns them by serving verified work. Claims are made explicitly with `--claim <name.pwb>` (optionally `--content <hex-cid>`), signed with the saved key, gossiped to the swarm, and recorded in the persisted domain registry (`--domains`, default `<data>/domains.json`). Nothing is claimed automatically on startup.

---

## 8. Batched shard writes (2026-10-22)

`StorePostings` carries up to `POSTINGS_BATCH_MAX = 64` postings for one
recipient, signed together under `PWBPOSTINGS1` (term, content id, title, and
term frequency for each entry, followed by the publisher key). Publishing a
bundle groups its terms by responsible shard and sends one message per batch
instead of one `StorePosting` per term, so a large document cannot exhaust the
per-peer token bucket (128 msg/s, 256 burst) and lose the tail of its index or
the follow-up `CheckIndexProof`. `StorePosting` remains accepted for
compatibility and single-term updates.

## 9. NAT traversal: hole punching and relay fallback (2026-10-22)

STUN only makes cone NAT mappings dialable. Two additional paths let peers
behind symmetric NAT or CGNAT communicate:

- **Rendezvous hole punching.** A peer sends `PunchRequest { target }` to a
  connected rendezvous. The rendezvous answers each side with `PunchSignal`
  carrying the other side's observed source address. Both sides emit signed
  113-byte probe datagrams (`PWBPUNCH1 || nonce || key || signature`) so their
  NAT mappings open, then prove the path with a normal `Ping`/`Pong`. Probes
  are per-source rate-limited and always answered with a same-size probe, so
  they cannot be used to amplify traffic. Only already-known peers are probed.
- **One-hop relay fallback.** `RelayDeliver { origin, origin_addr, target,
  datagrams }` wraps the originator's normal signed and encrypted envelopes
  inside a message addressed to a direct relay. The relay re-signs only the
  outer envelope and forwards the inner datagrams unchanged, so it cannot read
  or forge the payload; the recipient authenticates the original sender
  end-to-end, registers the origin at `origin_addr` with a relay route, and
  replies through the same relay. Relayed routes are cleared when a direct
  datagram from that peer arrives, and a relay target that is itself relayed is
  refused (no relay chains).

`NodeEngine::dial_resilient` tries direct dial, hole punch, then relay, in that
order. Connection links, remembered peers on restart, and the `nodes.plain`
connect action use it.

## 10. Registry sync funding certificates (2026-10-22)

`RegistrySyncResponse` carries `SyncRecordWire { record, certificate }`. When
a record cites `(funding_head, funding_balance)`, the receiver requires a
`QuorumCertificateWire` whose subject is the owner, whose `(head, balance)`
match the citation, whose balance covers `credits_spent`, and whose `k`-of-`N`
signatures verify against the receiver's deterministic attestor neighborhood
(`BLAKE3(owner NodeID || epoch)` ranking over live peers). Funded records whose
certificate is missing or invalid are ignored. Certificates are cached and
persisted with `domains.json` so they can be attached to later sync responses.
This closes the "invented balance" registry-sync path; the neighborhood is
still rooted in each node's live peer view, so bootstrap trust remains the
residual assumption.

## 11. Operational surface (2026-10-22)

`pwb-node` serves a loopback HTTP endpoint (`--health-addr`, default
`127.0.0.1:7711`; `--no-health` disables):

- `GET /health` — `200` while the UDP socket is bound, `503` otherwise.
- `GET /status` — JSON addresses, peer count, index/store/registry sizes,
  claim price, and earned credits.
- `GET /metrics` — Prometheus text for packet, message, rate-limit, replay,
  onion, relay, dial, and hole-punch counters.

`PWB_LOG_FORMAT=json` switches `pwb_core::telemetry::log_event` output to one
JSON object per line on stderr, for log shippers.
