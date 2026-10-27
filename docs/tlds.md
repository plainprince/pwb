# PlainWeb TLDs

PlainWeb has two namespaces. The list below is complete: anything not on it is
not part of PlainWeb's namespace and cannot be claimed.

## Official namespace — `.plain` (reserved)

`.plain` is **strictly reserved**. It holds only:

- the system protocols: `search.plain`, `register.plain`, `gossip.plain`,
  `nodes.plain`, `network.plain`, `help.plain`, `chat.plain`,
  `devtools.plain`
- the browser-shipped content pages: `welcome.plain`, `demo.plain`

No user, miner, or node can claim, register, or overwrite a `.plain` name —
every claim path (plain, attested, blind, and gossip relays) rejects it.

## Unofficial namespace — claimable TLDs

Every TLD below is claimable on `register.plain` with earned credits, resolves
through the registry, and is recognized by the browser as an address.

| TLD | Flavour | Example |
|-----|---------|---------|
| `.pwb` | The original PlainWeb namespace — the default for new sites | `mysite.pwb` |
| `.non` | Generic "unofficial" names | `blog.non` |
| `.yep` | Playful, personal, micro-pages | `hello.yep` |
| `.lam` | Short, punchy one-liner sites | `toot.lam` |
| `.arch` | Archives and long-lived snapshots | `1994.arch` |
| `.vio` | Vivid, visual and expressive pages | `canvas.vio` |
| `.org` | Communities, collectives, projects | `commons.org` |
| `.net` | Networks and services | `relay.net` |
| `.com` | Commercial and project homepages | `shop.com` |

The machine-readable source of truth is `UNOFFICIAL_TLDS` in
[`pwb-core/src/system.rs`](../pwb-core/src/system.rs). The browser's address
recognition, every claim path's validation, and the error text shown for a
refused name all derive from that one list — this document is its human-readable
mirror.

### What "claimable" means

- **Claim**: `pwb-node --host ./site --claim mysite.non` (or the
  `register.plain` page). Claims are signed, debited from your credit ledger,
  and gossiped across the swarm.
- **Resolve**: any peer resolves `mysite.non` to the hosted bundle's content
  id through the registry (locally cached, or on demand from the swarm).
- **Recognize**: the browser treats `name.<listed-tld>` typed into the address
  bar as an address, not a search.

### Rules

1. **Case-insensitive.** `MYsite.PWB` and `mysite.pwb` are the same name; the
   registry lowercases before compare and claim.
2. **A label must precede the dot.** `.pwb` alone is not a name;
   `site.pwb` is.
3. **Nested labels are allowed.** `a.b.com` is a valid claim (matching the
   original `.pwb` behaviour).
4. **Unknown TLDs are refused.** `mysite.xyz` or a bare `mysite` fails every
   claim path with `unknown TLD: names must end in one of …`, and the same
   check runs on gossiped claims, so all participants converge on this list.
5. **These TLDs are PlainWeb-internal.** `shop.com` inside PlainWeb is *not*
   the real internet's `shop.com`: names resolve through the PlainWeb
   registry over the peer-to-peer network, never through DNS.

### Internal pseudo-addresses

Two dotted prefixes are reserved by the browser for its own use and are not
registry names you should claim:

- `file.pwb/<path>` — read a local bundle from disk
- `content.pwb/<hex-cid>` — address a bundle directly by its BLAKE3 hash,
  with no registry lookup

The bare `devtools` domain is likewise an internal browser tool with no TLD.

## Adding a TLD

1. Append it to `UNOFFICIAL_TLDS` in `pwb-core/src/system.rs` — **that one line
   is the whole system**. Validation, the shared error text, the
   `register.plain` claim hint, the `help.plain` namespace row, the browser's
   address recognition, and the `pwb-browser --help` summary all derive from
   the list (tests assert each rendered page mentions every entry).
2. Add a row to the table above, and mirror it in the prose enumerations in
   `README.md`, `docs/protocol.md`, and the crate READMEs — prose can't be
   derived from code.

Because validation happens on every claim path *and* on gossiped relay, a new
TLD only takes effect once nodes run the updated binary — older nodes refuse
claims carrying a TLD they do not know, which is the safe direction (no name
can resolve differently on two nodes).
