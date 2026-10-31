# pwb-node — layer 3: hoster + miner & system domain protocols

A **serverless** network participant. There are no servers and no work pools:
indexing, ranking, signed claim validation, and domain allocation are all done
*by the network* and sharded across nodes. A running node relays onion cells
and mints relay credits from them, verifies peers' index proofs against the
actual bundle bytes, and gathers/answers `k`-of-`N` balance attestations.

## Running a node

```bash
# Join an internet swarm (required across NAT; LAN discovery needs no flag).
# The host may be a DNS name, so a seed can move without changing the link.
pwb-node --bootstrap '<host:port>/<hex_pubkey>'
# or: PWB_BOOTSTRAP='<host:port>/<hex_pubkey>,<host:port>/<hex_pubkey>' pwb-node

# Do not contact a STUN server (LAN-only reachability, no public-IP disclosure)
pwb-node --no-stun

# Bind a specific UDP address. The default 0.0.0.0:0 serves IPv4 peers only;
# [::]:0 binds a dual-stack socket that serves IPv4 and IPv6 peers at once.
# The listen family also picks the family for resolved bootstrap seeds and
# the STUN server, so an IPv6-only node is never handed an IPv4 address.
pwb-node --listen '[::]:7921'

# Pin the attestors whose k-of-N certificates may fund domain claims. Repeat
# the flag, or set PWB_TRUST_ANCHORS to a comma-separated list. Without pins a
# node verifies funding against the live peer neighborhood instead.
pwb-node --bootstrap '<host:port>/<hex_pubkey>' \
  --trust-anchor <attestor_hex_pubkey> --trust-anchor <attestor_hex_pubkey>

# Opt into decoy onion cells (randomized timing; costs bandwidth)
pwb-node --cover-traffic

# Host and publish a site directory (or an existing .pwb bundle)
pwb-node --host ./mysite

# Join a peer, run one federated query, and exit
pwb-node --connect 'plain://nodes.plain?connect=<ip:port>/<pubkey>' --search 'term'

# Claim a .pwb name for the content you host (a bundle of at least 640
# compressed bytes earns the 10-credit index reward that covers the initial
# claim price; an existing owner can re-point a name for free)
pwb-node --host ./mysite --claim mysite.pwb

# Ask the swarm for a private (onion-routed) search answer; the exit replies
# through the return circuit and never learns this node's address
pwb-node --bootstrap '<host:port>/<hex_pubkey>' --private-search 'term'

# Chat with a peer from the daemon itself: an E2EE REPL over stdin that
# replies with SendFlags::E2EE (transcripts persist under <data>/chat.json).
# --chat-via picks the route: direct (default), onion (a three-hop circuit,
# needing three direct peers), or auto (circuit when possible, loud fallback).
# The peer's delivery receipt settles each line: confirmed, or unconfirmed
# after 8 seconds.
pwb-node --connect '<link>' --chat <peer_hex_pubkey> --chat-via direct

# Post to a public room from the daemon: every node joins `lobby` by
# default, and a post is signed by this identity and flooded through every
# peer (members keep the history, bystanders relay it). --room opens a REPL
# for one room; --chat and --room each own stdin, so run one at a time.
pwb-node --connect '<link>' --room lobby

# Show credits earned by service, the peer-attested balance, and the wallet
pwb-node --status

# Convert attested credits into a blind bearer voucher (one denomination each)
pwb-node --withdraw 10 [--issuer <hex_pubkey>] [--voucher-file <path>]

# Claim a name against an attested balance quorum, or privately with a voucher
pwb-node --host ./mysite --attested-claim mysite.pwb
pwb-node --host ./mysite --blind-claim private.pwb
```

`--withdraw` asks connected attestor peers (or the one named by `--issuer`) for
one blind voucher of the requested denomination and stores it in
`<data>/vouchers.json` (mode 0600 on Unix). `--blind-claim` spends the smallest
stored voucher that covers the current rolling claim price and removes it once
the claim is accepted. `--attested-claim` gathers a quorum certificate from the
deterministic neighborhood of live peers and spends attested balance instead.
Both one-shot claim paths gossip the signed record like `--claim`. Plain
`--claim` is deferred rather than fatal when the ledger is short: the node keeps
serving and retries the claim every 15 seconds, so it can earn the price from
the traffic it handles instead of exiting before it serves anything.

`--trust-anchor` changes the funding-authority rule for both gossiped claims
and registry sync: the certificate must carry `min(5, anchor count)` distinct
anchor signatures, and the live peer table is not consulted. Pinning one key
means 1-of-1, three keys means 3-of-3; these are operator trust decisions, so
the startup banner prints the active policy. Without anchors the historical
deterministic neighborhood applies, which is only as independent as the peers
the node already knows.

