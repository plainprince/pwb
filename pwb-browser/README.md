# pwb-browser — layer 2: runtime & tiny_skia renderer

`pwb-browser` opens a `.pwb` binary bundle, decompiles it back into a Plain DOM AST, applies Plain Stylesheets (`.pss`) rules, executes PlainJS (`.pjs`) script payloads in the QuickJS-NG engine with `document` bindings, and renders the page directly in a native desktop window.

## Renderer & typography features

- **tiny_skia 2D surface**: Software-rasterized 2D canvas drawing powered by `tiny-skia` (not GPU-accelerated Skia).
- **Crisp TTF text rasterization**: Powered by `ab_glyph` to outline and render system TTF fonts (Arial/Helvetica/DejaVu) with Porter-Duff alpha blending.
- **Viewport-relative font scaling**: Base font heights and paddings scale dynamically relative to window height (`viewport_scale = window_height / 600.0`).
- **Dynamic content sizing**: Buttons and text views compute bounding boxes from text metrics and padding rather than fixed wide blocks.
- **PlainJS runtime (`.pjs`)**: QuickJS-NG embedded engine executing `.pjs` scripts with custom `document` DOM manipulation APIs.
- **Content-bound scrolling**: Scroll position capped to content height with a visual scrollbar.
- **Text selection & clipboard**: Text selection with `Ctrl+C`/`Cmd+C` copy, `Ctrl+X`/`Cmd+X` cut, and `Ctrl+V`/`Cmd+V` paste into focused inputs via one long-lived `arboard` handle (so X11/Wayland clipboard managers keep the contents).
- **Context menu**: Right-click selects "Copy" or "Inspect Element"; native AppKit menu on macOS, an in-window menu on Linux/Windows with a live hover highlight (click outside or press Escape to dismiss).
- **PNG images**: `<image>`/`<img>` decodes PNGs (file paths, `file://`, or `data:image/png;base64,…`), caches them, and scales them to the layout box; unreadable sources show a placeholder.
- **Native links**: Clicking `<link href="…">`/`<a href="…">` (or text inside it) follows the target through the normal navigation path, with no JavaScript required.
- **Verified authorship & page isolation**: signed artifacts are verified on decode (a tampered or missing author signature is rejected with a readable error page), and every top-level navigation gets a fresh QuickJS context and DOM/async state so one page cannot leave globals or handlers behind. Unsigned bundles remain supported.

## Native system domains

| System domain | Protocol service role |
|---------------|----------------------|
| **`search.plain`** | Decentralized search engine API & dynamic interface handler (`plain://search?q=query`). Nodes participate in indexing and ranking. |
| **`devtools.plain`** | Built-in Developer Tools DOM & Style Inspector (`F12` or `Cmd/Ctrl + Shift/Opt + I/J`). |
| **`register.plain`** | Domain claim console (`plain://register.plain`). Claim a `.pwb` name for a content ID: the claim is signed by your identity, gossiped to peers, persisted locally, and resolvable across the swarm. |
| **`gossip.plain`** | P2P gossip announcements for artifact propagation across peers. |
| **`nodes.plain`** | Decentralized peer routing table and health discovery. |
| **`network.plain`** | Live node status: listen/discovery/STUN addresses, peer table, and local index/store/registry counts. |
| **`help.plain`** | Help index linking every system domain, plus namespace, hosting, and credit rules. |

## Official (`.plain`) vs unofficial (TLD list) sites

`.plain` is the **official** namespace. It is strictly reserved for the browser's built-in pages: the system protocols (`search.plain`, `devtools.plain`, `register.plain`, `gossip.plain`, `nodes.plain`, `network.plain`, `help.plain`) and the shipped content pages (`welcome.plain`, `demo.plain`). No user, miner, or node can claim or register a `.plain` domain — registration attempts are rejected by `pwb-node::domain`.

The **unofficial** namespace is the claimable TLD list `.pwb`, `.non`, `.yep`, `.lam`, `.arch`, `.vio`, `.meh`, `.son`, `.isuearchbtw`, `.org`, `.net`, `.com`: any site created and claimed by network participants (e.g. `mysite.pwb`, `club.non`). Names outside the list are refused. See [docs/tlds.md](../docs/tlds.md).

## PlainJS (`.pjs`) `document` API

PlainWeb embeds the QuickJS-NG JavaScript engine (`rquickjs`). PlainJS scripts access a lean `document` object:

- `document.selector("query")` — query DOM element by `.class`, `#id`, or tag name.
- `document.getElementById("id")` — shorthand for `selector("#" + id)`.
- `element.text` — the element's text content.
- `element.getText()` / `element.setText(t)` — get or set element text.
- `element.html` — the element's markup.
- `element.getHtml()` / `element.setHtml(h)` — get or set element markup.
- `element.setStyle(prop, val)` — set a CSS/PSS property dynamically.
- `element.onClick("js_code")` — register a click handler.
- `element.addEventHandler("event", "js_code")` — register an event handler for any event.
- `console.log(...)` — debug output.

Event handlers run with `this` bound to the element, so `this.setText("Clicked!")` works inside a handler:

```js
var btn = document.getElementById("main-btn");
btn.onClick("this.setText('Clicked!');");

var btn2 = document.getElementById("ctx-btn");
btn2.addEventHandler("contextmenu", function(evt) {
    evt.preventDefault();
    this.setText("menu suppressed");
});
```

## Script globals

The page script (and devtools) get these globals, backed by Rust:

