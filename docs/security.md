# PlainWeb Security Model — Onion Routing

What the `pwb-core::onion` design protects, and — just as important — what it
does **not**. This document scopes the onion layer only; see
[§2 Onion Routing & Encryption](design/02-onion-routing.md) for the design and
[Protocol Reference](protocol.md) for pinned constants.

The onion layer is a Sphinx-style fixed-size cell network: every request travels
as a **512-byte** [`OnionCell`] whose routing slots and payload are encrypted in
independent layers, one per relay. Each hop peels its layer, shifts the slot
stack, and re-emits an identically-sized cell. Transport re-originates every
datagram from the forwarding relay's own socket.

Constants referenced below: `CELL_SIZE=512`, `ONION_VERSION=1`, `MIN_HOPS=3`,
`MAX_LAYER_SLOTS=6`, `PAYLOAD_MAX=118` (`pwb-core/src/onion/constants.rs`).

---

## What it protects against

### 1. Confidentiality of payloads to relays and observers

Each hop's layer key and payload sub-key are derived (BLAKE3 `derive_key`, separate
contexts) from the ECDH shared secret between the cell's **fresh per-cell
ephemeral X25519 key** and that relay's long-term X25519 key. Only the sender and
hop `i` can compute `shared_i`, so:

- An on-path observer cannot read any slot or the payload.
- An intermediate relay cannot read the payload — it is sealed to the exit hop's
  payload sub-key only.
- The exit hop decrypts the payload, but no earlier hop can.

### 2. Wire metadata: payload length, packet type, path length

Every cell is exactly `CELL_SIZE` bytes. The payload region is a **fixed-size**
plaintext (length prefix + payload + random padding) before sealing, so the
ciphertext length never reveals the plaintext size. `cell_kind` is framing only
and never encodes a query/packet type. Consequently an observer cannot learn the
query contents, its length, the packet type, the path length, or the hop count
from the wire image.

### 3. Hook position and "final hop" indistinguishability

Directives carry **no position, no path length, and no final-hop flag** — only
`FORWARD <next relay>` or `DELIVER`. Unused trailing slots are sender-random
padding that shifts forward as the stack is peeled, so a relay cannot tell where
the last real hop sits. Two worlds where a relay's next hop is (or is not) the
final destination produce a byte-identical observable; only the downstream
behaviour diverges. This is pinned by
`test_intermediate_cannot_tell_if_next_hop_is_destination`.

### 4. Origin IP masking at every hop

`pwb-core::net::CellChannel` binds a UDP socket per node and **re-originates**
each datagram from its own socket toward the next relay's registered address. The
envelope is *exactly the cell bytes* — there is no source-address field to carry,
spoof, or strip. A receiving hop therefore observes only the previous relay's IP;
the originator's address stops at the entry hop. `recv_cell` accepts only
exactly-`CELL_SIZE` datagrams.

### 5. Destination / path-length inference resistance

`randomize_path` builds each request's path from the caller's active route plus a
uniformly random number of dummy relays (0 ..= remaining slots), scattered over
the interior gaps. Entry stays first, exit stays last, the active route survives
as an ordered subsequence, and total length varies per request across
`MIN_HOPS..=MAX_LAYER_SLOTS`. Repeated requests over the same route are therefore
not linkable by length.

### 6. Timing and traffic analysis (raised cost)

Cover traffic (`CoverTraffic` / `CoverDispatch`) emits decoy cells that are
byte-for-byte shaped like real requests: ordinary `cell_kind` 0 cells over
randomized paths with uniformly random payloads of random length. Dispatch is
scheduled at **exponentially distributed, clamped intervals** (memoryless,
re-sampled every firing) so there is no fixed period for an analyst to lock onto.
This makes a single relay's traffic harder to characterize, but see the
counterpart in "not protect against" §4.

### 7. Recorded-cell forward secrecy

The per-cell ephemeral X25519 secret is generated for one cell and discarded
after encapsulation; the derived layer/payload keys are single-use, which is why
the AEAD can safely run with an all-zero nonce. A relay's long-term key leaking
*later* does not decrypt previously captured cells: recomputing `shared_i`
requires the per-cell ephemeral secret, which no longer exists.

### 8. Directive integrity / tamper resistance

Slots and the payload are ChaCha20-Poly1305 AEAD blobs. A relay only acts on a
front slot that authenticates under its layer key; a corrupted or misaddressed
front slot makes `peel` return `Ok(None)` and the relay silently drops the cell.
Altering a directive to redirect a forward cannot survive authentication at the
next hop.

---

## What it does **not** protect against

### 1. The exit hop sees the request

The payload is sealed to the **exit hop** — by construction the last relay
decrypts it. Unless the application adds its own end-to-end encryption *above*
the onion layer, the exit learns the query/content. The destination service is
part of the path, so it necessarily knows it was the recipient.