The node mounts `DistributedSearch` (shard queries, posting storage, artifact
fetch, artifact gossip) and `SystemDomainHandler` (`search.plain` queries,
`FetchArtifact`) as soon as it starts, so hosted content is served to the
network. `--host` publishes after the bootstrap/join attempt and announces the
artifact with `GossipAnnounce`, so peers fetch and index it. `--demo` runs the
old synthetic workload instead of real work. Hosted artifacts, the search
index, the domain registry, credits, `chat.plain` transcripts (`chat.json`),
public-room history and memberships (`rooms.json`),
and the peer table persist under `--data
<dir>` (default `~/.pwb/data`, `%USERPROFILE%\.pwb\data` when `$HOME` is
unset as it is on a stock Windows shell) and reload at startup. On restart the node
re-dials the 32 most recently seen remembered peers in parallel, so it can
rejoin the swarm even when no bootstrap is reachable.

## Joining without a hand-copied link

The task-oriented walkthrough — start, connect to a peer, serve a site, query the swarm — is
[Join the network](../docs/join-the-network.md). The details below are the reference for the same flags.

A node with no `--connect` link or `--bootstrap` entry fetches the project's
published join descriptor (`deploy/join.json`) and joins the seeds it lists.
That is how a stranger gets onto the network without being sent a
`plain://nodes.plain?connect=…` link first:

```sh
pwb-node                          # fetch the default descriptor, then join
pwb-node --join https://example.org/join.json   # use your own mirror
pwb-node --no-join                # never fetch the default; LAN discovery only
```

The descriptor is only a rendezvous/attestor directory: every listed seed is
still proven with a Ping/Pong round-trip before it is counted as joined, and
the descriptor's trust anchors are used only when no `--trust-anchor` was
passed. `--join` accepts an HTTPS URL, a `file://` path, a filesystem path, or
an inline JSON document (bounded to 256 KiB). An explicit `--bootstrap` or
`PWB_BOOTSTRAP` entry suppresses the default fetch, so private and LAN
deployments never depend on GitHub. `--join-required` fails at startup when no descriptor seed (or
remembered peer) answers a liveness probe, instead of silently running
standalone or falling back to LAN discovery.

A published descriptor may be signed. The first signer a node sees is pinned
in `<data>/join-signer.json`, so a later mirror cannot silently replace the
seed list; `--join-signer` pins the key up front and
`--require-join-signature` rejects unsigned documents outright.

### Writing the descriptor you publish

An operator who runs a seed (see
[`docs/seed-node.md`](../docs/seed-node.md)) can write the descriptor from
their own configuration. This is a maintenance command: it writes JSON, never
starts the engine, and never writes a secret.

```sh
pwb-node --write-join deploy/join.json \
  --join-seed 'seed.example.com:7921/<seed_hex_pubkey>' \
  --join-expires 1800000000 \
  --trust-anchor <attestor_hex_pubkey>
# [Join] Wrote 1 seed(s) to deploy/join.json
# [Join] Signer public key: <hex>
```

`--write-join-key <identity file>` signs with a dedicated key instead of the
node's identity. Commit the file to the default URL, or host it anywhere and
point clients at it with `--join`; publish the signer key in the same place so
clients can pin it. `--join-seed` is repeatable, `--join-expires` is optional
(unix seconds), and anchors come from `--trust-anchor` or the seed's existing
configuration.

To withdraw a published seed list, add `--withdraw-join`. It signs a
descriptor with zero seeds and cannot be combined with `--join-seed`:

```sh
pwb-node --write-join deploy/join.json --withdraw-join --join-expires 1800000000
```

Clients with `--join-required` then exit instead of dialling the revoked seed;
flagless clients fall back to LAN discovery. The signature is what
distinguishes an intentional withdrawal from a broken mirror.

Before publishing, check the file exactly as a client will. `--verify-join`
parses the descriptor, enforces its signature and expiry, and exits non-zero on
any problem without starting the engine or creating an identity:

```sh
pwb-node --verify-join deploy/join.json \
  --join-signer <signer_hex> --require-join-signature
```

[`deploy/publish-join.sh`](../deploy/publish-join.sh) automates the whole
release step (key creation, atomic write, verification) and refuses to keep a
signing key inside the repository. The surrounding procedure is the
[publish-a-seed runbook](../docs/runbooks/publish-a-seed.md).

## Native System Domain Handlers

