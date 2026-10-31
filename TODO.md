# PlainWeb — Project Status (TODO.md)

## Status: Modules implemented and integrated

The §1–§6 modules exist and are unit-tested. Transport, discovery, persistence,
federated search, system pages, and signed claim/credit state are wired into the
binaries. As of 2026-10-19 the transport is encrypted as well as authenticated
(ephemeral X25519 + ChaCha20-Poly1305 envelopes), 512-byte onion cells are
relayed hop-by-hop over the same UDP socket, the Kademlia routing table drives
`FindNode` lookups and shard placement, and peer-attested balances, blind
voucher issuance/redemption, remote proof verification, and relay credits are
exchanged over the network. See
[`docs/production-readiness.md`](docs/production-readiness.md) for remaining
operational hardening (large-network validation, queue backpressure, and the
full browser shell).

Transport and peer liveness were remediated on 2026-10-18: datagrams are now
carried in signed, replay-protected envelopes (fragmented/reassembled to fit a
1200-byte datagram), the engine never re-injects its own outbound traffic, LAN
discovery binds port 7922 and advertises the real bound port, and bootstrap
joins are only reported after a peer answers a liveness probe. `pwb-node` runs
federated search and the system-domain responder, publishes `--host`ed content,
persists artifacts and its index under `--data`, and verifies signed postings,
shard responses, and domain records. Credits are earned from verified service
traffic (requester-signed shard challenges, real served bytes) into a signed
hash-chained ledger; the old self-issued challenge and editable persisted
balance are gone.

Follow-up work on 2026-10-19 added gossiped pricing observations, live peers and
a working connect action on `nodes.plain`, browser-side persistence under
`PWB_DATA`, on-demand `content.pwb` fetching, announced `--host` artifacts, and
free owner re-points for existing names. Claims start at 10 credits, which one
hosted bundle's index reward covers. The same pass wired onion relaying and
relay-credit minting into `NodeEngine`, replaced flat shard routing with the
live DHT, added multi-cell onion framing, and exposed attested/blind claim and
withdrawal messages through `AttestationService` and `DomainService`.

A further pass on 2026-10-19 closed the remaining protocol gaps: onion
messages now carry a signed request id and an optional return path, so
`NodeEngine::send_onion_request` / `send_onion_reply` implement full
request/reply circuits and `pwb-node --private-search` routes a `search.plain`
query through three relays; `find_node` is a real iterative Kademlia walk
(`ALPHA=3`, `K=20`) rather than a single round; and the operator CLI exposes
`--status`, `--withdraw`, `--attested-claim` and `--blind-claim` with a
0600 voucher wallet under `--data`. Documented peer-attested index work also
feeds those claims: publishing broadcasts `CheckIndexProof`, so neighbors
record the earning that an attested claim later spends.A final user-facing pass on 2026-10-19 filled the last reserved system
domains: `network.plain` now reports the live listen/discovery/STUN addresses,
peer table, indexed-document/term counts, hosted artifacts, registered
domains, claim price, and balance, and `help.plain` is the system-domain index
with the namespace, hosting, and credit rules. The receiver also enforces a
per-peer token bucket (128 msg/s sustained, 256 burst, 4096 tracked peers) so
one peer cannot flood the handler or application subscribers.

