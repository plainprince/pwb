# Production Readiness Audit

Date: 2026-10-18
Scope: all four crates (`pwb-core`, `pwb-web`, `pwb-node`, `pwb-browser`), their
binaries, and the design/protocol docs that describe them.

Method:

- Read every module in the workspace, focusing on what is *wired into a
  production binary* versus what only exists in tests or demo code.
- Ran `cargo test --workspace`: **389 tests pass** (132 core, 126 browser,
  97 node, 5 domain-claiming, 3 federated-search, 3 manual-network,
  4 merge, 4 network-join, 15 web).
- Ran `pwb-node` against a fresh identity and captured its startup output.

The test suite is green, but green tests here mostly prove that each module
matches its own unit-test expectations. They do not prove the pieces are
connected to each other or to the binaries.

## Bottom line

Single-process, local features are real: the bundle format, PML/PSS parsing,
the crypto primitives, the renderer, the JS runtime, local indexing, and the
domain-claim signature format all work.

**Every cross-node feature is partial or misleading in at least one
load-bearing way.** In its default configuration the network cannot discover
peers, cannot keep peers, cannot verify who sent what, cannot verify search
results, does not use the onion transport, and the full `pwb-node` daemon does
not run search or serve any system page. A large fraction of `pwb-node`'s
startup output is demo code operating on hardcoded values.

---

## Remediation status (2026-10-18, follow-up pass)

Steps 1–2 of the remediation order below were implemented after this audit.
The audit text is kept as the original snapshot; the table records what
changed.

