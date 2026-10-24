# Runbook: publish a public seed and its join descriptor

This is the end-to-end operational procedure for taking PlainWeb from "nobody
can join" to "a stranger installs a client, runs it with no arguments, and is
on the network". It covers one host and one signing key; repeat the seed step
for each additional seed and list them all in one descriptor.

Use it with:

- [`deploy/pwb-seed.service`](../../deploy/pwb-seed.service) — systemd unit for the seed
- [`deploy/publish-join.sh`](../../deploy/publish-join.sh) — regenerates and verifies the signed descriptor
- [`docs/seed-node.md`](../seed-node.md) — background on what a seed is and is not

## What you need

| Item | Notes |
|------|-------|
| A host with a public IPv4 address | A VPS with a directly routable address is simplest; a home host needs UDP port-forwarding |
| UDP 7921 reachable inbound | This is the only port the public needs. UDP 7922 is LAN discovery only |
| A built `pwb-node` | `cargo build --release -p pwb-node` |
| A separate release machine | Where the signing key lives; it never needs to be the seed |
| A stable HTTPS location | Default: `deploy/join.json` in this repository (raw GitHub URL) |

The signing key is a **release credential**. It is not the seed's identity and
does not belong on the seed host. If it leaks, an attacker can publish a
descriptor that points clients at their own seed.

## 1. Install and start the seed

On the seed host:

```sh
cargo build --release -p pwb-node
sudo groupadd --system pwb 2>/dev/null || true
sudo useradd --system --gid pwb --home /var/lib/pwb \
  --shell /usr/sbin/nologin pwb 2>/dev/null || true
sudo install -d -o pwb -g pwb -m 0750 /var/lib/pwb
sudo install -m 0755 target/release/pwb-node /usr/local/bin/pwb-node
sudo install -m 0644 deploy/pwb-seed.service /etc/systemd/system/pwb-seed.service
sudo systemctl daemon-reload
sudo systemctl enable --now pwb-seed
```

Open the port (pick one):

```sh
# ufw
sudo ufw allow 7921/udp

# nftables
sudo nft add rule inet filter input udp dport 7921 accept
```

Confirm the process, socket, and health endpoint:

```sh
systemctl status pwb-seed
sudo ss -lunp | grep 7921
curl -s http://127.0.0.1:7711/health
```

Keep `--health-addr` on loopback. It is read-only but reveals operational state.

## 2. Capture the seed's public link

The first start creates the node identity under `/var/lib/pwb` and prints the
connection link:

```sh
sudo journalctl -u pwb-seed -b --no-pager | grep 'plain://nodes.plain'
```

```text
plain://nodes.plain?connect=seed.example.com:7921/<seed_hex_pubkey>
```

The link contains only the address and public key, so it is safe to paste into
the descriptor step. Never copy `identity.json` off the host: it is the seed's
private key. Back it up encrypted if you would rather not regenerate the seed
identity when the host is rebuilt.

Before publishing, confirm a node can dial it from a different network. On any
other machine:

```sh
pwb-node --bootstrap 'seed.example.com:7921/<seed_hex_pubkey>' \
  --data /tmp/pwb-seed-check --no-health
```

Wait for `Joined via 1 bootstrap peer(s)`, then `Ctrl-C` and remove
`/tmp/pwb-seed-check`. On the seed, `curl -s http://127.0.0.1:7711/metrics |
grep pwb_peers_connected` should now show at least one peer. If it does not,
stop here: a descriptor pointing at an unreachable seed is worse than none.

## 3. Create the release signing key

Run this on the release machine, once. The script creates the identity on first
use with mode `0600` and refuses to keep it inside the repository.

```sh
bash deploy/publish-join.sh --help
```

Back the key up somewhere durable and encrypted (password manager, sealed
offline copy). Its public half is the value clients pin; the secret half is
never printed by the script and must never be committed, pasted into an issue,
or stored in CI logs.

## 4. Generate the descriptor

From a checkout of the release machine:

