# Design Decisions: URI Resolution & the `file.pwb` Pseudo-Domain

## Problem

PlainWeb uses `pwb://` for all network navigation. Every domain ends in `.pwb`
(unofficial user content) or `.plain` (official built-in pages). This means
passing a bare filename like `demo.pwb` on the command line collides with the
domain resolution path — the resolver tries to look it up in the
`DomainRegistry` instead of reading it from disk.

## Solution

### The `file.pwb` Pseudo-Domain

`file.pwb` is a **reserved, unhostable pseudo-domain**. It cannot be registered
on the PlainWeb network because the resolver intercepts it before reaching the
`DomainRegistry`. The path segment after `file.pwb/` is treated as a local
filesystem path:

```
file.pwb/path/to/bundle.pwb   →   reads ./path/to/bundle.pwb from disk
file.pwb//tmp/my.pwb           →   reads /tmp/my.pwb (absolute path)
```

### Resolution Priority

The resolver evaluates URIs in this strict order:

| Priority | Match Condition | Handler |
|----------|----------------|---------|
| 1 | `*.plain` TLD | Official built-in page — system handler (search, register, devtools, …) or shipped content page (welcome, demo) |
| 2 | `file.pwb/` prefix | Local filesystem read |
| 3 | `content.pwb/<hex-cid>` | Content-addressed lookup in the local artifact host (used by `search.plain` result links) |
| 4 | `*.pwb` TLD or contains `.` | Network domain lookup via `DomainRegistry` |
| 5 | Fallback | Treat raw argument as local file path |

Placing `file.pwb/` **before** network domain lookup (priority 2 vs 4) is
intentional: it guarantees that `file.pwb/…` can never be hijacked by a
registered domain, and that the filesystem path is resolved without touching
the network layer.

### The `--file` / `-f` Flag

A convenience flag rewrites the target automatically:

```sh
pwb-browser --file demo.pwb
# internally becomes → file.pwb/demo.pwb
```

This keeps the URI model consistent while giving developers a natural CLI
experience.

### Why Not Just Read Any `.pwb` Argument as a File?

Because `mysite.pwb` is a **valid (unofficial) network domain**. If we silently fell back
to disk for every `.pwb` argument, a local file could shadow a network domain
(or vice-versa), creating confusing behaviour and a potential security issue.
The `file.pwb/` prefix makes the intent **explicit**: the user is requesting
local filesystem access, not network resolution.

### Fallback Path

If the target matches none of the above categories (no `.plain`, no `file.pwb/`,
no `.pwb` TLD, no dot at all), the resolver falls back to `fs::read(target)`.
This handles raw filenames like `demo.pwb` that are passed without any prefix,
preserving backwards compatibility.

## Security Considerations

- `file.pwb` **cannot be registered** on the PlainWeb network — the resolver
  intercepts it before `DomainRegistry` is consulted.
- `file.pwb` **cannot be hosted** — there is no content behind it on the
  network; it is purely a local pseudo-domain.
- File access is **read-only** — no write operations are supported.
- File access is **explicit** — requires the `file.pwb/` prefix or `--file`
  flag; no accidental filesystem reads from network URIs.