- `fetch(url, {method, headers, body})` — async HTTP on a background thread; returns a Promise with `ok`, `status`, `statusText`, `headers`, `text()`, `json()`, `arrayBuffer()`. CORS is enforced by default: cross-origin http(s) responses must send `Access-Control-Allow-Origin: *` (same-origin file/relative requests are always allowed). Build with `--features permissive-cors` to opt out for local development.
- `setTimeout` / `setInterval` / `clearTimeout` / `clearInterval` — driven by the winit event loop.
- `localStorage` / `sessionStorage` — `getItem`/`setItem`/`removeItem`/`clear`/`key`/`length`. `localStorage` persists per bundle to `~/.pwb/<bundle-hash>.json` (override with `PWB_STORAGE_DIR`).
- `alert` / `confirm` / `prompt` — native dialogs on macOS (NSAlert), Linux (zenity/kdialog/yad via `tinyfiledialogs`), and Windows (task dialogs), with console fallbacks when no GUI backend is available (`PWB_HEADLESS` forces the fallback). Right-click menus are native on macOS and painted in-window on other platforms.
- `crypto.getRandomValues()` / `crypto.randomUUID()` and `btoa` / `atob` (Latin-1 binary strings).
- `performance.now()` — monotonic milliseconds.
- DOM building: `document.createElement(tag)`, `el.appendChild` / `removeChild` / `remove`, `el.setAttribute` / `getAttribute` / `removeAttribute`, plus `document.body`.
- `WebSocket(url)` — full-duplex over a background thread; `onopen` / `onmessage` / `onclose` / `onerror`, `send()`, `close()`, `readyState`.

## Usage

```sh
# Open a local .pwb file
cargo run -p pwb-browser -- --file path/to/bundle.pwb
cargo run -p pwb-browser -- -f path/to/bundle.pwb

# Open via file.pwb pseudo-domain
cargo run -p pwb-browser -- file.pwb/path/to/bundle.pwb

# Open the official built-in welcome page
cargo run -p pwb-browser -- welcome.plain

# Join a swarm through a bootstrap peer (or set PWB_BOOTSTRAP)
cargo run -p pwb-browser -- --bootstrap '<host:port>/<hex_pubkey>' mysite.pwb

# Fetch a published join descriptor instead of a hand-copied bootstrap link
# (the default descriptor is used when no --bootstrap/--connect is given;
# --no-join disables it)
cargo run -p pwb-browser -- --join https://example.org/join.json mysite.pwb

# Pin funding attestors instead of trusting the live peer neighborhood
cargo run -p pwb-browser -- --trust-anchor <hex_pubkey> mysite.pwb

# Search, content fetch, and .pwb resolution use onion circuits by default;
# --direct switches to the direct transport and reveals your address.
cargo run -p pwb-browser -- --direct mysite.pwb

# Emit decoy onion cells at randomized intervals (opt-in; costs bandwidth)
cargo run -p pwb-browser -- --cover-traffic welcome.plain

# Open an unofficial registered network domain
cargo run -p pwb-browser -- mysite.pwb

# Open a system domain
cargo run -p pwb-browser -- search.plain?q=plainweb

# Show help
cargo run -p pwb-browser -- --help
```

## Documentation

Detailed design decisions and architecture documentation live in the [`docs/`](../docs/) folder. Browse everything from the [docs index](../docs/README.md):

- [Protocol Reference](../docs/protocol.md) — pinned wire/onion/discovery constants and the `.plain` security rule.
- [§1 Core Architecture](../docs/design/01-core-architecture.md) — the shared `pwb-core::net::NodeEngine` both this browser and `pwb-node` instantiate.
- [§2 Onion Routing & Encryption](../docs/design/02-onion-routing.md) — fixed-size cells, path rules, and cover traffic.
- [§3 Decentralized Node Discovery](../docs/design/03-node-discovery.md) — beacons, `plain://nodes.plain?connect=...` links, and QR pairing.

## Browser shell (2026-10-22)

Every page is rendered under a fixed browser chrome:

- **Tabs** — click a tab to switch, `+` opens a new one. `Ctrl/Cmd+T` opens a
  tab; `Ctrl/Cmd+W` closes the active tab (the last tab becomes
  `welcome.plain`).
- **Address bar** — `Ctrl/Cmd+L` focuses it, Enter navigates, and a bare word
  becomes `search.plain?q=…`. The current URL is always visible.
- **History** — `Alt+Left` / `Alt+Right` and the `Back` / `Fwd` buttons walk
  each tab's history. `history.plain` lists the session.
- **Reload** — `Ctrl/Cmd+R` or the `Reload` button.
- **Bookmarks** — `Ctrl/Cmd+D` or the `Save` button toggles the current page;
  `bookmarks.plain` lists saved pages as native links.
- **Connection badge** — `● N peers` (green) or `● offline` (red) mirrors the
  live peer table; clicking it opens `network.plain`.

Tabs, per-tab history, and bookmarks persist across restarts in
`$PWB_DATA/session.json` (default `$HOME/.pwb/session.json`). Writes use a
temp file + rename so a crash cannot truncate the session.

Artifacts, the search index, `.pwb` claims, and the credit ledger live under
`$PWB_DATA` (default `$HOME/.pwb/browser`); per-bundle `localStorage` follows
`PWB_STORAGE_DIR` (default `$HOME/.pwb`). The data directory is deliberately
separate from `pwb-node`'s `$HOME/.pwb/data`: the browser embeds a full node,
and two live processes must not append to one ledger or flush one index. Set
`PWB_DATA` to the same directory only when you mean to share a profile. The chrome is a
fixed 92px DOM bar; layout reserves its height above the page (and keeps it
pinned while scrolling), so it works on every page without JavaScript
privileges and never covers the first lines of content.
