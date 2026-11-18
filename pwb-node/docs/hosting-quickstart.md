# Host a site — node operator quickstart

Running `pwb-node` publishes content to the PlainWeb network. There is no
upload step, no web server, and no HTTPS setup: connected peers fetch the
compiled bundle, index it, and answer searches for it. Your job is to keep one
UDP port reachable and one process alive.

This guide covers hosting a site on a server. To run a **seed / rendezvous**
node instead, see [Running a public seed](../../docs/seed-node.md).

## What you need

- A Linux/macOS/Windows host that can stay online. A public VPS is ideal; a
  home machine works if UDP is forwarded.
- **Outbound UDP** (to reach seeds and peers). **Inbound UDP 7921** if you want
  others to dial you directly; without it the node still joins, but peers need
  hole punching or a relay.
- Disk for the binary plus state (a few tens of MB), and room for hosted artifacts.
- No domain, TLS certificate, or reverse proxy. PlainWeb does not speak HTTP to
  clients; the only HTTP listener is the loopback health endpoint.

## 1. Build and install

```sh
git clone https://github.com/plainprince/pwb
cd pwb
cargo build --release
sudo install -m 0755 target/release/pwb-node /usr/local/bin/pwb-node
sudo install -m 0755 target/release/pwb-web  /usr/local/bin/pwb-web
```

You can also compile the site on your laptop and copy only the `.pwb` file to
the server; `pwb-web` is not needed there.

## 2. Create a service user and data directory

```sh
sudo groupadd --system pwb 2>/dev/null || true
sudo useradd --system --gid pwb --home /var/lib/pwb \
  --shell /usr/sbin/nologin pwb 2>/dev/null || true
sudo install -d -o pwb -g pwb -m 0750 /var/lib/pwb
```

The node creates its Ed25519 identity on first start. By default it goes to
`$HOME/.pwb/identity.json`; pass `--identity /var/lib/pwb/identity.json` so it
lives with the rest of the service state (the unit below does this). **Back this
file up and keep it private.** It is the key that signs your claims and chat; if
it is lost, your claimed names cannot be re-pointed by you, and a new identity
starts with 0 credits. The connection link printed at startup is public and safe
to share — it contains only the address and public key.

## 3. Build the site

On the server, or on your laptop then copied over:

```sh
pwb-web mysite mysite.pwb --author-key /var/lib/pwb/identity.json
```

Signing is optional, but a signed bundle is verified by the browser on open.
Hosting a **directory** (`--host ./mysite`) compiles it at node startup without
signing, so use the compiled `.pwb` if you want the author signature. See the
[pwb-web quickstart](../../pwb-web/docs/quickstart.md) for the source format.

The identity file only exists after the node's first start (step 4). Until
then, compile without `--author-key`, or start the node once, stop it, and
recompile signed.

## 4. Run the node under systemd

`/etc/systemd/system/pwb-site.service`:

```ini
[Unit]
Description=PlainWeb site node
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=pwb
Group=pwb
WorkingDirectory=/var/lib/pwb
ExecStart=/usr/local/bin/pwb-node \
    --listen 0.0.0.0:7921 \
    --data /var/lib/pwb \
    --identity /var/lib/pwb/identity.json \
    --host /var/lib/pwb/mysite.pwb \
    --claim mysite.pwb \
    --health-addr 127.0.0.1:7711
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/pwb
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

```sh
sudo install -o pwb -g pwb -m 0640 mysite.pwb /var/lib/pwb/mysite.pwb
sudo systemctl daemon-reload
sudo systemctl enable --now pwb-site
journalctl -u pwb-site -f
```

Notes on the flags:

- `--listen 0.0.0.0:7921` gives the node a stable UDP port. The default
  `0.0.0.0:0` picks a random port, which is fine for a client but not for a
  host that expects inbound connections. Use `--listen [::]:7921` on a
  dual-stack or IPv6-only host; that one socket serves both families.
- `--claim` is optional. It signs a name for the hosted content and gossips it
  to peers. See [step 6](#6-claim-a-name-optional).
- With no `--connect`/`--bootstrap`, the node fetches the published join
  descriptor and joins the public seed automatically. Add `--join-required` if
  you want the service to fail instead of running standalone when the seed is
  unreachable.
- Add `--no-stun` when the host has a directly routable address and you do not
  want it to contact a STUN server.

## 5. Verify it works

```sh
# Process alive, socket bound?
curl -s http://127.0.0.1:7711/health
curl -s http://127.0.0.1:7711/status
systemctl is-active pwb-site
ss -lunp | grep 7921