A follow-up pass closed the documented attested double-spend gap: attested
claims are now funding-bound (each record cites its certificate's `(head,
balance)` under a `PWBDOMAIN2` owner signature), spend accounting is derived
from the live funded records instead of an unpersisted side table, and merges
repair overdrawn funding groups first-spent-wins. Registry repair, funding
forgery, and a live two-node exhausted-certificate test cover the new paths.

A 2026-10-24 pass removed the out-of-band link requirement for strangers:
`pwb-core::join` + `pwb-node::join` fetch, verify, signer-pin (trust on first
use under `--data`), and merge well-known descriptors, and
`pwb-node --write-join` publishes one from an operator's seeds and anchors.
`pwb-browser` now routes `search.plain`, content fetch, and `.pwb` resolution
over onion circuits by default, with `--direct` as the only (and loudly
labelled) opt-out; nodes answer onion-delivered shard, artifact, and registry
queries. `pwb-node --help` documents the operator flags instead of ignoring
them. `deploy/join.json` ships with an empty seed list until a real seed is
published.

A 2026-10-26 pass added `chat.plain` and a low-level E2EE send flag. Any
message type can now be sealed end-to-end for its peer with
`NodeEngine::send_to_with(.., SendFlags::E2EE)` / `broadcast_with` (or
`seal_message` to pair it with another send path such as an onion circuit):
a fresh ephemeral-X25519 sealed box per message
(`NetworkMessage::Sealed`, BLAKE3-derived key, AAD bound to the
sender/recipient pair, bounded TTL replay cache). The receiver opens it
before dispatch and re-validates the inner message with the usual
identity/signature/shape checks, so subscribers keep their existing API and
relays/onion exits never see the inner payload. The wire change is additive
(no version bump). Counters: `pwb_e2ee_messages_opened_total` /
`pwb_e2ee_messages_dropped_total`.`chat.plain` is the first app of
the flag: a peer picker plus transcript composer page in the browser, backed by
`pwb-node::chat::ChatService` (capped transcripts, 500 entries per peer,
persisted to `<data>/chat.json` by both binaries — the write merges by
message id, so one `PWB_DATA` shared by the browser and the node cannot
clobber the other's entries). Delivery picks a `ChatRoute`: direct with
`SendFlags::E2EE` by default, across a three-hop circuit (sealed via
`seal_message`) in the browser's onion privacy mode with a loud direct
fallback when no circuit can be built. A headless `pwb-node` receives,
stores, and prints chats addressed to its identity; `--chat <peer_hex>`
gives the operator a stdin REPL to reply, and `--chat-via direct|onion|auto`
picks the route its lines take. Delivery is confirmed, not assumed: every
recorded chat is answered with a `ChatAck` receipt on the path it arrived on
(the circuit's return path for onion sends), and an outbound entry shows
confirmed (✓) once the peer acknowledges it and unconfirmed (⚠) when the
8-second window expires.

Alongside the private conversations, `chat.plain?room=<name>` is the public
room (default `lobby`, joined by both binaries at startup). Posts are
author-signed (`RoomPostWire`, verified on every copy) and **flooded**: each
holder re-broadcasts to its peers minus the sender, spending a hop from
`ROOM_POST_HOPS` (8), with termination from a bounded `(author, id)` digest
set and the hop budget — so a post reaches the network through bystanders,
who relay without storing. `RoomSubscribe` is one hop and
envelope-authenticated, which gives each member an honest roster of the peers
known to be present (a member answers a newcomer's first announcement with
its own, terminating on first sight); `RoomSync`/`RoomHistory` give a late
joiner up to 50 signed posts so it catches up instead of waiting for traffic.
Membership and history persist to `<data>/rooms.json`, merged by message id
like `chat.json`. The page lists rooms on the `chat.plain` picker, joins one
when opened, and posts through `__pwb_room_send`; `pwb-node --room <name>`
is the headless REPL.

The
sections tracked here are:

- §1 Core Architecture Restructuring (`pwb-core` consolidation)
- §2 Onion Routing & Encryption Layer (`pwb-core::onion`)
- §3 Decentralized Node Discovery (`pwb-core::discovery`)
- §4 Anti-Sybil Defense & Peer-Attested Credit System (`pwb-node::credit`)
- §5 Dynamic Rolling Average Domain Pricing & Gossip Merging
- §6 Testing, Validation & Verification

## Where the design lives

The design decisions that used to be tracked here have been moved into
`docs/design/`:

- `docs/design/01-core-architecture.md` — §1 `pwb-core` consolidation (net engine, crypto, system resolver)
- `docs/design/02-onion-routing.md` — §2 onion routing, Sphinx encapsulation, cover traffic, CellChannel
- `docs/design/03-node-discovery.md` — §3 UDP broadcast/multicast discovery, connection links, QR codes, Kademlia DHT
- `docs/design/04-credit-system.md` — §4 credit minting, sharded peer-attested ledger, blind balance & privacy
- `docs/design/05-pricing-and-merge.md` — §5 rolling average pricing, CRDT gossip partition & network merge

Critical protocol constants (cell size, hop counts, discovery port, pricing
parameters, `.plain` reserve rule) are in `docs/protocol.md`.

License: GNU GPLv3 — see `LICENSE`. Third-party notices are in `NOTICE`.

---

## Public-internet readiness

Gaps that only matter once a node is exposed to the public internet rather than
a loopback test cluster.

### Closed 2026-10-21

- **Onion cells bypassed all rate limiting.** Fixed: per-source-IP token bucket
  (128 cells/s, 1024 burst, sized for a complete 48 KiB circuit) plus a global
  peel budget (4096/s, 8192 burst), checked before any X25519/ChaCha work. See
  `pwb-core/src/net/loops.rs`.
- **Relay credit was minted for cells that were never relayed.** Fixed: a
  `RelayEvent` is emitted only after a cell is queued for a resolved, non-self
  next hop; exit delivery and unknown/self next hops mint nothing.
- **Unbounded message queues.** Fixed: inbound (4096), outbound (4096), each
  subscriber (512), and onion-reply wait (1) are bounded, with drop-on-full
  semantics and closed-subscriber pruning.
- **Peer table lost on restart.** Fixed: `pwb_node::peers` persists the peer
  table to `peers.json` every minute and at shutdown, re-dials the 32 most
  recent remembered peers on startup, drops unreachable probes, and keeps
  offline addresses for a later attempt.
- **CORS was compiled out by default.** Fixed: cross-origin `fetch` requires
  `Access-Control-Allow-Origin: *` unless built with `--features
  permissive-cors`.

### Closed 2026-10-22

- **Registry sync verified funding citations.** `RegistrySyncResponse` now
  carries a `SyncRecordWire` per record: the signed record plus the quorum
  certificate for its `(head, balance)` citation. The receiver checks the
  owner signature, the certificate's subject/head/balance, the spend against
  the attested balance, and the quorum against its own deterministic attestor
  neighborhood. Unfunded records still merge by owner signature; a funded
  record with a missing or fabricated certificate is ignored. Certificates are
  cached and persisted with the registry so a node can attach them when it
  serves a later sync. Regression:
  `test_registry_sync_rejects_unverifiable_funding_citations`.
- **Postings are batched.** `StorePostings` carries up to 64 signed postings
  per message (`POSTINGS_BATCH_MAX`), grouped per responsible shard. A 600-term
  document is now 10 messages instead of 600, so it no longer trips the
  per-peer 256-message burst bucket. Regression:
  `test_large_document_postings_arrive_in_batches`.
- **Structured logging, metrics, and a health endpoint.**
  `pwb_core::telemetry` provides `PWB_LOG_FORMAT=json` JSON-lines events and
  lock-free counters for datagrams, messages, rate limits, replays, onion
  cells, relays, dials, and hole punches. `pwb-node` serves `GET /health`,
  `/status`, and Prometheus `GET /metrics` on `--health-addr` (default
  `127.0.0.1:7711`; `--no-health` disables). `network.plain` shows the live
  service counts (documents, terms, artifacts, domains, peers, claim price,
  balance) beside those endpoints. Covered by unit tests.
- **NAT hole punching and relay fallback.** `PunchRequest`/`PunchSignal` let a
  rendezvous peer introduce two NATed peers; both emit signed 113-byte probes
  to open their mappings, then prove the path with a normal Ping/Pong. If that
  fails (symmetric NAT/CGNAT), `RelayDeliver` forwards the originator's own
  signed, encrypted envelopes through a direct relay, so the relay cannot read
  or forge them and replies follow the same route. `dial_resilient` tries
  direct, punch, then relay; connection links, remembered peers, and the
  `nodes.plain` connect action use it. Integration tests:
  `pwb-core/tests/nat_traversal.rs`.
- **Browser shell.** The window now has a fixed chrome above every page:
  clickable tabs (plus `+`), an editable address bar (Ctrl/Cmd+L, Enter, bare
  words become searches), back/forward/reload, a bookmark toggle, and
  `bookmarks.plain` / `history.plain` pages. Alt+Left/Right navigate history;
  Ctrl/Cmd+T/W/D/R handle tabs, close, bookmark, and reload. Tabs, per-tab
  history, and bookmarks persist to `$PWB_DATA/session.json` (or
  `$HOME/.pwb/session.json`) with an atomic write. `shell::tests` covers
  navigation rules, persistence, chrome actions, list pages, and fixed
  layout.

### Closed 2026-10-23

- **STUN opt-out.** `pwb-node` and `pwb-browser` accept `--no-stun`, which
  skips the public STUN query and leaves connection links on the routable LAN
  address. The security doc previously promised that operators could accept
  LAN-only reachability but offered no supported flag. Verified by running a
  node with `--no-stun` and confirming no public address/STUN line is printed.
- **Public-join claims corrected.** The browser module docs, `--help`, and
  the README quick start no longer imply that a flagless start joins an internet
  swarm: they now say LAN discovery is the default and `--bootstrap` /
  `PWB_BOOTSTRAP` is required across NAT. The stale "ledger not wired" and
  "no onion rate limiting" notes were also corrected.
- **Bootstrap entries accept DNS names.** `--bootstrap` and `PWB_BOOTSTRAP`
  now resolve `<host:port>/<hex_pubkey>` through DNS instead of requiring a
  literal IP. The public key stays pinned, so a changed address record cannot
  substitute a different seed identity. Resolution is bounded to five seconds
  and, since 2026-10-28, matches the address family of the `--listen` socket
  when a host publishes both families (IPv4 preferred when none is
  configured). Covered by `bootstrap_spec_accepts_literal_addresses_and_dns_names`.
- **Onion cells have a replay cache.** Relays hash each accepted 512-byte cell
  and reject exact duplicates for five minutes, so captured cells cannot be
  re-injected to repeat a peel or delivery. The cache is capacity-bounded
  (65,536 digests) with oldest-first eviction and is checked after the existing
  per-source/global rate budgets. `onion_cells_replayed` is exposed in
  `/metrics`. This does not add a circuit id; it only rejects identical
  ciphertext, and a legitimate retransmission builds fresh cells.
- **Two-process smoke test.** `pwb-node/tests/public_network_smoke.rs` launches
  two real binaries with separate identity/data directories, reads the first
  node's connection link, and asserts the second joins through `--bootstrap`.
  It runs in the normal `cargo test` CI job and needs no STUN or internet
  access.
- **Funding trust can be pinned independently of the peer table.**
  `pwb-node` and `pwb-browser` accept repeatable `--trust-anchor
  <hex_pubkey>` and comma-separated `PWB_TRUST_ANCHORS`. When at least one
  anchor is configured, attested claims and registry-sync funding citations
  must be signed by `min(DEFAULT_QUORUM_K, anchors)` distinct anchors, with no
  reference to the receiver's live peers. A fresh node whose only peer is
  malicious can therefore reject forged funding during bootstrap. Without
  anchors the historical deterministic-neighborhood policy still applies, and
  startup prints which policy is active. Covered by
  `test_pinned_trust_anchors_are_the_funding_authority`,
  `test_pinned_anchors_gate_claims_and_require_the_full_small_quorum`, and
  `test_forged_certificate_does_not_poison_the_sync_cache`.
- **The funding-certificate cache is verification-gated and bounded.**
  Inbound attested claims previously cached their wire certificate before
  checking the quorum, so unauthenticated gossip could poison what registry
  sync serves; the cache also grew without limit. Certificates are now cached
  only after the attestor policy verifies them, and the cache is capped at
  `MAX_FUNDING_CERTIFICATES` (4096). The same test above proves a forged
  certificate leaves the cache empty.
- **Cover traffic has an operator switch.** `--cover-traffic` enables the
  existing `CoverDispatch` loop in `NodeEngine`: decoy 512-byte cells over
  randomized paths, scheduled at exponential intervals (5-120s, mean 30s),
  with `pwb_onion_cover_cells_sent_total` exposed in `/metrics`. It stays off
  by default because it costs bandwidth. Covered by
  `cover_traffic_is_opt_in_and_emits_decoy_cells`.
- **Dials retry through packet loss.** `NodeEngine::dial` now resends its
  signed Ping up to four times inside the caller's timeout instead of
  declaring a peer dead after one lost datagram. A deterministic lossy UDP
  shim in `pwb-core/tests/packet_loss.rs` drops the first two probes in each
  direction and proves the dial still succeeds.
- **Fuzzing runs in CI.** `.github/workflows/fuzz.yml` runs the onion-cell,
  JSON-message, bundle, and PML/PSS parser targets for 30 seconds per target on
  relevant pull requests and five minutes each weekly (plus manual dispatch).
  The workflow caches corpora under `fuzz/corpus/` and uploads crash artifacts.
  The local limitation remains: this environment has no C++ toolchain, so the
  targets were not executed here.

### Open after the 2026-10-23 public-network audit

This pass re-checked every path advertised in the READMEs against the running
binaries. The items below are the remaining public-network gaps; they are
listed here so they are not mistaken for finished work.

- **Bootstrap trust defaults to a local view.** Without pinned anchors,
  funding certificates are checked against an attestor neighborhood derived
  from the receiver's live peer table (`DEFAULT_QUORUM_K = 5`, shrinking to the
  number of known peers), so a fresh node whose only peer is malicious can be
  lied to. Operators can close this with `--trust-anchor` /
  `PWB_TRUST_ANCHORS`, and a published descriptor can carry anchors, but the
  remaining work is a well-known anchor set whose signer key clients obtain
  independently of the descriptor host (a release note or project site).
  Shrinking `k` is a deliberate trade-off for small networks, not a
  verification bypass. The collect side (attested claims, withdrawals, and
  the `--status` balance certificate) now uses the same anchor set when
  configured, and dials anchors through the DHT when they are not direct
  peers, so a stranger on a fresh bootstrap can collect a certificate the
  anchored verifier accepts.
- **No public seed is hosted yet.** Clients can now discover seeds from a
  published descriptor (`deploy/join.json`, or `--join` / `PWB_JOIN` with
  `--join-signer` pinning), and `--write-join` produces signed descriptors, but
  the repository's descriptor has an empty seed list. Until an operator runs a
  seed and publishes it, a flagless node prints "No public seed is published
  yet" and falls back to LAN discovery. Bootstrap entries may use DNS names,
  and [`docs/seed-node.md`](docs/seed-node.md) plus
  [`deploy/pwb-seed.service`](deploy/pwb-seed.service) provide the deployment
  recipe; hosting and funding the seed is operational work, not a protocol gap.
- **Real-topology validation is incomplete.** NAT traversal, relay fallback,
  registry sync, batching, and peer exchange are tested on loopback with small
  clusters. A deterministic lossy-UDP shim now covers retry-on-loss for dials
  (`pwb-core/tests/packet_loss.rs`), but real NATs, asymmetric loss, jitter,
  and hundreds of nodes are still not exercised.
- **Documented privacy limits remain.** Cover traffic is opt-in via
  `--cover-traffic` and off by default, the onion replay cache rejects
  identical ciphertext but has no circuit id (so a re-encrypted replay is
  indistinguishable from a fresh cell), and multi-cell messages pad to fixed
  4 KiB/48 KiB envelopes but still share a fragment id and timing. Padding
  hides exact bulk-transfer lengths and their cell counts, not the fact that
  one circuit carried a message. These limits are stated in
  `docs/security.md`, not hidden claims.
- **Fuzzing coverage is still narrow.** `fuzz/` targets the onion-cell, JSON
  message, `.pwb` bundle, and PML/PSS parsers, and `.github/workflows/fuzz.yml`
  runs them on relevant pull requests and weekly. There is still no target for
  the `.pwb` site-directory builder (which touches the filesystem).

Residual (accepted, not blocking local/public use): a node that does not pin
`--trust-anchor` still trusts its live peer view during bootstrap; see the
first item above. Validating on real multi-node internet topologies and
against a global passive adversary remains future work.

### Closed 2026-10-28

- **Full IPv6 support.** The UDP transport, STUN, LAN discovery, and
  bootstrap/seed resolution now handle both address families. `[::]` binds are
  explicitly dual-stack (outbound destinations are v4-mapped for the kernel,
  received sources canonicalized), `XOR-MAPPED-ADDRESS` encodes and decodes
  family `0x02` per RFC 5389 §15.2, discovery binds an IPv4 and a v6-only
  socket side by side on port 7922 and fans out to `ff02::1` / `ff05::255:1`
  alongside broadcast and `239.255.0.1`, and both binaries parse `--listen`
  before resolving bootstrap specs, `PWB_BOOTSTRAP`, join-descriptor seeds,
  and the STUN server so every dialled address matches the listen family.
  The default listen stays `0.0.0.0:0` (switching it to `[::]` would lose
  IPv4 on platforms that default to v6-only); `--listen [::]:7921` opts a
  node or seed into dual-stack, as documented in `docs/seed-node.md` and the
  runbook. Covered by the workspace test suite plus a live three-node run: an
  IPv6 peer and an IPv4 peer completed the handshake against one dual-stack
  socket at the same time.


---

## Platform support

### Closed 2026-10-29 — Windows

- **Default state paths resolve portably.** `pwb_core::paths::home_dir()`
  tries `$HOME`, then `$USERPROFILE`, then `%HOMEDRIVE%%HOMEPATH%`, and every
  default path (node `--data`, browser `--data`, the identity file,
  `session.json`, and the `localStorage` directory) now goes through it.
  Previously they read `$HOME` alone, which a stock Windows shell never sets:
  both binaries panicked at `no identity path available (set PWB_IDENTITY or
  $HOME)`, the data directories fell back to `pwb-data`/`.pwb` inside whatever
  directory the process was started from, and `default_session_path()`
  returned `None`, silently disabling tab/bookmark persistence. Covered by
  `paths::tests` (precedence, empty values, `HOMEDRIVE`+`HOMEPATH`
  composition).
- **Tests no longer assume `/tmp`.** The two `pwb-browser` bundler tests used
  literal `/tmp/pwb_bundle_*` paths; every other test already used
  `std::env::temp_dir()`, which is `%TEMP%` on Windows.
- **`in_path` is Unix-only.** The `PATH` probe behind the native-dialog
  availability check only runs on Unix (Windows uses the TaskDialog API and
  never probes), so it is `#[cfg(unix)]` now instead of dead code that broke
  the `-D warnings` gate on Windows.
