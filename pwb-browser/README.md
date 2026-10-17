# pwb-browser — layer 2: runtime & tiny_skia renderer

`pwb-browser` opens a `.pwb` binary bundle, decompiles it back into a Plain DOM AST, applies Plain Stylesheets (`.pss`) rules, executes PlainJS (`.pjs`) script payloads in the QuickJS-NG engine with `document` bindings, and renders the page directly in a native desktop window.

## Renderer & typography features

- **tiny_skia 2D surface**: Software-rasterized 2D canvas drawing powered by `tiny-skia` (not GPU-accelerated Skia).
- **Crisp TTF text rasterization**: Powered by `ab_glyph` to outline and render system TTF fonts (Arial/Helvetica/DejaVu) with Porter-Duff alpha blending.
- **Viewport-relative font scaling**: Base font heights and paddings scale dynamically relative to window height (`viewport_scale = window_height / 600.0`).
- **Dynamic content sizing**: Buttons and text views compute bounding boxes from text metrics and padding rather than fixed wide blocks.
- **PlainJS runtime (`.pjs`)**: QuickJS-NG embedded engine executing `.pjs` scripts with custom `document` DOM manipulation APIs.
- **Content-bound scrolling**: Scroll position capped to content height with a visual scrollbar.
- **Text selection & clipboard**: Text selection with `Ctrl+C`/`Cmd+C` clipboard copy via `arboard`.

## Native system domains

| System domain | Protocol service role |
|---------------|----------------------|
| **`search.plain`** | Decentralized search engine API & dynamic interface handler (`plain://search?q=query`). Nodes participate in indexing and ranking. |
| **`devtools.plain`** | Built-in Developer Tools DOM & Style Inspector (`F12` or `Cmd/Ctrl + Shift/Opt + I/J`). |
| **`register.plain`** | Sparse credit domain registry dashboard (`plain://register.plain`). Claims domain names mapped to Content IDs. |
| **`gossip.plain`** | P2P gossip announcements for artifact propagation across peers. |
| **`nodes.plain`** | Decentralized peer routing table and health discovery. |

## Reserved system TLD (`.plain`)

The `.plain` TLD is strictly reserved for built-in system protocols. No user, miner, or node can claim or register `.plain` domains — registration attempts are rejected by `pwb-node::domain`. User network sites live on `.pwb` domains (e.g. `welcome.pwb`, `mysite.pwb`).

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

- `fetch(url, {method, headers, body})` — async HTTP on a background thread; returns a Promise with `ok`, `status`, `statusText`, `headers`, `text()`, `json()`, `arrayBuffer()`. CORS is not enforced by default; build with `--features strict-cors` to require `Access-Control-Allow-Origin: *` on cross-origin responses.
- `setTimeout` / `setInterval` / `clearTimeout` / `clearInterval` — driven by the winit event loop.
- `localStorage` / `sessionStorage` — `getItem`/`setItem`/`removeItem`/`clear`/`key`/`length`. `localStorage` persists per bundle to `~/.pwb/<bundle-hash>.json` (override with `PWB_STORAGE_DIR`).
- `alert` / `confirm` / `prompt` — native macOS dialogs (NSAlert); console fallbacks on other platforms.
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

# Open a registered network domain
cargo run -p pwb-browser -- welcome.pwb

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
