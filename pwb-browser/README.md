# pwb-browser — layer 2: runtime & Skia renderer

`pwb-browser` opens a `.pwb` binary bundle, decompiles it back into a Plain DOM AST, applies Plain Stylesheets (`.pss`) rules, executes PlainJS (`.pjs`) script payloads in QuickJS-NG engine with `document` bindings, and renders the webpage directly in a native desktop window.

## Renderer & Typography Features

- **Skia 2D Surface**: Powered by `tiny-skia` for software-rasterized 2D canvas drawing.
- **Crisp TTF Text Rasterization**: Powered by `ab_glyph` to outline and render system TTF fonts (Arial/Helvetica/DejaVu) with Porter-Duff alpha blending.
- **Viewport-Relative Font Scaling**: Base font heights and paddings scale dynamically relative to window height (`viewport_scale = window_height / 600.0`).
- **Dynamic Content Sizing**: Buttons and text views compute bounding boxes from text metrics and padding rather than fixed wide blocks.
- **PlainJS Runtime (`.pjs`)**: QuickJS-NG embedded engine executing `.pjs` scripts with custom `document` DOM manipulation APIs (`getElementById`, `querySelector`, `setText`, `setStyle`).
- **Content-Bound Scrolling**: Scroll position is capped to content height with a visual scrollbar.
- **Text Selection & Clipboard**: Browser-like text selection with `Ctrl+C`/`Cmd+C` clipboard copy via `arboard`.

## JavaScript Web APIs

The page script (and devtools) get browser-style globals backed by Rust:

- `fetch(url, {method, headers, body})` — async HTTP on a background thread; returns a `Promise` with a Response-like object (`ok`, `status`, `statusText`, `headers`, `text()`, `json()`, `arrayBuffer()`). By default **CORS is not enforced** so local bundles can call any API; build with `--features strict-cors` to require `Access-Control-Allow-Origin: *` on cross-origin responses.
- `setTimeout` / `setInterval` / `clearTimeout` / `clearInterval` — driven by the winit event loop (`about_to_wait`), so they fire even while the app is idle.
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

Detailed design decisions and architecture documentation are in the [`docs/`](docs/) folder:

- [Architecture Overview](docs/architecture.md) — High-level design and components
- [URI Resolution & `file.pwb`](docs/uri-resolution.md) — How local file access and domain resolution work
- [Scrolling & Viewport](docs/scrolling-viewport.md) — Content-bound scrolling and the visual scrollbar
- [Text Selection & Clipboard](docs/text-selection.md) — Browser-like text selection
- [Building & Running](docs/development.md) — Setup and usage instructions