- **LAN discovery actually broadcasts.** The fanout sends to
  `255.255.255.255`, which the kernel refuses without `SO_BROADCAST` — `EACCES`
  on Unix, `WSAEACCES` on Windows — so only the multicast datagram ever
  left the IPv4 socket. The discovery socket sets `set_broadcast(true)` after
  binding, restoring the documented broadcast + multicast fanout on every
  platform.
- **CI builds and tests on Windows.** A `windows-latest` job runs
  `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo test --workspace` alongside the Linux job (format and coverage stay
  on Linux). Verification performed here was cross-target: `cargo check
  --workspace --all-targets --target x86_64-pc-windows-gnu` compiles clean;
  running the Windows binaries needs the CI job or a Windows machine.

---

## Production-readiness backlog (2026-10-31)

These are the gaps the production-readiness audit still lists as open. They are
ordered by risk; operator/design/external work is separated at the end so it
is not mistaken for a missing local feature. Check items off as they land.

- [x] **Bundle authorship is signed and verified.** `Bundle` now carries an
  optional Ed25519 `author_signature` over the canonical bytes
  (`PWB-AUTHOR-V1` domain tag); `Bundle::deserialize` rejects a named author
  with a missing or invalid signature. `pwb-web --author-key` (or
  `PWB_AUTHOR_KEY`) signs at compile time. Unsigned bundles keep the old byte
  layout. Revocation is still undefined.
