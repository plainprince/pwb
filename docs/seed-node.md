# Running a public seed / rendezvous node

PlainWeb has no central server, but an internet join still needs one reachable
UDP address to start from. A **seed** is just a long-running `pwb-node` that
accepts those first connections, answers liveness probes, and gossips its peer
table through `PeerExchange`; it has no special authority over search, content,
or domain names.

The project does not operate a seed for you. This page and
[`deploy/pwb-seed.service`](../deploy/pwb-seed.service) are the deployment
assets for running your own.

## 1. Host and identity

Use a host with a public IPv4 **or IPv6** address (or a stable port-forward)
and open UDP 7921. An IPv6-only host works unchanged: pass
`--listen [::]:7921` and the node resolves seeds and STUN to IPv6 too. Reserve
UDP 7922 only if you also want LAN discovery on that host.

```sh
sudo groupadd --system pwb 2>/dev/null || true
sudo useradd --system --gid pwb --home /var/lib/pwb \
  --shell /usr/sbin/nologin pwb 2>/dev/null || true
sudo install -d -o pwb -g pwb -m 0750 /var/lib/pwb
sudo install -m 0755 target/release/pwb-node /usr/local/bin/pwb-node
```

The node creates its Ed25519 identity on first start. Run it once in the
foreground (or start the unit and read the log) to capture the public key:

```sh
sudo -u pwb /usr/local/bin/pwb-node \
  --listen 0.0.0.0:7921 --no-stun --data /var/lib/pwb
```

The startup banner prints `Node Identity Public Key` and the connection link:

```text
plain://nodes.plain?connect=seed.example.com:7921/<hex_pubkey>
```

That link is safe to publish: it contains only the address and public key.
Never copy the identity file (`identity.json`) off the host.

## 2. Run it under systemd

Copy [`pwb-seed.service`](../deploy/pwb-seed.service) to
`/etc/systemd/system/pwb-seed.service`, adjust `ExecStart` if the binary or
data path differs, then:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now pwb-seed
systemctl status pwb-seed
curl -s http://127.0.0.1:7711/health
curl -s http://127.0.0.1:7711/metrics | grep pwb_peers_connected
```

Keep `--health-addr` on loopback. The endpoint is read-only, but it still
reveals operational state and should not be exposed publicly.

The unit binds `--listen 0.0.0.0:7921`, which serves IPv4 peers only. On a
dual-stack or IPv6-only host change it to `[::]:7921`: one socket then serves
both families, and whatever seeds or STUN server the node resolves follow the
family it can dial. `ss -lunp | grep 7921` should show the address you
expect: `0.0.0.0:7921` for the default, `[::]:7921` for a dual-stack bind.

## 3. Publish the join information

Write a signed descriptor that clients fetch on their own. This never starts
the engine, so it is safe to run on the seed host or on an operator machine:

```sh
pwb-node --write-join deploy/join.json \
  --join-seed 'seed.example.com:7921/<seed_hex_pubkey>' \
  --join-expires 1800000000 \
  --trust-anchor <attestor_hex_pubkey> --trust-anchor <attestor_hex_pubkey>
```

It prints `[Join] Signer public key: <hex>` and exits. Commit the file to the
repository's `deploy/join.json` (the default URL clients fetch), or host it at
any stable HTTPS URL and hand clients `--join <url>`. An explicit
`--bootstrap` / `PWB_BOOTSTRAP` still works and suppresses the default fetch;
either way the seed key is pinned, so a changed DNS answer cannot substitute a
different identity.

Descriptors written by `--write-join` are signed with the node identity, or
with `--write-join-key <identity file>` when you want a dedicated release key.
Publish that signer key next to the descriptor and have clients pass
`--join-signer <hex>` (optionally with `--require-join-signature`). The first
signer a client sees is also remembered under `--data`, so a compromised
mirror cannot silently swap the seed list later.

[`deploy/publish-join.sh`](../deploy/publish-join.sh) wraps that command for a
release: it validates the seed links, creates a `0600` signing key outside the
repository, writes the descriptor atomically, verifies it with
`--verify-join`, and prints the public signer key. The full procedure —
including rotation and an emergency withdrawal — is in the
[publish-a-seed runbook](runbooks/publish-a-seed.md).

Check a descriptor (local or published) the way a client will, without
starting a node or creating an identity:

```sh
pwb-node --verify-join deploy/join.json \
  --join-signer <signer_hex> --require-join-signature
```

`--trust-anchor` is the independent funding authority; without it a node falls
back to the deterministic neighborhood of its live peers, which is exactly the
bootstrap-trust caveat described in
[production readiness](production-readiness.md). Publish anchor keys through a
channel that is not the seed's connection string (a signed release note, a
project website, or an operator config bundle), or the pin adds no independence.
Clients pick anchors out of the descriptor only when they were not pinned by
`--trust-anchor` themselves.

## 4. What the seed can and cannot do

- It can see the source addresses and timing of peers that connect to it, and
  it can drop or withhold traffic. It is an availability dependency.
- It **cannot** read onion payloads; onion cells are peeled per hop and the seed
  is only one hop unless a client chooses it as the exit.
- With pinned anchors it **cannot** invent funded domain claims: attestations
  must come from the pinned keys. Without pins it is only as trustworthy as the
  live peer neighborhood it supplies.
- It does not index the network by itself. Search and content still come from
  the peers that hold the shards and artifacts.

Rotate or move a seed by publishing the new `host:port/<hex>`; the key stays
stable only if you carry the identity file to the new host. If the key changes,
clients must update their `--bootstrap` entry.
