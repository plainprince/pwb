# pwb — PlainWeb

PlainWeb is a **serverless**, onion-inspired network whose explicit goal is to
stay *untrackable by default* while offering **optional identifiability** and a
**native, decentralized search** layer. No central servers, no pools, no
duplicate work: indexing, ranking, verification, and domain allocation are all
done *by the network itself*, sharded across nodes.

## Native System Domains Specification

PlainWeb replaces legacy web URLs and central search engines with **Native System Protocols & System Domains**:

| System Domain | Protocol Service Role |
|---------------|----------------------|
| **`search.plain`** | Decentralized search engine API & dynamic interface handler (`plain://search?q=query`). Every node participates in indexing and ranking. |
| **`devtools.plain`** | Built-in Developer Tools DOM & Style Inspector (`F12` or `Cmd/Ctrl + Shift/Opt + I/J`). |
| **`register.plain`** | Sparse credit domain registry dashboard (`plain://register.plain`). Claims domain names mapped to Content IDs. |
| **`gossip.plain`** | P2P gossip announcements for artifact propagation across peers. |
| **`nodes.plain`** | Decentralized peer routing table and health discovery. |

## Reserved System TLD Security Rule (`.plain`)

PlainWeb strictly reserves the **`.plain`** Top-Level Domain (TLD) for **built-in system protocols**. No user, miner, or node in the network can claim, register, or overwrite `.plain` domains:

| TLD / Domain | Security & Protocol Role |
|--------------|--------------------------|
| **`*.plain`** | **Strictly Reserved System TLD**. Only handles built-in system endpoints (`search.plain`, `devtools.plain`, `register.plain`, `gossip.plain`, `nodes.plain`). Registration attempts by network participants are rejected by `pwb-node::domain`. |
| **`*.pwb`**   | **User Network Domains**. Claimed by network participants using sparse credits (e.g. `welcome.pwb`, `mysite.pwb`). |

## PlainWeb File Extensions

PlainWeb sites are authored using three clean, standard files in a site folder:
- **`.pml`** — **Plain Markup Language** (custom PML tag & element structure).
- **`.pss`** — **Plain Stylesheets** (custom PSS styling rules, positioning, and cursor maps).
- **`.pjs`** — **PlainJS (Plain JavaScript)** (scripting runtime payload, executed in QuickJS-NG engine with custom `document` & `EventHandler` DOM bindings).

Layer 1 (`pwb-web`) compiles a site folder containing `.pml`, `.pss`, and `.pjs` into a single, compact, losslessly compressed **`.pwb`** binary bundle.

## Interactive Browser Architecture (`pwb-browser`)

- **Built-in DevTools Shortcut (`F12` / `Cmd+Shift+I`)**: Pressing `F12` or `Cmd/Ctrl + Shift/Opt + I/J` dynamically opens `devtools.plain`, rendering live DOM inspection, AST attributes, and computed styles!
- **Unified Layout Tree Engine**: Computes exact pixel-accurate bounding boxes (`LayoutBox`) for every DOM element, eliminating layout divergence between rendering, hit-testing, and event handling.
- **`EventHandler` Interactive System**: PlainJS scripts register element click handlers via `element.onClick("js_code")` or `element.addEventHandler("click", "js_code")`, dispatched on mouse click!
- **Container-Aware Text Selection**: Mouse drag text selection highlighting across elements (`<text>`, `<button>`, `<input>`) with active visual selection highlights.
- **Pixel-Exact Cursor Hovering**: Queries the Layout Tree to accurately update OS window cursors (`Pointer` for buttons/links, `Text` for text elements, custom PSS `cursor: pointer | text | grab | crosshair`).
- **Mouse Wheel Scrolling**: Smooth vertical viewport canvas scrolling (`scroll_y`).
- **Crisp Glyph Rasterization**: Layer 2 (`pwb-browser`) rasterizes TTF font outlines (`ab_glyph` engine) directly onto the Skia canvas surface with Porter-Duff alpha blending.

## Custom PlainJS (`.pjs`) `document` & `EventHandler` API

PlainWeb browser embeds the **QuickJS-NG** JavaScript engine (`rquickjs`).
PlainJS (`.pjs`) scripts have access to a modern, lean `document` object designed specifically for PlainWeb:
- `document.selector("query")` — query DOM element by `.class`, `#id`, or tag name.
- `document.getElementById("id")` — query DOM element by ID.
- `element.text` / `element.setText("new text")` / `element.getText()` — get or set element text (replaces legacy `innerText`).
- `element.html` / `element.setHtml("new html")` / `element.getHtml()` — get or set element markup (replaces legacy `innerHTML`).
- `element.setStyle(prop, val)` — set CSS/PSS property dynamically.
- `element.onClick("js_code")` / `element.addEventHandler("event", "js_code")` — register interactive event handlers.
- `console.log(...)` — debug output.

## The three layers (+ a shared core)

| Crate | Layer | Role |
|-------|-------|------|
| `pwb-core` | — | Shared primitives: PML tag spec, `.pwb` wire format, crypto identity, and transport framing. |
| `pwb-web` | 1 — Authoring | Ingest site folders (`.pml`, `.pss`, `.pjs`), parse AST and style rules, and bundle into `.pwb`. |
| `pwb-browser` | 2 — Runtime | Open a `.pwb` bundle, decompile it, build DOM, compute Layout Tree, execute PlainJS script with `EventHandler` bindings, and render directly in a **full-window Skia window**. |
| `pwb-node` | 3 — Miner | Serverless node: host `.pwb` sites, run decentralized search, verify indexing proofs, hold credit keypairs, and claim domains. |

## Quick Start

```sh
# 1. Compile a site folder (.pml, .pss, .pjs) into demo.pwb
cargo run -p pwb-web -- demo_site demo.pwb

# 2. Render native system search in browser with text selection & scrolling
cargo run -p pwb-browser -- "search.plain?q=untrackable"

# 3. Render a registered user network domain (.pwb)
cargo run -p pwb-browser -- welcome.pwb

# 4. Launch a serverless node (host, search index, verifier, domain registry)
cargo run -p pwb-node
```