- [x] **PlainJS pages are isolated.** Top-level navigation resets the QuickJS
  context and page-scoped DOM/async state (handlers, timers, fetch callbacks,
  detached nodes, focus, pending actions); devtools are re-mounted and their
  script re-run in the fresh context.
- [x] **Artifact decode errors surface in the UI.** A malformed bundle or an
  invalid author signature now renders a native "cannot open page" page with
  the decode reason instead of printing to stderr and leaving the old page.
- [ ] **Script and event-handler errors surface.** Failed page scripts and
  event handlers still print to stderr; add a visible page/status banner so a
  broken handler is not silent.
- [ ] **Browser downloads.** Links marked `download` are followed as ordinary
  navigations; there is no save path or download state.
- [ ] **PlainJS modules are not silently mangled.** `strip_esm_syntax` is a
  line filter: multi-line imports, `export default`, and strings that start
  with `import` are not handled correctly, and `pwb.import` re-evaluates the
  original (untransformed) module source. Either compile a well-defined
  script-mode subset or reject unsupported ESM with a clear build error.
- [ ] **`bundle.toml` is honored or removed.** The manifest exists in
  `pwb-browser/webpages/devtools/` but no crate reads it. Define its role
  (title/author/assets) and implement it, or delete the stale format.
- [ ] **Site-directory fuzz target.** `fuzz/` covers the parsers but not the
  filesystem-facing site builder; add a bounded target and wire it into the
  fuzz workflow.