# Identity, credit balance, wallet, and what is claimed
sudo -u pwb pwb-node --data /var/lib/pwb \
  --identity /var/lib/pwb/identity.json --status
```

Then from a **different machine** on the network:

```sh
pwb-browser mysite.pwb
# or point it at this node explicitly while testing:
pwb-browser --bootstrap '<host:port>/<hex_pubkey>' mysite.pwb
```

The host key is in the `[Network] Your connection link:` banner line, e.g.
`plain://nodes.plain?connect=203.0.113.9:7921/<hex_pubkey>`.

If `--host` printed `[Host] Published …` and `--claim` printed `Claimed`, the
site is being served. Search takes a moment to propagate: peers must fetch and
index the bundle before its words appear in `search.plain`.

## 6. Claim a name (optional)

Hosting works without a name: content is addressed by content ID, and search
results link to `content.pwb/<cid>`. A name like `mysite.pwb` is a short alias
that resolves through the network's signed registry.

```sh
pwb-node --host ./mysite --claim mysite.pwb
```

- `.plain` is reserved and cannot be claimed. Claimable TLDs are listed in
  [docs/tlds.md](../../docs/tlds.md) (`.pwb`, `.non`, `.org`, and others).
- A claim costs credits. A fresh node starts at 0 and earns credits from
  verified work: indexing a hosted bundle, answering queries, relaying cells,
  and serving as a seed. The initial claim price is 10 credits, and indexing a
  hosted bundle of at least 640 compressed bytes earns 10 — enough to cover it.
- If the balance is short, the node **keeps serving** and retries the claim
  every 15 seconds. Watch `journalctl` for `[Claim] Claimed` or
  `[Claim] Refused for now`.
- To point an existing name at new bytes (a re-published site), claim the same
  name again with the new bundle; the owner can re-point it for free.