| Original finding | Status | What changed |
|---|---|---|
| #1 unauthenticated JSON transport | **Fixed** | Every datagram carries a 70-byte header + Ed25519 signature (`pwb-core/src/net/envelope.rs`): sender identity, sequence number, timestamp, and payload are signed. Payloads are sealed to the recipient with ephemeral X25519 + ChaCha20-Poly1305, receivers attribute messages to the signing key (never to a JSON field), and replays are dropped with a per-sender 64-entry sliding window. Sequence numbers start from wall-clock microseconds on process start, so a restarted identity is not mistaken for a replayed old session. The onion layer is wired into the same socket and now supports request/reply circuits. |
| #2 discovery cannot work | **Fixed** | `NodeEngine::start` advertises the socket's actual bound port (not `listen_addr.port()`, which is 0), binds `DISCOVERY_PORT` (7922) for non-loopback nodes, falls back to an ephemeral port only when another local node holds 7922, and skips discovery for loopback-only nodes. `DiscoveryService` ignores its own beacons and beacons advertising port 0. |
| #3 peers expire after 5 minutes | **Fixed** | `last_seen` is refreshed on every authenticated datagram, and the maintenance loop pings peers each heartbeat so a Pong refreshes them too. Only genuine silence (`PEER_TIMEOUT`, 300 s) expires a peer. |
| #4 `send_to` self-injects | **Fixed** | `send_to`, `broadcast`, and `send_to_addr` only transmit. The local handler queue is fed exclusively by the receiver loop, and messages signed by the local key are dropped on receipt. |
| #5 joining is a local table insert | **Fixed** | Added `Ping`/`Pong` messages and `NodeEngine::dial`, which registers a peer, sends a signed Ping, and waits for a matching Pong from that address. Both binaries now use `connect_bootstrap_verified`, which dials bootstraps in parallel and reports only peers that answered; an unreachable bootstrap prints "No bootstrap peers reachable". `connect_bootstrap` remains for callers that only want registration and is documented as unverified. |
| #7 64 KiB single-datagram ceiling | **Fixed** | Messages are fragmented into ≤1200-byte signed datagrams and reassembled with bounded state (4 MiB message cap, 4096 fragment cap, 30 s TTL, 8 MiB global buffer budget, duplicate/out-of-order/conflicting fragments rejected). |
| #6 fetch ignores content addressing | **Fixed** | `DistributedSearch` drops any `ArtifactResponse` whose bytes do not hash to the claimed `content_id`; the bytes are neither stored nor indexed nor handed to a waiter. Covered by `test_fetch_rejects_artifacts_that_do_not_match_the_requested_content_id`. |
| `pwb-node` ran no search/system service (#8) | **Fixed** | The daemon now mounts `DistributedSearch` (shard queries, posting storage, artifact fetch) and `SystemDomainHandler` (`search.plain`, `FetchArtifact`), adds `--host <dir|.pwb>` to publish content and `--search <q>` for a one-shot federated query, and moves the synthetic workload behind `--demo`. A two-process run hosted a directory and served a query plus a 3982-byte artifact fetch. |
| No persistence for content or index (#13) | **Fixed** | `ArtifactHost::open(dir)` loads/writes content-addressed `.pwb` files; `SearchEngine::open(path)` snapshots postings/titles as JSON. Both use temp-file + fsync + rename (`pwb-node/src/persist.rs`). Verified across a real restart: the store reloaded 1 artifact and answered a shard query from the persisted index without `--host`. |
| Unsigned postings and shard responses | **Fixed** | `StorePosting` carries a publisher signature over `posting_sign_message`, and `ShardResponse` carries the responder's signature over `shard_response_sign_message(query_id, term, postings, challenge, responder)`. Receivers verify both; requesters additionally require the responder to be a responsible node and verify any receipt against the challenge/result set. |
| Unsigned registry records, constant claim epoch (#10/#11) | **Fixed** | `DomainRecordWire` carries the owner's signature over the canonical claim (now including `credits_spent` and `registered_at_epoch`); registry sync and `DomainResponse` reject unsigned/forged records. Claims are stamped with a monotonic unix epoch, future epochs beyond 300 s are rejected, and merges use the existing higher-epoch rule — so a later claim supersedes an earlier one instead of tying at `1`. |
| #14 default run prints verifier FAILED | **Fixed** | `IndexProof::sign` / `IndexProof::signing_message` are now the single source for the signed tuple, and `pwb-node` builds its proof through them, so the check prints `PASSED`. The synthetic credit/domain demo block was removed entirely. |
| Credits self-declared, self-issued challenges, editable balance (#9) | **Fixed** | `CreditLedger` (`pwb-node/src/credit/ledger.rs`) derives the balance from a signed, hash-chained entry log (`credits.json`); loading truncates at the first bad signature/link/overdraft, so hand-edited amounts are unspendable. The spendable balance is no longer stored in `identity.json` or as a `balance` field in `domains.json`. `ShardQuery` now carries the requester's signature over `(term, query_id, sender, challenge)`, and the serving node records that nonce only after verifying it — a node can no longer mint against a challenge it invented. Served-query traffic is the serialized response size, indexing meters the bundle bytes, artifact serving meters the bytes sent, and the default floor is 64 verified bytes per credit. |
| Browser had no link or image support | **Fixed** | Clicking a `<link href>` (or text inside it) walks to the nearest `Tag::Link` and navigates through the same `pending_navigate` path as the JS binding, skipping empty and `#fragment` hrefs; links paint as underlined accent text. `<image>`/`<img>` lays out with `width`/`height` attributes or CSS lengths (320×180 default) and paints a decoded PNG scaled to the box, with a cache, `file://` paths and `data:image/png;base64,` support, and a placeholder for unreadable sources. |
| Bundle compression was a no-op | **Fixed** | `.pwb` artifacts use the `PWBZ` container: 41-byte header (magic, version, uncompressed length, BLAKE3 digest) plus a zlib stream of the canonical `PWB1` bytes. `Bundle::deserialize` inflates transparently (bounded at 64 MiB, digest-checked) and still accepts legacy uncompressed bundles. `pwb_web::compress` exposes `compress_bundle`, `decompress_bundle`, and `is_compressed`; the CLI, builtin pages, native system pages, and `pwb-node --host` all emit compressed artifacts. |
| Dialogs/context menu were macOS-only | **Fixed** | `alert`/`confirm`/`prompt` use NSAlert on macOS and `tinyfiledialogs` (zenity/kdialog/yad on Linux, task dialogs on Windows) elsewhere, falling back to console output when `cfg(test)`, `PWB_HEADLESS`, no display, or no GUI backend is present. The right-click menu stays native on macOS and is painted in-window on other platforms: left click selects Copy/Inspect, the row under the pointer is highlighted live, click-outside or Escape dismisses, and the press that closes it does not activate the page. |
| Clipboard only worked via right-click and was dropped immediately on Linux | **Fixed** | `BrowserApp` keeps one `arboard::Clipboard` for the process lifetime instead of creating and dropping one per operation, so X11/Wayland clipboard managers can read the contents. `Ctrl`/`Cmd`+`C` copies the page selection or focused input, `+X` cuts, and `+V` pastes at the input caret; the context-menu Copy action uses the same handle. |

**Pricing gossip (2026-10-19):** a successful claim broadcasts a
`PricingEpoch` observation (current-hour registrations, earned work, live peer
count). Receivers fold it into their `RollingPricing` window, so claim prices
move with network activity instead of staying at the initial value forever.

Verified by `cargo test --workspace` (433 tests, including signed-ledger,
link-target, image-rendering, and free re-point gossip tests) plus multi-process runs: `pwb-node --connect <link>` prints `Connected to peer <key> at <addr>`
only after the remote node answers the Ping, a bootstrap pointing at an empty
`127.0.0.1:7922` prints `No bootstrap peers reachable; running standalone.`,
the default run prints `[Verifier] Peer verification check: PASSED`,
two nodes served a federated query whose signed receipt appended
`query +5` to `credits.json`, and editing that file's amount dropped the
reported balance back to 0.

Follow-up pass (2026-10-19) addressed the remaining user-facing gaps listed
below: claims now start at 10 credits (one hosted bundle covers the first
claim), owners can re-point a name for free, claim failures exit non-zero,
`content.pwb` links fetch on demand, `--host` publishes after joining and
announces via real `GossipAnnounce`, `nodes.plain` shows live peers and dials
its connect button, browser artifacts persist under `PWB_DATA`, fake bootstrap
defaults were removed in favor of `--bootstrap`/`PWB_BOOTSTRAP`, and pricing
epoch observations are gossiped. A further integration pass added encrypted
envelopes (X25519 + ChaCha20-Poly1305), onion cell relaying over the engine
socket with multi-cell framing, `FindNode`/`Nodes` DHT routing used for shard
placement, remote `CheckIndexProof` verification, wired `AttestBalance` /
`BalanceAttest` certificates, blind withdrawal and private claims, and relay
credits. A final protocol pass added signed request/reply onion circuits
(`send_onion_request` / `send_onion_reply`, used by `--private-search`),
turned `find_node` into an iterative Kademlia walk, and gave the node CLI
operator actions for `--status`, `--withdraw`, `--attested-claim`, and
`--blind-claim` with a 0600 voucher wallet. `--withdraw` requires the subject's
balance to have been attested by a peer (publishing a bundle broadcasts
`CheckIndexProof` to every connected neighbor to seed that). A last
user-facing pass implemented the two reserved domains — `network.plain`
(live listen/discovery/STUN addresses, peer table, index/store/registry
counts, claim price, balance) and `help.plain` (system-domain index, namespace
rules, hosting steps, credit rules) — so no native domain reports "no handler".
The receiver also now enforces a per-peer token-bucket quota (128 msg/s
sustained, 256 burst) before peer bookkeeping or application delivery. The
browser shell (address bar/history/tabs/bookmarks) was added in the 2026-10-22
pass; large-network validation and a node-level UI for the wallet actions
remain open.

Attested double-spend across partitions (previously the known economic gap)
is closed by funding-bound claims: every attested record cites the
certificate `(head, balance)` that funded it, the owner's signature commits
to the citation (`PWBDOMAIN2`), and receivers refuse claims that exceed the
owner's live funded spend. Merges repair overdrawn funding groups (oldest
claim keeps its funding, first-spent wins) and spend accounting is derived
from the surviving records, so the per-owner `claimed` side table —
unpersisted and merge-unsafe — is gone. Covered by registry repair tests,
funding-signature forgery tests, and a live two-node test proving an
exhausted certificate cannot fund a second claim over the wire. Residual:
funding verification uses the receiver's live peer view unless the operator
pins attestors. A fresh node whose only peer is malicious can therefore still
be lied to during bootstrap. `--trust-anchor` / `PWB_TRUST_ANCHORS` closes
that hole when the anchor identities are obtained independently of the seed;
shipping a well-known anchor set remains deployment work. The collect side
(attested claims, withdrawals, and the `--status` balance certificate) now
uses the same anchor set when configured, and dials anchors through the DHT
when they are not direct peers, so a stranger on a fresh bootstrap can collect
a certificate the anchored verifier accepts. Onion-cell rate limiting and
relay-credit accounting were closed in the 2026-10-21 pass.---

## Public-internet hardening (2026-10-21)

A final readiness pass fixed the remaining issues that only matter once a node
is exposed to the public internet:

| Finding | Status | What changed |
|---|---|---|
| Unauthenticated onion cells bypassed every rate limit | **Fixed** | The receiver now applies a per-source-IP token bucket (128 cells/s, 1024 burst — enough for one maximum-size 48 KiB circuit, ~470 cells — with 4096 tracked sources) and a global peel budget (4096/s, 8192 burst) before spending an X25519 + ChaCha20-Poly1305 peel on a 512-byte cell. The global budget is checked first so spoofed sources cannot multiply the work. |
| Relay credit minted for cells that were never relayed | **Fixed** | `handle_onion_datagram` emits `RelayEvent` only after the peeled cell is queued for a resolved, non-self next hop. Exit delivery earns nothing, and a cell aimed at an unknown relay or at this node's own onion key mints nothing, so sealing cells to a public onion key is not a credit source. |
| Unbounded queues | **Fixed** | The inbound queue (4096), outbound queue (4096), each application subscription (512), and onion-reply wait (1) are bounded; sends use `try_send` and drop on saturation. Relay, onion, and message fan-out prune closed subscribers. |
| Peer table lost on restart | **Fixed** | `pwb_node::peers` snapshots the peer table to `peers.json` (atomic write) every minute and at shutdown, merges previously remembered peers, and re-dials the 32 most recent on startup in parallel. Failed probes are removed; offline addresses stay remembered for the next start. |
| CORS off in default browser builds | **Fixed** | Cross-origin `fetch` now requires `Access-Control-Allow-Origin: *` unless built with `--features permissive-cors`. Unit tests and a live loopback HTTP test cover the blocked and blessed paths. |

Tests added: onion-limiter burst/refill/global-budget and subscriber
backpressure unit tests in `pwb-core/src/net/loops.rs`; relay-credit regression
for unforwarded cells in `pwb-node/tests/relay_credits.rs`; peer-store
round-trip, corrupt-file, and live restart-rejoin tests in
`pwb-node/src/persist.rs` and `pwb-node/tests/peer_persistence.rs`; CORS
block/allow tests in `pwb-browser/src/webapi/network.rs`.

The 2026-10-22 pass (below) closed the remaining items from this list:
registry-sync funding verification, structured logging/metrics/health, NAT
hole punching and relay fallback for symmetric NAT/CGNAT, and the browser
shell (tabs/address bar/history/bookmarks).

The 2026-10-23 pass pinned funding trust to operator-selected attestors
(`--trust-anchor`), gated and bounded the funding-certificate cache, added an
opt-in cover-traffic switch (`--cover-traffic`), made dials retry through lost
datagrams, added a deterministic packet-loss test, and moved the fuzz targets
(including the PML/PSS parsers) into a PR/weekly CI workflow. Remaining work
is real multi-node topology validation and a shipped seed/anchor distribution.

---


## P0 — blockers

The findings in this section are the original 2026-10-18 snapshot. The
remediation table and the 2026-10-19 follow-up note above record what has since
changed; obsolete headings carry a superseded marker.

### 1. The real transport is unauthenticated plaintext JSON over UDP; the onion layer is test-only

`NodeEngine` serializes `NetworkMessage` as JSON and sends it in a single UDP
datagram (`pwb-core/src/net/messages.rs`, `pwb-core/src/net/engine.rs`). There
is no encryption, no MAC/signature on the envelope, and no replay protection.

- `OnionCell`, `CellChannel`, `randomize_path`, and `CoverTraffic` have no
  caller outside `pwb-core` tests. `CellChannel` is not used by `NodeEngine`.
- Every `NetworkMessage` variant that carries a `signature` or identity field
  (`GossipAnnounce`, `PeerDiscovery`, `StorePosting`, `ShardQuery`,
  `ShardResponse`, `SubmitIndexWork`, `VerifyProof`) is processed without ever
  verifying it. `GossipAnnounce.signature` is never checked at all.
- `docs/security.md` describes the onion layer accurately *as a module*, but
  production traffic never touches it.

Consequences: any on-path host or LAN peer can read, inject, drop, replay, or
spoof every message. The "privacy" and "Sybil resistance" claims do not apply
to the running network.

### 2. LAN discovery cannot work as shipped

`NodeEngine::start()` constructs `DiscoveryService` with:

- `bind_port: 0` (ephemeral), so the node never listens on `DISCOVERY_PORT=7922`;
- `listen_port: self.config.listen_addr.port()`, which is `0` for the default
  `0.0.0.0:0` listen address.

`DiscoveryService::announce()` sends beacons to `255.255.255.255:7922` and
`239.255.0.1:7922`, but no node has a socket bound there, and received beacons
would advertise port `0`. The design doc says the service binds 7922; the code
says port 0. Unit tests avoid this by using `bind_port: 0` *and* unicast
`send_beacon_to`, so the broken production path is untested.

### 3. The peer table deletes every peer after five minutes

`peer_maintenance_loop` drops entries with `last_seen` older than 300 s.
`last_seen` is only set on *insert*:

- `PeerDiscovery`, `PeerExchange`, `ShardQuery`, `StorePosting` all use
  `entry(...).or_insert_with(...)`, which does nothing for an existing peer.
- The only refresh path is `GossipAnnounce`, and no production code ever sends
  one.

Result: a node's peer table empties every five minutes even while it is
actively exchanging shard queries with those peers.

### 4. `send_to()` injects the sender's own outgoing message locally

`NodeEngine::send_to` sends over the wire **and** pushes the same message into
the local handler queue with the *destination's* address as `sender_addr`
(`pwb-core/src/net/engine.rs`). `process_message` then registers
`sender_pubkey` — i.e. *your own key* — at the peer's address. That address is
then included in `peer_exchange_loop` gossip, so other nodes are told that your
key lives at someone else's address. `broadcast()` does the same local
self-enqueue (with your own address).

Every test that calls `send_to`/`broadcast` exercises this behavior, but none
asserts that the sender's own identity is absent from the peer table.

### 5. "Connecting" is only a local table insert; bootstrap success is not verified

`connect_to()` validates the address and writes a `PeerInfo`; `launch_connection`
is the same. No handshake, ping, pong, or acknowledgement is sent or awaited.
`connect_bootstrap()` returns success for any syntactically valid entry.

Verified empirically: `pwb-node` printed

```
[Network] Joined via 1 bootstrap peer(s)
    -> 42424242 at 127.0.0.1:7922
```

with nothing listening at `127.0.0.1:7922`. The default bootstrap entries in
both binaries are placeholders (`127.0.0.1:7922` with key `[0x42; 32]` in
`pwb-node`, and a different all-but-two-zero key in `pwb-browser`), so the two
binaries do not even agree on the fake peer.

### 6. Artifact fetch does not enforce content addressing

`DistributedSearch::handle_message` accepts `ArtifactResponse { content_id,
bundle_bytes }`, stores the bytes under their *actual* hash, but then records
the title and indexes the bundle under the *claimed* `content_id`, and hands
the bytes to a waiter keyed by the claimed ID. `repo` never checks
`ContentId::compute(bundle_bytes) == content_id`.

A peer can therefore serve arbitrary content for any requested CID, and the
browser will render it as that page. Combined with unauthenticated search
results (below), there is no end-to-end integrity check for fetched content.

### 7. UDP single-datagram framing caps bundles at ~64 KiB and drops failures

`receiver_loop` reads into a 65535-byte buffer; `sender_loop` ignores send
errors (`let _ = socket.send_to(...)`). `ArtifactResponse` carries the entire
bundle in one datagram, so any bundle larger than the UDP payload limit simply
never arrives (and there is no fragmentation, chunking, ACK, or retry).
*(Fixed)* `pwb-web` now emits compressed `PWBZ` bundles, so repetitive
artifacts are usually far smaller than the 64 KiB datagram limit.

### 8. `pwb-node` — the daemon that is supposed to run the network — runs none of it

`pwb-node/src/main.rs` instantiates `DomainService` but **not**
`DistributedSearch` and **not** `SystemDomainHandler`. Therefore the full node:

- does not answer shard queries or store postings,
- does not serve `search.plain`, `register.plain`, `gossip.plain`, or
  `nodes.plain` to peers,
- has no way to host or publish a site (it only reads a fixed `demo.pwb` from
  the working directory),
- never sends `SearchQuery`, `GossipAnnounce`, `SubmitIndexWork`, or
  `VerifyProof`.

The browser is currently the only process that runs the federated search
service. `SearchQuery`/`SearchResponse` are dead protocol variants in
production; federation uses only `ShardQuery`/`ShardResponse`.

### 9. The credit system is self-declared, and query receipts are self-minted

- `IdentityManager::new()` hardcodes 100 credits "for testing"; the balance is
  a field in a JSON file under `$HOME/.pwb`.
- `DomainService` persists its own balance to the same file family and treats
  it as spendable. Nothing reconciles it with any network ledger.
- `AttestorLedger`, `BalanceAttestation`, `QuorumCertificate`,
  `BlindVoucherIssuer`, and the voucher spend log are only used by tests,
  `blind` unit tests, and the `merge_integration` test. No wire message carries
  an attestation, certificate, voucher, or balance proof.
- In `DistributedSearch`, a node serving a `ShardQuery` calls
  `minter.issue_challenge(challenge, now)` on **its own** challenge ledger using
  the *requester's* nonce, then immediately `mint_query`s against it. The check
  that is supposed to prove a live requester therefore always passes.
- `QUERY_SERVICE_BYTES = 8 KiB` of synthetic traffic is recorded per served
  query to clear the anti-ASIC floor; real bytes are not metered.
- `query_with_receipts` returns receipts, but `query` (the only production
  caller) discards them and no code verifies a `QueryReceiptWire`.

Verified empirically at audit time: the default `pwb-node` run printed
`[Credit] Query mint refused: verified traffic covers 0 of 5120 required bytes`
and then minted relay credits from 35 synthetic 1 KiB cells. **Fixed** — the
demo mint block is gone, a fresh node starts at 0, and credits come only from
the signed ledger (see the remediation table).

### 10. Domain resolution and registry sync are unauthenticated *(fixed — see table)*

- `DomainRecordWire` has no signature. `DomainService::resolve` accepts the
  first `DomainResponse` that arrives; any peer can resolve any `.pwb` name to
  any CID.
- `RegistrySyncResponse` records are merged directly by `merge_records` with no
  signature, ownership, or price check. Any peer can inject arbitrary domain
  records into a joining node's registry.
- `ClaimDomain` *is* signature-checked, but `credits_spent` is self-declared:
  receivers check it against the current price, never against the owner's
  balance. The code documents this as a known limitation; it means the
  namespace is first-come-first-served only by local convention, not by
  protocol.

### 11. Domain claim ordering is degenerate *(fixed — see table)*

`DomainRecord::registered_at_epoch` is hardcoded to `1` in `register()`,
`DomainService::claim()`, and `on_claim()`. Merge/apply uses "higher epoch wins;
ties break on the lexicographically smaller owner key". With every record at
epoch 1:

- a domain's first claimant is not protected;
- an attacker with a smaller public key can overwrite any domain forever;
- there is no clock, sequence number, or claim chain to order claims.

### 12. Dynamic pricing is static in practice *(superseded — see Pricing gossip above)*

`record_pricing_epoch` is called only from tests and from the demo block in
`pwb-node/src/main.rs`. No `NetworkMessage` carries `PricingEpochStats`, and
`DomainService::persist` does not save pricing history. A real node's
`claim_cost()` is permanently `INITIAL_CLAIM_PRICE = 50`, including across
restarts. The design doc says the stats are "gossip-delivered"; there is no
such gossip.

### 13. Nothing durable is stored *(fixed — see table)*

`ArtifactHost` is an in-memory `HashMap`; `SearchEngine` is an in-memory
inverted index. Neither is persisted anywhere, in either binary. Restarting a
node loses every hosted artifact, every posting, and the whole index. The
browser re-indexes only pages it opens during that session. In a "serverless
web", a node currently cannot serve yesterday's content.

### 14. The default node run prints a failing verifier check *(fixed — see table)*

`pwb-node/src/main.rs` builds an `IndexProof` whose signature is over
`b"index_proof"`, then calls `PeerVerifier::verify_indexing_proof`, which
reconstructs a different message (`content_id || term_hash`). The check fails,
so every default run prints:

```
[Verifier] Peer verification check: FAILED
```

The surrounding credit/domain "gossip merge" block is a hardcoded demo
(`cluster_a`/`cluster_b`, fabricated owners `[11;32]`/`[22;32]`, synthetic
epoch stats), not a network operation.

---

## Subsystem detail

### `pwb-core::net`

| Area | Status | Notes |
|---|---|---|
| UDP send/receive | Works | JSON datagrams, one message per datagram |
| Peer table | Partial | Expires peers after 5 min; no liveness refresh; self-injection via `send_to` |
| Message authentication | Missing | No envelope signature, no per-variant verification |
| Encryption | Missing | Onion/CellChannel never used |
| Framing | Fragile | 64 KiB ceiling, silent send failures, no fragmentation/retry |
| Lifecycle | Buggy | `running` is set before `bind`; a failed bind leaves the engine permanently "started" |
| `Drop` | Misleading | Comment says it stops the engine; it only drops a mutex guard, loops keep running |
| `Transport` trait | Dead | Zero implementations |
| Bootstrap/connect | Registration only | No handshake or reachability check |
| Rate limiting | Fixed | The receiver drops authenticated messages above a per-peer token bucket (128 msg/s sustained, 256 burst, 4096 tracked peers) before bookkeeping or application delivery; the bucket map evicts its oldest entry instead of growing. Unauthenticated onion cells have their own per-source bucket (64/s, 128 burst) plus a global peel budget (4096/s, 8192 burst), so a spoofed-source flood cannot multiply X25519 peels. |
| Peer persistence | Fixed | The live peer table is written atomically to `peers.json` under `--data` every 60 s and at shutdown; startup re-dials the 32 most recent remembered peers in parallel, keeps only those that answer a liveness probe, and retains offline addresses for the next attempt. |

### `pwb-core::onion` + `CellChannel`

Implemented and well-tested in isolation: X25519/ChaCha20-Poly1305 fixed-size
cells, path validation, dummy hops, cover traffic, forward secrecy, and
source-address re-origination. **Not connected to any production path.** The
cell payload max is 118 bytes and multi-cell framing is explicitly future work,
so even after wiring this cannot carry current `NetworkMessage`s unchanged.

### `pwb-core::discovery`

Beacon encoding, freshness, and Ed25519 verification are real. The service's
production wiring is broken (see P0 #2). Additional gaps: beacons are not
authenticated as *current* members of the swarm beyond the timestamp window;
the merge path is the only place `last_seen` is refreshed. Address families are
covered: the service binds an IPv4 and an IPv6 socket side by side, fans out to
`255.255.255.255`/`239.255.0.1` and to `[ff02::1]`/`[ff05::255:1]` respectively,
and merges beacons from either into the same peer table (a family whose socket
cannot bind is skipped, and one datagram leaving anywhere counts as success).

### `pwb-core::dht`

`RoutingTable` (Kademlia k-buckets, ping/pong state, latency-weighted
selection) is exported, documented, and tested, but **no production code
constructs one**. Federation calls `NodeId::from_public_key().xor(target)` over
the flat peer table and truncates to three. There is no `FindNode`, no
iterative lookup, no bucket refresh, no correctness under partial connectivity.
The "DHT-sharded" claim is only true for a fully-connected cluster of a handful
of nodes.

### `pwb-core::stun`

Works for IPv4 and IPv6: `XOR-MAPPED-ADDRESS` decodes both families (IPv6
bodies XOR the transaction ID per RFC 5389 §15.2), the query targets a STUN
server matching the listen socket's family, and a dual-stack node queries an
IPv4 server v4-mapped and canonicalizes the answer. There is no NAT hole
punching, no keepalive, no relay fallback. STUN alone does not make a node behind a
symmetric NAT or CGNAT dialable, which the README's "joins the swarm" phrasing
implies.

### `pwb-node::search` / `federation`

What is real: term extraction, shard hashing, tokenization, local ranking, and
cross-node shard query/response over the transport.

What is not production-ready:

- In-memory index only; no persistence, no eviction, no size bound.
- No posting authentication: anyone can `StorePosting` arbitrary
  `(term, content_id, title, frequency)` entries.
- No response authentication: `ShardResponse` postings are merged without
  checking a receipt, checking that the responder is responsible for the term,
  or checking the signer.
- Replicas are the ≤3 known peers closest by XOR; no replication repair, no
  handoff when a node leaves, no anti-entropy.
- Ranking is summed term frequency plus a 5× title boost. No IDF, no link or
  author signal, no spam resistance, no freshness. Snippets are the literal
  string `Content ID: ...`.
- Fixed 400 ms query deadline, no retries, no indication of partial results.
- Queries are sent in plaintext; no onion or end-to-end protection.
- `index_and_publish` now mints index credits against an `IndexReceipt` over the bundle bytes (fixed; kept here as part of the original audit).
- No re-index verification: `SubmitIndexWork`/`VerifyProof` are never sent or
  handled.

### `pwb-node::credit` / `ledger` / `blind` / `verifier`

The cryptography and accounting logic is genuine and heavily tested:
mint policy, challenge TTL, relay hash chain, blind Schnorr issuance, voucher
spend log, attestor ledgers, quorum certificates.

The *system* is not: nothing is exchanged over the network, no peer verifies
another peer's balance, no spend is gossiped, and the local balance is a
mutable JSON field. `PeerVerifier` only checks that a miner signed a tuple; it
does not re-index the bundle or challenge the claim, and the production demo
passes it the wrong message. The blind/attested claim paths
(`register_attested`, `register_blind`) are not reachable from `DomainService`.

### `pwb-node::domain`

Real: signed canonical claim statements, local dedup, gossip forwarding,
persistence, remote resolution messages.

Gaps: unsigned registry sync and responses (P0 #10), degenerate epochs
(P0 #11), static pricing (P0 #12), no balance proof, no proof that the claimant
hosts the target CID, no gossip TTL/hop limit (only a process-local
`seen_claims` set that grows forever), non-atomic file writes (a crash can
truncate the registry), and a persisted balance that a user can edit.

### `pwb-node::system`

The system pages render correctly and are used by the **browser**. The node
never mounts the handler, so no node serves them over the network. The handler
supports only `SearchQuery`/`FetchArtifact`; `ClaimDomain`, `RegistrySync*`,
`DomainQuery`, and the rest are handled by `DomainService` only. *(Fixed)* all
seven native domains now render: `network.plain` shows the live address/peer
and local-service state, and `help.plain` indexes the system domains with the
namespace, hosting, and credit rules.

### `pwb-browser`

Substantial and working: winit/softbuffer window, layout, text selection,
scrolling, devtools, rquickjs runtime, fetch/WebSocket, localStorage, timers.

Production gaps:

- *(Fixed)* `<img>`/`<image>` decodes and paints PNGs scaled to the box, with a
  cache and placeholder fallback.
- *(Fixed)* `<a>`/`Tag::Link` click navigation follows `href`; the
  `__pwb_navigate` JS binding remains available.
- *(Fixed)* Non-macOS `alert`/`confirm`/`prompt` use native dialogs when a GUI
  backend exists (console fallback otherwise), and the context menu is painted
  in-window instead of being a no-op.
- *(Fixed)* CORS is enforced by default: cross-origin `fetch` responses need `Access-Control-Allow-Origin: *`; `--features permissive-cors` is the explicit local-development opt-out.
- JS runs in one shared global context in script mode: no ESM execution, no
  per-page isolation, and devtools share the page's DOM.
- *(Fixed)* The window has tabs, per-tab history, bookmarks, an editable address bar, and reload/back/forward; the remaining gap is downloads.
- *(Fixed)* `SystemDomain::Network` renders the live address/peer/service page and `Help` renders the system-domain index with namespace and credit rules.
- Page loads and searches are "best effort": errors are printed, not surfaced
  in the UI consistently.

### `pwb-web` / wire format

- *(Fixed)* `compress_bundle` emits a zlib-compressed `PWBZ` container with a
  BLAKE3 integrity digest; `Bundle::deserialize` inflates both compressed and
  legacy uncompressed bundles, with a 64 MiB bound.
- `bundle.toml` is never read by any crate (one exists in
  `webpages/devtools/`).
- Bundles have an optional `author_pubkey`, but no production code ever sets it
  (`None` everywhere) and no code verifies it. There is no author authenticity,
  no signature over the bundle, and no revocation.
- *(Fixed)* `Bundle::deserialize` now requires the cursor to reach the end of
  the input (truncated and trailing-byte bundles are rejected), including for
  the compressed `PWBZ` container.
- `strip_esm_syntax` is line-based: multi-line imports, `export default`
  expressions, and strings containing `import ` can be mangled silently.
- *(Fixed)* `pwb-browser/src/bundler/` no longer has a divergent PML parser:
  `bundle_from_dir` delegates to `pwb_web::parse::parse_site_directory`, so
  devtools and built-in pages share one compiler. The shared parser now also
  accepts HTML container tags (`<div>`, `<body>`, `<a>`, …), skips doctypes and
  `<link rel=stylesheet>`/`<meta>` metadata, turns inline text in containers
  into `Text` children, and inherits `font-size`/`color` down the tree.

### Cross-cutting operations

- *(Fixed)* Content and indexes persist under `--data` (`ArtifactHost::open`,
  `SearchEngine::open`), and `domains.json`/`credits.json` survive restarts.
- No configuration file; flags and `PWB_*` environment variables only. The
  placeholder bootstrap defaults were removed in favor of explicit
  `--bootstrap`/`PWB_BOOTSTRAP` peers, which accept literal addresses or DNS
  names with a pinned public key.
- *(Fixed)* `PWB_LOG_FORMAT=json` emits structured events, `pwb_core::telemetry`
  exposes counters, and `pwb-node` serves `/health`, `/status`, and `/metrics`
  (loopback by default). There is still no distributed tracing.
- Many `unwrap()`/`expect()` calls in async tasks and mutex paths; a panic in a
  loop silently stops that subsystem, and mutex poisoning cascades.
- *(Fixed)* Inbound, outbound, and per-subscriber queues are bounded. A slow
  consumer drops messages (UDP is lossy by design) instead of growing memory;
  closed subscribers are pruned. Inbound is still per-peer rate-limited.
- *(Partial)* `fuzz/` has cargo-fuzz targets for the onion-cell, JSON message,
  `.pwb` bundle, and PML/PSS parsers, and `.github/workflows/fuzz.yml` runs
  them for 30 seconds per target on relevant pull requests and five minutes
  weekly. There is still no target for the site-directory builder (which
  touches the filesystem).
- CI runs fmt, clippy, tests (including the two-process
  `public_network_smoke` test), and coverage, but enforces no coverage
  threshold. `pwb-core/tests/packet_loss.rs` now exercises retry-on-loss with a
  deterministic UDP shim; real NAT/topology runs are still absent.
- *(Fixed)* Documentation overclaims were corrected: `TODO.md` now reflects
  the wired search/system/pricing paths, and the `gossip.plain`, `nodes.plain`,
  discovery, and pricing docs describe behavior that exists in the binaries.

---

## What is actually production-quality today

- `pwb-core::crypto`: Ed25519 identity, BLAKE3 content IDs, X25519 DH, blind
  Schnorr issuance, voucher verification/spend log.
- `pwb-core::wire`: deterministic bundle encoding/decoding with symbol
  interning and round-trip tests.
- `pwb-core::onion` (as a library) and `pwb-core::qr`.
- `pwb-web::parse`: PML/PSS parsing, components/imports, script bundling, CSS
  bundling via lightningcss.
- `pwb-browser` layout/render/selection/devtools/JS runtime for local content.
- `pwb-node::search` local index math and `pwb-node::credit` policy math, as
  isolated components.
- The unit/integration test suites themselves; they are broad and fast.

---

## Suggested remediation order

1. **Pick the transport.** Either wire `OnionCell`/`CellChannel` into
   `NodeEngine` (and add multi-cell framing), or explicitly drop the onion
   claim and add authenticated encryption + signatures to the JSON envelope.
   Also add fragmentation/reassembly and a size limit policy.
   *Status: done — signed/encrypted envelopes, fragmentation, multi-cell onion
   framing, return circuits, and relay credits are wired into the engine.*
2. **Fix liveness.** Bind discovery to 7922 (or make the port configurable and
   consistent), advertise the *actual bound* port, refresh `last_seen` on every
   valid message, filter self, and remove the local self-enqueue in
   `send_to`/`broadcast`.
   *Status: done — see the remediation table above.*
3. **Make the node a node.** Instantiate `DistributedSearch` and
   `SystemDomainHandler` in `pwb-node`, add a `--host <dir|file>`/publish path,
   and delete the demo block (or move it behind a `--demo` flag). Remove the
   `[Verifier] FAILED` path.
4. **Authenticate the data plane.** Sign/receipt postings and shard
   responses, verify receipts on the requester, enforce content IDs on
   artifact fetch, and reject responses whose bytes do not hash to the
   requested CID.
5. **Authenticate the control plane.** Add signatures to `DomainRecordWire`,
   verify registry-sync records, and turn claims into an ordered log (monotonic
   epoch/sequence) instead of `epoch = 1` LWW.
6. **Make credits real.** Connect minting to the ledger/attestation path,
   remove the self-issued challenge, meter real bytes, and never persist a
   spendable balance that the user can edit.
   *Status: done — signed `credits.json` chain, requester-signed challenges,
   real served bytes; peer attestation of balances is still library-only.*
7. **Persist.** Content store and index need an on-disk format with atomic
   writes before anything else is meaningful across restarts.
8. **Then** address browser feature gaps, wire compression, unify the two PML
   parsers, and set a coverage/integration bar in CI.
   *Status: images, link navigation, non-macOS dialogs, context menu,
   compression, and parser unification are done; CI remains.*

Until steps 1–7 are done, the project is a working local browser and a
collection of well-tested protocol primitives, not a working serverless
network.

---

## Public join and private browsing (2026-10-24)

A stranger no longer needs a hand-copied connection link to get onto the
network, and browsing no longer defaults to revealing the local address:

| Change | What it does |
|---|---|
| `pwb-core::join` + `pwb-node::join` | A signed, versioned descriptor (`deploy/join.json`) lists seeds plus optional funding anchors and an expiry. Clients fetch it over HTTPS, from a file, or from inline JSON (256 KiB cap), prove one randomly drawn seed with Ping/Pong (failing over to another only when it stays silent), and merge several sources. The first signer seen is pinned under `--data`; `--join-signer`/`--require-join-signature` pin it up front. |
| `pwb-node --write-join` | Publishes a descriptor from `--join-seed`, `--trust-anchor`, and `--join-expires`, signs it with the node identity or `--write-join-key`, prints the signer key, and exits without starting the engine; write errors and an expiry in the past are fatal. |
| `pwb-node --join` / `PWB_JOIN` / `--no-join` | Selects sources and makes the default public descriptor explicit; an explicit `--bootstrap` suppresses it so private deployments stay independent of GitHub. `--join-required` fails closed. |
| `pwb-node::privacy` | Randomized three-hop circuits from direct peers, with the exit pinned when the caller needs one. |
| Browser privacy defaults | `search.plain`, content fetch, and `.pwb` resolution run over onion circuits by default; `--direct` is the only opt-out and prints a warning. A failed circuit never falls back to a direct request. Nodes answer onion-delivered shard, artifact, and registry queries, so private browsing works against an ordinary swarm. |
| `pwb-node --help` | Lists the join, hosting, credit, and privacy flags; before this pass `--help` was treated as an unknown flag and started a node. |

Covered by `pwb-node/tests/join_discovery.rs` (signed HTTP descriptor, tamper
rejection, writer round-trip), `rendezvous_join.rs` (two nodes discovering each
other through a rendezvous seed, undialable peer filtering, failed join is not
counted), `public_network_smoke.rs` (two processes over a bootstrap link), and
`private_routing.rs` (private search, content fetch, and domain resolution over
an onion circuit).

The remaining deployment step is operator work, not code: run a seed on a host
with a public UDP port, publish the resulting signed `deploy/join.json` (or
mirror it), and publish the signer/anchor keys through a channel independent of
that host. Large-swarm validation, sybil resistance in an open network, and an
independent security review are still open, as the sections above describe.