- [ ] **CI coverage threshold.** Coverage is uploaded but never enforced, so
  a change can delete tested paths without failing CI.
- [ ] **Distributed tracing.** Structured logs, metrics, and health endpoints
  exist; there are no trace/span ids that follow a query across peers.
- [ ] **Async panic cleanup.** Many `unwrap()`/`expect()` calls remain in
  async tasks, timer paths, and mutex guards; a panic silently stops a
  subsystem. Audit and convert reachable ones to logged errors.
- [ ] **Search link/author/freshness signals.** Ranking is IDF/title based;
  trustworthy link-graph and publication-time signals need a signed protocol,
  not a local heuristic. This is design work.
- [ ] **Operator/external work:** publish a real seed and anchor set, validate
  large/NAT topologies, decide open-network sybil economics, and commission an
  independent security review. These cannot be completed honestly by a local
  checkout.

---

## Next steps (open, not tracked here)

The core modules are implemented; the following are suggested directions, not
obligations:

### Make the browser not ugly as fuck

The current renderer is functional but bare-bones. Obvious cosmetic work:
- Real CSS-like styling beyond the minimal PSS subset (borders, rounded corners, shadows, flexbox gaps, proper typography scale)
- A default theme that doesn't look like a debug terminal (sensible colors, spacing, component chrome)
- Smooth transitions / animations instead of instant layout jumps
- HiDPI / scaling awareness beyond the viewport-relative font hack

