# Join PlainWeb

From nothing to a node that serves content, connects to other nodes, and
answers queries. No account, no server to rent — a handful of commands.

Every command and output below was run against the current build. Copy them in
order, or jump to the [cheat sheet](#cheat-sheet).

## 0. Build once

```sh
git clone https://github.com/plainprince/pwb
cd pwb
cargo build --release
export PATH="$PWD/target/release:$PATH"
```

You now have `pwb-node` (the network node), `pwb-web` (the site compiler), and
`pwb-browser` (the window). The rest of this page only needs `pwb-node` and
`pwb-web`.

## 1. Start a node and get your link

```sh
pwb-node
```

First start creates your identity in `~/.pwb/` and prints, among other things:

```text
[Join] no seed list is published at https://raw.githubusercontent.com/plainprince/pwb/main/deploy/join.json yet (HTTP 404)
[Join] No public seed list is published yet. Staying on LAN discovery; dial a peer yourself with --connect <link> or --bootstrap <host:port>/<key>.
...
[Network] No bootstrap peers configured; LAN discovery only.

[Network] Your connection link:
  plain://nodes.plain?connect=192.168.64.19:40877/de7e6336831a25cc82f72c0f4e4393270239c98d4d6a52e576ba9afcc0863fea
```

**That link is your address on the network.** It contains only your IP, port,
and public key — safe to paste in chat or a ticket. Keep the process running;
everything else happens in a second terminal.

The `[Join]` lines are the public seed list. They say nothing is published on
that URL yet (see [the public swarm](#the-public-swarm-today)), so a fresh node
starts on LAN discovery and waits for someone to dial it. When it reaches
nobody, it ends the startup output with the commands that would change that.
Swap the link with a peer and you are on a network.

On the same LAN you can stop here: start a second node and the two find each
other over UDP 7922.

## 2. Connect to another node

Take the link the other node printed and dial it:

```sh
pwb-node --connect "plain://nodes.plain?connect=192.168.64.19:40877/de7e6336…"
```

You are connected when this line appears:

```text
[Network] Connected to peer de7e6336… at 192.168.64.19:40877 (reputation 0)
```

That message is printed only after the other node answers a signed Ping, so it
means the path really works. A reachable address with the wrong public key
prints `No bootstrap peers reachable` instead — the key is part of the address.

The same thing without a link, on the node you are starting:

```sh
pwb-node --bootstrap '203.0.113.9:7921/3f2a…'       # IP and key
pwb-node --bootstrap 'seed.example.com:7921/3f2a…'  # DNS is fine
```

Hosts behind NAT should add `--listen 0.0.0.0:7921` and forward that UDP port
so their link stays dialable; without a forward, two NATed nodes can still meet
through a third node (`--connect` tries direct, then hole punching, then relay).
On an IPv6-capable host, `--listen [::]:7921` binds a dual-stack socket that
serves IPv4 and IPv6 peers at once.

## 3. Serve a site

A site is a folder of `.pml` (markup), `.pss` (styles), and `.pjs` (scripts).
Start with the generated demo:

```sh
pwb-web                     # writes demo_site/ and demo.pwb
pwb-node --host demo.pwb
```

Or point it straight at a folder — it compiles on start:

```sh
pwb-node --host ./mysite
```

Either way:

```text
[Host] Published 'demo.pwb'
       Content ID: 68b14268d12a1ffa6fec1674cbd666f3759d9e6293fcd22d572e9763a7301fa9
       Postings replicated to responsible peers; announced via gossip.plain.
```

That is the deployment. There is no upload step: connected peers fetch the
artifact, index it, and serve it on request. The content ID is the permanent
address of those exact bytes.

## 4. Query the network

From any machine that can reach a node:

```sh
pwb-node --bootstrap '192.168.64.19:40877/de7e6336…' --search "PlainWeb"
```

```text
[Search] Querying the swarm for 'PlainWeb'…
  #1: "Plain Web Demo" | CID: 68b14268… | Rank: 1.50
[Fetch] Received 464 bytes for 68b14268d12a1ffa6fec1674cbd666f3759d9e6293fcd22d572e9763a7301fa9
```

A search process joins, queries, fetches the top result to prove it is
retrievable, and exits. Use `--private-search` to route the query over onion
circuits instead of talking to the node directly.

To look at it in the window instead:

```sh
pwb-browser --bootstrap '192.168.64.19:40877/de7e6336…' "search.plain?q=PlainWeb"
pwb-browser --file demo.pwb          # a bundle already on this machine
```

Quote anything containing `?`, or your shell will eat it. Search results are
prefetched, so clicking one opens it straight away; a `content.pwb/<cid>` URL
only resolves once that bundle is in the local store.

## 5. Claim a name (optional)

`.pwb` names are claimed with earned credits, not bought. Serve and name in one
command:

```sh
pwb-node --host ./mysite --claim mysite.pwb
```

The claim is signed by your identity, gossiped to connected peers, and
resolvable by anyone as `mysite.pwb`. A fresh node starts with 0 credits and
earns them from verified service traffic. Indexing a hosted bundle pays 10
credits once the bundle clears the traffic floor (64 verified bytes per credit,
so 640 bytes — the compressed bundle, not your source folder), which a
medium-sized site does immediately. If the balance is still short, the node
**keeps serving** and retries the same claim every 15 seconds, because answered
queries and relayed cells add verified traffic too; it does not exit and leave
the site unserved. `pwb-node --status` shows the balance.
`.plain` is reserved for built-in pages and cannot be claimed.

## 6. Check that it worked

```sh
pwb-node --status
curl -s http://127.0.0.1:7711/health
curl -s http://127.0.0.1:7711/metrics | grep pwb_peers_connected
```

In the browser, the top bar carries a connection badge — `● 3 peers` in green,
or `● offline` in red — and clicking it opens `network.plain`, which shows this
node's addresses, its live peer table, and what it is serving. The window title
carries the same state, and `nodes.plain` lists peers and dials a link.
Health and metrics bind to loopback by default — keep them there.

## The public swarm (today)

A node started with no `--connect`/`--bootstrap` fetches the project's
published descriptor and dials the seeds it lists, so no hand-copied link is
needed. **That file is not published yet** — the default URL currently returns
404 because `deploy/join.json` has not been pushed, and the node says so and
falls back to LAN discovery. There is no public swarm to join until someone
deploys a seed and commits the descriptor.

When that happens, nothing about your workflow changes:

```sh
pwb-node                     # joins the public seeds automatically
pwb-node --join-required     # ...or exits if no seed answers
pwb-browser welcome.plain    # browser joins the same way
```

If you want to be the one who publishes it, [Running a public seed
node](seed-node.md) covers the host, firewall, and systemd unit, and the
[publish-a-seed runbook](runbooks/publish-a-seed.md) covers signing, verifying,
and rotating the descriptor. Any other operator's descriptor works just as
well:

```sh
pwb-node --join https://example.org/join.json
pwb-node --join ./join.json                  # a file works too
pwb-node --join-signer <hex> --require-join-signature
```

## When it does not work

| Symptom | Cause | Fix |
|---|---|---|
| `no seed list is published … yet` | The public descriptor is not published yet | Use `--connect <link>` or `--bootstrap <host:port>/<key>` |
| `Ignoring traffic from a peer speaking PlainWeb wire v3` | The two nodes were built from different protocol versions | Rebuild both from the same commit |
| `No bootstrap peers reachable` | Wrong port or key, firewall, or the peer is not running | Re-copy the link; allow inbound UDP 7921 on the peer |
| Two LAN nodes do not see each other | UDP 7922 blocked, or one used `--no-join` on a routed network | Allow UDP 7922, or exchange connection links |
| Connected, but search finds nothing | Nobody published matching content; this is not a web crawl | Serve something (`--host`) and query a word from its title or body |
| `mysite.pwb` does not resolve | Name never claimed, or no peer carrying the claim is connected | Claim it, and stay connected to at least one node |
| Browser badge says `● offline`, pages are empty | It is connected to nobody | Share your link, or pass `--bootstrap <host:port>/<key>` |
| Handshake rejected (`ClockSkew`) | Envelopes reject clocks more than 10 minutes fast | Fix the system clock |

## If the build itself fails on macOS

`cargo` can reuse a proc-macro `.dylib` whose ad-hoc signature macOS no longer
accepts — typically after an OS or Xcode update, or after copying `target/`
from another machine. The error mentions `library load disallowed by system
policy` and a `lib*_macros-*.dylib`; the artifact is stale, not the project.

```sh
cargo clean -p phf_macros        # the fast fix for this specific error
cargo run -p pwb-browser
```

If another `*_derive` crate fails the same way, clean everything once
(`cargo clean`, then rebuild). If it comes straight back, check whether
something is modifying the artifacts:

```sh
xattr -l target/debug/deps/libphf_macros-*.dylib   # com.apple.quarantine?
cargo clean && CARGO_TARGET_DIR="$HOME/.cache/pwb-target" cargo run -p pwb-browser
```

Building outside a synced folder (iCloud/Drive/Dropbox) avoids the second
cause. A first build takes a few minutes; later ones are incremental.

## Cheat sheet

```sh
pwb-node                                    # start; prints your connection link
pwb-node --connect <link>                   # dial a specific node
pwb-node --bootstrap <host:port>/<key>      # dial on start (repeatable)
pwb-node --host ./mysite                    # compile and serve a folder
pwb-node --host x.pwb --claim x.pwb         # serve and name it
pwb-node --search "term" --bootstrap <peer> # one query, then exit
pwb-node --status                           # identity, credits, wallet
pwb-browser --bootstrap <peer> x.pwb        # browse a claimed name
pwb-browser --file x.pwb                    # browse a local bundle
pwb-browser network.plain                   # your peers and services
```

All flags: `pwb-node --help`, `pwb-browser --help`.