```sh
bash deploy/publish-join.sh \
  --seed 'plain://nodes.plain?connect=seed.example.com:7921/<seed_hex_pubkey>' \
  --anchor <attestor_hex_pubkey> \
  --expires-in 180
```

What it does:

1. normalizes the seed link and rejects anything that is not `host:port/<64-hex key>`;
2. creates or reuses `~/.pwb/join-release-identity.json` (`0600`);
3. writes the descriptor atomically to `deploy/join.json`;
4. re-reads it with `pwb-node --verify-join`, enforcing signature and expiry;
5. prints the public signer key.

Useful variations:

```sh
# Several seeds, no expiry, custom output
bash deploy/publish-join.sh --no-expiry \
  --seed 'seed-a.example.com:7921/<hex>' \
  --seed 'seed-b.example.com:7921/<hex>' \
  --output /tmp/join.json

# Seeds from a file (one link per line, # comments allowed)
bash deploy/publish-join.sh --seeds-file ops/seeds.txt --expires-in 90

# A dedicated key and an explicit expiry
bash deploy/publish-join.sh --identity ~/.pwb/release-2027.json \
  --expires-at 1800000000 \
  --seed 'seed.example.com:7921/<hex>'
```

`--anchor` is optional but recommended. Without it, clients fall back to the
deterministic neighborhood of their live peers as the funding authority, which
is exactly the bootstrap-trust caveat in
[production readiness](../production-readiness.md). The startup banner of a
node reports which rule it is using.

The script never runs git. Review the diff yourself:

```sh
git diff -- deploy/join.json
```

The file is public data: seed addresses, public keys, anchors, signer, and
signature. It never contains a secret.

## 5. Publish the descriptor and the signer key

Default channel (the URL clients fetch with no flags):

```sh
# after review, through the project's normal commit process
git add deploy/join.json deploy/publish-join.sh
git commit -m "Publish <n> seed(s) in the join descriptor"
git push
```

Any other stable HTTPS URL works too; clients use it with `--join <url>` or
`PWB_JOIN=<url>`. The descriptor is small and changes rarely, so a static file
or release asset is enough — no dynamic endpoint is needed.

Publish the signer key **out of band**, somewhere an attacker who controls the
descriptor host cannot also edit: a signed release note, the project website, a
pinned announcement, or inside the client release that will ship with the
descriptor. Clients then pin it:

```sh
pwb-node --join-signer <signer_hex> --require-join-signature
# or: PWB_JOIN_SIGNER=<signer_hex>
```

Without that pin, a client still verifies the signature and remembers the first
signer it sees (trust on first use, stored in
`<data>/join-signer.json`). That protects against a later mirror swap, not
against a compromised first fetch.

## 6. Verify what you published

Verify the exact bytes clients will fetch:

```sh
pwb-node --verify-join \
  https://raw.githubusercontent.com/plainprince/pwb/main/deploy/join.json \
  --join-signer <signer_hex> --require-join-signature
```

Expected shape:

```text
[Join] Verified https://…
[Join] Signer: <hex>
[Join] Expires: epoch …, in 179 day(s)
[Join] Seeds: 1
  seed.example.com:7921  <hex>
[Join] Trust anchors: 1
  <hex>
```

Then exercise the stranger path from a machine that is not on the seed's host
or LAN:

```sh
pwb-node --join-required \
  --join-signer <signer_hex> --require-join-signature \
  --data /tmp/pwb-stranger --no-health
```

Success is `[Join] Descriptor published 1 seed(s) …` followed by
`Joined via 1 bootstrap peer(s)`. Then `Ctrl-C` and delete `/tmp/pwb-stranger`.
`--join-required` exits non-zero when no seed answers, which is what makes this
a real test rather than a log line.

Browser clients accept the same join flags; their search and content fetches
are onion-routed by default.

## 7. Rotate, move, and revoke