### Add more elements

Current tag coverage is still limited. Done: `<image>`/`<img>` PNG rendering and
`<a>`/`<link>` click navigation. Gaps:
- `<input type="checkbox">`, `<select>`, `<textarea>` — form widgets
- `<video>` / `<audio>` placeholders at minimum
- `<table>` / `<list>` structures
- Components / `<import>` reuse across bundles (partially wired in `pwb-web` but not fully exposed)

### Test on larger networks

Everything is validated on loopback / tiny clusters. Real-world validation:
- Multi-node LAN / internet deployments (NAT traversal, not just `127.0.0.1`)
- Larger DHT peer tables (hundreds of nodes, not 3–4)
- Gossip merge with many partitions, not just pairs
- Pricing engine under sustained registration load, not synthetic epochs
- Cover traffic and onion routing over real network latency / packet loss

---

## Implemented: Network Join (P2P message transport)

P2P message transport is implemented. `pwb-core::net::NodeEngine` runs a full
UDP transport stack:

- **NodeEngine::start** binds a UDP socket and spawns a sender loop, a
  receiver loop (verifying signed envelopes, reassembling fragments, dropping
  replays), a peer maintenance/liveness loop, and a [`DiscoveryService`]
  sharing the engine's peer table for LAN beacon discovery.