- `--attested-claim` (k-of-N attested balance) and `--blind-claim` (private,
  using a stored voucher) are alternative paths covered in the
  [node README](../README.md#running-a-node).

## 7. Update the site

The node compiles and publishes once at startup; it does not watch the folder.
To ship a new revision:

```sh
pwb-web mysite mysite.pwb --author-key /var/lib/pwb/identity.json
sudo install -o pwb -g pwb -m 0640 mysite.pwb /var/lib/pwb/mysite.pwb
sudo systemctl restart pwb-site
```

The new bundle has a new content ID. If the name should point at it, claim the
name again (owner re-points are free) or keep `--claim` in the unit, where it is
re-applied on every start.

Peers that already cached the old revision may keep serving it until their
store is refreshed; the owner-signed re-point is what resolves the name to the
new content ID.

## Operations

### Health and metrics

The node serves a read-only HTTP endpoint on loopback by default:

| Path | Purpose |
|---|---|
| `/health` | `200` once the UDP socket is bound, `503` before; JSON body. Use it for systemd/uptime checks. |
| `/status` | Listen/discovery/STUN addresses, peers, index/store/registry counts, claim price, balance. |
| `/metrics` | Prometheus text counters: datagrams/bytes, auth failures, rate-limit and replay drops, onion cells, relayed deliveries, dials, hole punches. |

Keep `--health-addr` on `127.0.0.1`. The endpoint is read-only, but it exposes
operational state. If a supervisor must scrape it over the network, put a
proxy in front and restrict access; `--health-public` only disables the
loopback requirement and does not add authentication.

For one JSON event per line on stderr (journald-friendly), set
`PWB_LOG_FORMAT=json` in the unit's `Environment=`.

### State and backups

Everything lives under `--data` (`/var/lib/pwb` here; default `~/.pwb/data`).
Writes are atomic (temp file + rename), so a crash cannot truncate a file.

| File | Contents |
|---|---|
| `identity.json` | Ed25519 keypair, when `--identity` points here; otherwise `$HOME/.pwb/identity.json` (created at startup) |
| `artifacts/` | Hosted `.pwb` bundles, reloaded on restart |
| `index.json` | Local search index |
| `domains.json` | Known signed domain claims |
| `credits.json` | Signed credit ledger |
| `attestations.json` | Peer attestations |
| `peers.json` | Remembered peers (up to 512, newest first) |
| `vouchers.json` | Bearer vouchers (mode 0600) |
| `chat.json`, `rooms.json` | Chat transcripts and public-room history |
| `join-signer.json` | Pinned join-descriptor signer |

Back up `identity.json` and `artifacts/` at minimum; the rest is rebuildable
from the network. Stop the service or copy with `sqlite`-style care is not
needed — files are replaced atomically — but a backup taken between writes is
always consistent.

### Upgrades

```sh
cargo build --release
sudo install -m 0755 target/release/pwb-node /usr/local/bin/pwb-node
sudo systemctl restart pwb-site
```

Built from different protocol versions, nodes ignore each other's traffic
(`Ignoring traffic from a peer speaking PlainWeb wire vN`). Upgrade a mixed
swarm in one window, or expect temporary isolation from old peers.

### Multiple sites

One process hosts one `--host` target. To host a second site, run a second
service with its own `--data` and `--listen` port — and be aware that credits,
identity, and claims are per data directory, so separate services do not share
a balance. A second service on the same host needs a different UDP port and a
different health port.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `[Host] '…' is not a valid .pwb bundle` | Wrong file, truncated copy, or source folder passed as a file. | Recompile with `pwb-web`, or pass the folder to `--host`. |
| `[Host] Failed to build '…': …` | PML/PSS compile error in the hosted folder. | Compile with `pwb-web` directly to see the full error; see the [authoring troubleshooting table](../../pwb-web/docs/quickstart.md#troubleshooting). |
| `/health` returns `503` | UDP socket not yet bound, or startup failed. | Check `journalctl -u pwb-site`; a bind conflict on 7921 is fatal. |
| `No bootstrap peers reachable` | Seed/peer down, wrong key, or outbound UDP blocked. | Try `--join-required` with the default descriptor, or `--connect <link>`; check the host firewall. |
| Node joins but nobody can dial it | Inbound UDP 7921 not open, or NAT without forwarding. | Open/forward UDP 7921; the connection link shows the address peers must reach. |
| `[Claim] Refused for now: need 10 credits, have 0` | New identity, no verified work yet. | Leave it running; indexing the hosted bundle earns credits and the claim retries every 15s. Check `--status`. |
| Claim says verified but name will not resolve elsewhere | The resolver is connected to no peer carrying the claim. | Stay connected; claims propagate by gossip. Retry from a node with peers (`network.plain`). |
| Permission denied writing `/var/lib/pwb` | File owned by root after a manual copy. | `sudo chown -R pwb:pwb /var/lib/pwb`. |
| Clock skew handshake failures | Envelopes reject clocks more than 10 minutes fast. | Enable NTP on the host. |

## See also

- [pwb-node README](../README.md) — every flag, join-descriptor details, system domains, and wallet commands.
- [Join the network](../../docs/join-the-network.md) — the shorter network walkthrough.
- [Security model](../../docs/security.md) — what onion routing protects and what it does not.
- [Production readiness](../../docs/production-readiness.md) — operational gaps to know before depending on a node.
- [Running a public seed](../../docs/seed-node.md) — if you also want to help newcomers join.
