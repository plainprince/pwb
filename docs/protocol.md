# PlainWeb Protocol Reference

Critical protocol constants, network parameters, and security rules. These
values are defined in source (`pwb-core/src/onion.rs`, `pwb-core/src/discovery.rs`,
`pwb-node/src/domain.rs`, `pwb-node/src/credit.rs`) and pinned by tests.

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

---


## 2. Network message transport (pwb-core::net)

PlainWeb nodes exchange NetworkMessage datagrams over UDP. Each
NodeEngine binds a UDP socket and runs three loops:

- Sender — transmits queued (SocketAddr, Vec<u8>) datagrams to registered peers.
- Receiver — deserializes inbound JSON datagrams as NetworkMessage and enqueues them for the handler loop. Invalid framing is silently dropped.
- Peer maintenance — evicts peers quiet for more than 5 minutes.

Messages are serialized with serde_json. NetworkError::PeerNotFound is returned when sending to a peer not in the peer table. NodeEngine::broadcast fans a message out to every registered peer's UDP address.

### Connection links

NodeEngine::generate_connection_link produces a plain://nodes.plain?connect=<ip:port>/<pubkey_hex> URI using the engine's actual bound socket address. NodeEngine::launch_connection parses such a link, validates it (non-zero port, specified address, not self-key), and registers the peer in the engine's peer table. A NetworkMessage::PeerDiscovery announcement is sent to the new peer so the connection is bidirectional.


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
| `INITIAL_CLAIM_PRICE` | `50` credits | Fresh engine starting price (the former fixed constant, now just the initial value). |
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

## 5. Reserved system TLD security rule (`.plain`)

The `.plain` TLD is **strictly reserved** for built-in system protocols. No user, miner, or node can claim, register, or overwrite `.plain` domains.

| Domain | Role |
|--------|------|
| `*.plain` | Built-in system endpoints only (`search.plain`, `devtools.plain`, `register.plain`, `gossip.plain`, `nodes.plain`). Registration attempts by network participants are rejected. |
| `*.pwb` | User network domains, claimed by participants using sparse credits (e.g. `welcome.pwb`, `mysite.pwb`). |

---

## 6. Credit & attestation summary (`pwb-node::credit`, `pwb-node::ledger`)

- Credits are earned strictly by delivering verified network services (serving `search.plain` queries, indexing PML bundles, relaying onion cells).
- Minting is rate-limited by real network traffic flow (`traffic_per_credit`) — credits can never outrun observed network flow (anti-ASIC floor).
- Node balances are not self-stored; they are attested and stored by `N` deterministic peer nodes (`BLAKE3(NodeID || Epoch)` shard coordinate). A subject is never its own attestor.
- Domain claims on `register.plain` require a `k`-of-`N` quorum certificate over one identical statement; the same certificate cannot fund multiple claims.