| Situation | Action |
|-----------|--------|
| Descriptor nearing expiry | Re-run the script with the same key. Clients accept the new document; the signer pin still matches |
| Seed moves to a new address | Keep the seed's `identity.json`, update `--seed` to the new `host:port`, regenerate. The key is what clients pin, so no re-pin is needed |
| Add a second seed | Add another `--seed`; publish one descriptor listing both |
| Seed host or key compromised | Rebuild the seed with a fresh identity, publish a descriptor that lists only healthy seeds, and stop the old host. Clients that still hold the old descriptor keep dialling it until expiry, so also unpublish or replace the old descriptor URL |
| Release signing key compromised | Generate a new key and publish its public half out of band. Already-pinned clients will reject the new descriptor until they pass `--join-signer <new>` or delete `<data>/join-signer.json`; treat this as an incident, not a routine rotation |
| Emergency: no healthy seed, or a seed key is untrusted | Publish a signed withdrawal (`--withdraw`). Clients with `--join-required` exit by design instead of dialling an untrusted host; flagless clients fall back to LAN discovery. Republish the seed list once a healthy seed exists |

Expiry is deliberate: a stale mirror must not keep pointing clients at an
address that changed hands. Put the renewal date on a calendar. `--no-expiry`
is fine for a lab, not for a public deployment.

### Withdrawing the seed list

When the right move is "stop dialling every seed" — a leaked seed identity, a
host you no longer control, or a network-wide shutdown — publish a signed
withdrawal instead of deleting the file or reverting it to the unsigned
placeholder:

```sh
bash deploy/publish-join.sh --withdraw --no-expiry
```

This signs a descriptor with zero seeds and verifies it exactly like a seed
list. Clients with `--join-required` exit with "the published descriptor
withdraws all seeds"; flagless clients fall back to LAN discovery. Because the
withdrawal is signed by the same key, clients can tell an intentional shutdown
from a broken mirror or a stripped signature.

To restore service, rerun step 4 with healthy seeds. The signing key is
unchanged, so clients that pinned it accept the new document without any
reconfiguration. `--withdraw` cannot be combined with `--seed` or
`--seeds-file`; it ignores `PWB_BOOTSTRAP` entirely, so a stray environment
variable cannot turn a withdrawal into a seed list.

## Troubleshooting

| Symptom | Likely cause and fix |
|---------|----------------------|
| `[Join] no seed list is published at … yet (HTTP 404)` | Nothing is published at that URL: wrong path/branch, or the file was never pushed. Check with `curl -sI <url>` |
| `[Join] fetch failed: …` (any other status) | DNS, TLS, or the mirror is down. Check from the client network with `curl -v <url>` |
| `join descriptor has expired` | Re-run step 4 and republish; check clients are not using a stale mirror |
| `join signer … does not match pinned signer` | Descriptor was signed by a different key, or the mirror was swapped. Confirm out of band before clearing the pin |
| `join descriptor is unsigned but a signature is required` | Use `--write-join` (always signs) and check the file was not edited by hand afterwards |
| `--join-required` exits 1 with `no usable join source` | Fetch failed; check DNS, HTTPS, and the descriptor URL |
| `--join-required` exits 1 with `the published descriptor withdraws all seeds` | Intended: the descriptor is a signed withdrawal. Republish a seed list to restore service |
| Descriptor verifies but the seed never joins | UDP 7921 blocked or the seed is down. Check `ufw status`, `ss -lunp`, and `journalctl -u pwb-seed`; retest from another network |
| `pwb-node … predates --verify-join` | Rebuild the binary (`cargo build --release -p pwb-node`) or pass `--node` |
| Script refuses a key inside the repository | Intended. Move `--identity` outside the checkout; do not use `--force-key-in-repo` for a real key |

## Pre-publish checklist

- [ ] `pwb-node --bootstrap <link>` joins from a different network
- [ ] `pwb-node --verify-join <published url> --join-signer <hex> --require-join-signature` passes
- [ ] Descriptor lists only healthy seeds and the intended anchors
- [ ] Signer key published somewhere independent of the descriptor host
- [ ] `deploy/join.json` diff contains no secret material
- [ ] Renewal date recorded; expiry set
- [ ] A fresh client with no flags joins, and `--join-required` exits cleanly when the seed is stopped