- **NodeEngine::send_to / broadcast** serialize [`NetworkMessage`] into signed
  envelope datagrams and transmit them to registered peers over UDP. Nothing
  is delivered locally: the sender already knows what it sent, and
  self-injection used to register its own key at the peer's address.
- **DiscoveryService** binds UDP port 7922 (falling back to an ephemeral port
  when another local node holds it), advertises the engine's actual bound
  listen port, ignores its own beacons, and merges discovered peers into the
  same [`PeerInfo`] table the engine uses. Loopback-only nodes skip discovery.
- **Liveness**: every authenticated datagram refreshes `last_seen`, and the
  heartbeat pings peers so an active peer never expires. `dial` sends a Ping
  and waits for a Pong; `connect_bootstrap_verified` reports only bootstraps
  that answered.
- **Connection links** (`plain://nodes.plain?connect=<ip:port>/<pubkey>`)
  generated with the node's actual bound address; `--connect` dials the peer
  and verifies it with a liveness probe before reporting success.
- **Integration tests** in `pwb-node/tests/network_join.rs` prove two live
  nodes on loopback UDP can connect via links and exchange `PeerDiscovery`,
  `SearchQuery`, and `GossipAnnounce` messages end-to-end.

[`NodeEngine`]: pwb_core::net::NodeEngine
[`PeerInfo`]: pwb_core::net::PeerInfo

---

## Contributing

See `CONTRIBUTING.md` for build, lint, test, and coverage requirements, plus
the no-purely-AI-generated-code policy.