`pwb-node` serves native system protocols and dynamic interfaces:
- **`search.plain`**: Sharded decentralized search engine index & ranking math. Querying `search.plain` returns verified index search results. Each result title and its "Visit Site" affordance are native `content.pwb/<cid>` links, so opening a hit needs no JavaScript.
- **`register.plain`**: Sparse credit domain registry interface. Backed by `domain::DomainService`: signed claims for the unofficial TLDs (`.pwb`, `.non`, `.yep`, `.lam`, `.arch`, `.vio`, `.meh`, `.son`, `.isuearchbtw`, `.org`, `.net`, `.com` — see [docs/tlds.md](../docs/tlds.md)) gossip across the swarm, late joiners sync the registry, unknown names resolve on demand, and records persist to disk. Claims debit the signed credit ledger, which is funded by verified query/index/relay/seed work; a fresh node starts at 0 credits.
- **`gossip.plain`**: P2P gossip engine for artifact propagation.
- **`nodes.plain`**: Peer routing table and network health.
- **`network.plain`**: Live listen/discovery/STUN addresses, the peer table, and this node's index/store/registry counts, claim price, and balance.
- **`help.plain`**: System-domain index plus the namespace, hosting, and credit rules.
- **`chat.plain`**: Direct E2EE peer conversations, plus the public rooms at
  `chat.plain?room=<name>` — backed by `room::RoomService`: author-signed
  posts flood the network hop by hop, memberships are announced one hop at a
  time, late joiners sync history from peers, and transcripts persist under
  `--data`.

## Module Structure

- `host`      — ingest `.pwb` artifacts from layer 1; serve them to peers.
- `search`    — decentralized inverted index + term-frequency ranking.
- `federation`— DHT-sharded posting replication, shard queries, artifact fetch.
- `attestation` — networked balance attestations, blind issuance, remote proof verification.
- `ledger`    — sharded peer-attested chains, quorum certificates, attestor selection.
- `blind`     — blind-signature issuer keys, withdrawals, private claims.
- `relay`     — meter relayed onion cells and mint relay credits.
- `seed`      — meter rendezvous work (newcomers admitted, peer tables served) and mint seed credits.
- `wallet`    — CLI voucher custody, attested claims, withdrawal helpers.
- `verifier`  — index-proof signing/checking primitives.
- `identity`  — persisted Ed25519 keypair identity, on `pwb-core::crypto`.
- `credit`    — mint policy/receipts plus the signed hash-chained credit ledger
  (`credits.json`), which is also the spend gate for domain claims.
- `domain`    — credit → domain allocation registry.
- `system`    — native pages for every system domain, including `network.plain` and `help.plain`.

## Documentation

- [Running a public seed / rendezvous node](../docs/seed-node.md) — systemd unit, firewall, link, and anchor publication.
- [Protocol Reference](../docs/protocol.md) — pinned network parameters and the `.plain` security rule.
- [§1 Core Architecture](../docs/design/01-core-architecture.md) — the shared `pwb-core::net::NodeEngine` this node runs on.
- [§3 Decentralized Node Discovery](../docs/design/03-node-discovery.md) — LAN beacons, connection links, and QR pairing.
- [§4 Anti-Sybil Credit System](../docs/design/04-credit-system.md) — `credit`, `ledger`, and `blind` minting/attestation.
- [§5 Pricing & Gossip Merging](../docs/design/05-pricing-and-merge.md) — `domain` rolling-average pricing and merge reconciliation.

Browse everything from the [docs index](../docs/README.md).

## Operations: health, metrics, and logs

A node exposes a read-only loopback HTTP endpoint for supervisors:

```sh
pwb-node                                   # http://127.0.0.1:7711/health
pwb-node --health-addr 127.0.0.1:9001      # override
pwb-node --no-health                       # disable

curl -s http://127.0.0.1:7711/health       # {"status":"ok", ...}
curl -s http://127.0.0.1:7711/status       # addresses, peers, index/store/registry
curl -s http://127.0.0.1:7711/metrics      # Prometheus text counters
```

`/health` returns `503` until the UDP socket is bound. Counters cover
datagrams and bytes in/out, authenticated messages, rate-limit and replay
drops, onion cells peeled/relayed, cover cells emitted, relayed deliveries,
dials, and hole-punch outcomes. Set `PWB_LOG_FORMAT=json` for one JSON event
per line on stderr.

NAT traversal has three stages: direct dial, rendezvous hole punching
(`PunchRequest`/`PunchSignal` plus signed probes), then a one-hop
`RelayDeliver` fallback whose inner envelopes stay end-to-end encrypted. The
`--connect` link, remembered peers on restart, and the `nodes.plain` connect
action use `dial_resilient`.