### 2. The entry hop sees the originator

The entry relay is the originator's first peer: it observes the originator's real
source IP and the timing/existence of the originator's traffic. Origin IP masking
protects the originator from *downstream* hops, not from the entry hop it chose.

### 3. Endpoint compromise

Nothing here defends against malware or an observer on the originator's or
destination's device, or a compromised browser/node process. The onion layer
starts above the local endpoint.

### 4. Global passive adversary / timing correlation

A network-wide adversary that observes **both** ends of a link can correlate the
distinctive timing (and count/rate) of a cell entering one relay with a cell
leaving another. Fixed cell sizes, path randomization, and cover traffic defeat
*content* and *length* analysis and raise the cost of timing analysis, but they
do not defeat a global observer that sees enough links. Cover traffic is also
optional and costs bandwidth; it only helps where it is actually running.

### 5. Colluding relays and Sybil path capture

Anonymity depends on path *selection*: `randomize_path` trusts the caller-supplied
active route and candidate pool. If an adversary controls a large fraction of
relays (Sybil) or holds the entry and exit positions (end-to-end collusion), it
can deanonymize. There are **no entry guards** / fixed first hops, so the
predecessor and traffic-confirmation attacks against random path selection remain
in scope.

### 6. No sender authentication — open participation

Directives authenticate the *cell contents*, not the *originator*. Any node that
knows a relay's long-term public key can encapsulate a cell addressed to it, and
an entry relay cannot tell a legitimate participant from an arbitrary sender.
There is no access control at the onion layer.

### 7. Replay and denial of service

Cells carry **no circuit id, counter, or replay cache** (header bytes 2..8 are
reserved zero, "future: circuit id, cover epoch, flags"). A captured cell can be
re-injected, and relays perform an ECDH + AEAD per cell with no onion-layer rate
limiting — a malicious relay or observer can drop, delay, reorder, replay, or
selectively forward cells. There are no delivery guarantees.

### 8. Multi-cell correlation for bulk transfers

`PAYLOAD_MAX` is 118 bytes per cell; larger transfers will fragment into
multi-cell streams above this layer (future work). Until that framing is defined,
volume-based correlation across the cells of one logical request is not addressed.

### 9. Non-onion metadata

Other PlainWeb subsystems intentionally expose identities out of band and are
outside onion protection:

- LAN `discovery` beacons are signed but announce a peer's public key and listen
  port to the local network.
- `plain://nodes.plain?connect=<ip:port>/<pubkey_hex>` connection links carry the
  address and key in the clear (they are meant to be shared out of band).
- QR pairing payloads encode the same connection link.

### 10. Cryptographic and implementation invariants

Security rests on X25519, ChaCha20-Poly1305, and BLAKE3 `derive_key`, and on two
hard invariants that must never be violated:

- **Path uniqueness** — `validate_path` rejects duplicate relays and
  `randomize_path` filters dummy candidates that duplicate the route. A relay
  appearing twice would derive one key used twice with the zero nonce, which is
  catastrophic AEAD nonce reuse. This is a correctness/security requirement, not
  a style rule.
- **Single-use keys** — every cell must use a fresh ephemeral keypair. Reusing an
  ephemeral key across cells would reuse derived keys.

Failures in RNG, key handling, or these invariants invalidate the guarantees
above. Weakened or misconfigured cover traffic (e.g. `mean_interval` set to zero)
reduces timing resistance.

### 11. Out of scope

This document covers the onion transport only. Content authenticity and
verification of `.pwb` bundles, domain/credit Sybil resistance, search-ranking
integrity, and censorship resistance are handled (or not) by other layers and
their own design docs.

---

## Summary

| Threat | Protected? |
|--------|-----------|
| Relay/observer reading payload contents | Yes |
| Payload length, packet type, path length on the wire | Yes |
| Distinguishing the final hop from an intermediate hop | Yes |
| Origin IP revealed to downstream relays | Yes |
| Length-based linkability across requests | Yes (randomized paths) |
| Timing characterization of one relay | Mitigated (cover traffic) |
| Recorded cells decrypted after a relay key leaks | Yes (per-cell forward secrecy) |
| Tampered directives surviving to the next hop | Yes (AEAD) |
| Exit hop learning the request | No (add end-to-end encryption above) |
| Entry hop learning originator IP | No |
| Compromised originator/destination endpoint | No |
| Global passive timing correlation | No |
| Colluding/Sybil relays, no guard nodes | No |
| Sender authentication / access control | No |
| Replay protection | No (planned circuit id) |
| DoS / rate limiting | No |
| Multi-cell bulk-transfer correlation | No (framing future) |
| LAN beacons & connection links | No (out-of-band by design) |

[`OnionCell`]: ../pwb-core/src/onion/cell.rs
