# PML, PSS, and PlainJS reference

The three authoring formats `pwb-web` compiles. For a walkthrough, start with the
[quickstart](quickstart.md).

- **PML** — Plain Markup Language: the document tree, in `.pml`.
- **PSS** — Plain Stylesheets: selectors and properties in `.pss` (or `.css`).
- **PlainJS** — the page script in `.pjs` (or `.js`), executed by `pwb-browser`.

Everything is compiled before it ships. There is no HTML parser, no CSS engine,
and no module loader at runtime; what you write is resolved into a flat tree of
nodes and attributes inside one `.pwb` bundle.

## PML

### Document shape

A `.pml` file is a tree of elements. `<plain>` is the conventional root, but any
root element is accepted. `head`, `title`, and body content can appear in any
order.

```html
<plain>
    <head>
        <title>Page title</title>
    </head>
    <view class="page">
        <text>Hello</text>
    </view>
</plain>
```

The `<title>` text becomes the window and tab title. A `title` attribute is the
fallback when no `<title>` element exists. If neither is present the title is
`PlainWeb Site`.

### Supported tags

| Written as | Compiles to | Notes |
|---|---|---|
| `<plain>`, `<document>` | Document | Root container. |
| `<head>` | Head | Structural only. |
| `<title>` | Title | Supplies the page title. |
| `<view>`, `<html>`, `<body>`, `<div>`, `<section>`, `<main>`, `<header>`, `<footer>`, `<nav>`, `<article>`, `<aside>` | View | Flex container by default. Text directly inside becomes a `<text>` child. |
| `<br>`, `<hr>` | View | Void elements; render as an empty container. |
| `<text>`, `<span>`, `<p>`, `<label>`, `<h1>`…`<h6>` | Text | Text content is stored on the element and painted. |
| `<image>`, `<img>` | Image | `src` is a PNG path, `file://` URL, or `data:image/png;base64,…`. |
| `<button>` | Button | Clickable; use `id` plus a script handler. |
| `<input>` | Input | Supports `placeholder` and `value`. |
| `<link>`, `<a>` | Link | `href` navigates natively when clicked. `<link rel="…">` is treated as metadata and skipped instead. |
| `<icon>`, `<icon-name>` | Icon | Void. `<icon-home>` sets `icon="home"`; alternatively `<icon name="home" />`. Names come from the built-in feather icon set. |
| `<code>` | Code | Block with the theme's code colors. |
| `<script>` | — | Extracted into the bundle's script payload; removed from the page. |
| `<component>` | — | Defines a reusable fragment; removed from the page. |
| `<import>` | — | Expanded to a component body at compile time. |
| `<meta>`, `<link rel="…">` | — | Skipped entirely (metadata, not DOM). |

Any other tag name is a compile error (`InvalidTag`). There are no custom
elements at runtime — use components for reuse.

### Attributes

Attributes are free-form key/value pairs. These keys have behavior in the
renderer:

| Attribute | Applies to | Meaning |
|---|---|---|
| `id` | any | Selector target; `#id` styles; `document.getElementById`. Must be unique for JS lookups. |
| `class` | any | One or more space-separated class names; `.class` styles. |
| `href` | `<link>`, `<a>` | Target URL; navigation is native, no JS needed. |
| `src` | `<image>`, `<img>` | Image source. |
| `placeholder` | `<input>` | Greyed hint text. |
| `value` | `<input>` | Initial value. |
| `icon` | `<icon>` | Feather icon name. |
| `title` | any | Page title fallback when there is no `<title>`. |
| `style` | any | Inline declarations, same syntax as a CSS declaration list. |

Every other attribute is still carried into the bundle. Style-like attributes
are applied by the renderer (see [PSS properties](#properties-the-renderer-uses)),
and `data-*` attributes are readable from scripts through
`getAttribute`.

Values may be double-quoted, single-quoted, or bare:

```html
<view class=page>
<view class='page'>
<view class="page">
```

A bare key with no value becomes `"true"`. That makes `<button disabled>` a
real attribute, not a parse error.

### Text, whitespace, and escaping

- Text inside a container (`<view>`, `<div>`, `<body>`, `<plain>`) is trimmed and
  inserted as a `<text>` child, so it is laid out and painted.
- Text inside a leaf (`<text>`, `<button>`, `<title>`) is stored as the element's
  content. Interleaving text and elements inside a leaf is not supported; wrap
  them in a container instead.
- Whitespace-only text between elements is dropped.
- `<script>` content is raw: everything up to `</script>` is kept verbatim, so
  `<`, `>`, and string literals are safe inside a script.
- There is no entity layer. `&amp;` is the literal characters `&amp;`; write
  `&` directly.
- Comments (`<!-- … -->`) and doctypes are skipped as tags, but keep comments
  simple: the parser stops at the first `>` it sees, and a nested `>` inside a
  comment can end it early.

### Void and self-closing tags

`<image>`, `<img>`, `<input>`, `<br>`, `<hr>`, and all icons are void: they never
contain children and need no closing tag. Any element may also be self-closed:

```html
<input id="name" placeholder="Name" />
<icon-home />
<view class="spacer" />
```

### Components

Define a component once with `<component name="…">`. The definition is removed
from the page; the body is registered for reuse.

```html
<component name="card">
    <view class="card">
        <text class="card-title">{title}</text>
        <text>{body}</text>
    </view>
</component>
```

Use it with `<import>` and fill `{placeholders}` from import attributes:

```html
<import component="card" title="Hello" body="World" />
```

Rules:

- A component body with exactly one child uses that child. A body with several
  children is wrapped in a `<view>`.
- Placeholders are replaced in both attributes and text. Unknown placeholders
  are left as written.
- A component may load its body from a file:
  `<component name="card" src="card.pml" />`. If that file contains a
  `<component>`, its children are the body; otherwise the file's children are.
- Imports can nest, up to 8 levels. Deeper nesting stops expanding and prints a
  warning.
- An unknown component name prints a warning and the `<import>` is dropped. A
  missing `src` file prints a warning and the component is registered with no
  body, so its imports expand to nothing. Compilation still succeeds.

### Scripts in markup

Both forms are extracted into the bundle's single script payload:

```html
<script src="app.pjs"></script>
<script>console.log("inline");</script>
```

They are removed from the page tree. Prefer the `app.pjs` entry file for the
main script; inline `<script>` is useful for a small page-local snippet.

## PSS

PSS is a deliberately small subset of CSS. It is evaluated while compiling: the
computed values are written into the bundle as attributes, and the stylesheet
itself is not shipped.

### Selectors

| Selector | Example | Matches |
|---|---|---|
| Tag | `button { … }` | Every `<button>` |
| Class | `.card { … }` | Elements with `class="card"` |
| Compound class | `.card.active { … }` | Elements with both classes |
| Id | `#submit { … }` | The element with `id="submit"` |
| Compound id | `#submit.primary { … }` | An id plus class |

There are no descendant combinators (`view text`), child combinators (`>`),
pseudo-classes (`:hover`), pseudo-elements, attribute selectors, or media
queries. Selector text is lowercased at parse time while `class`/`id` values
keep the case you wrote, so keep selector, class, and id names lowercase.

### Cascade and inheritance

- Specificity: id = 100, each class = 10, tag = 1. Higher wins; equal
  specificity falls back to source order, later rules win.
- A repeated selector extends the earlier rule instead of replacing it.
- `font-size` and `color` inherit to descendants. Everything else applies only
  to the matched element.- Rules are flattened into attributes after the markup is parsed, so a
  stylesheet declaration **replaces** a same-named markup attribute (for
  example `color` on a node). Don't set the same property in both places and
  expect the markup to win.
- Inline `style="…"` is applied by the renderer before individual attributes,
  so any attribute-level value (including one compiled from PSS) overrides the
  inline declaration for that property.
- Rules are applied in source order across files: the `.pss` files are
  concatenated alphabetically, then the bundled `.css`, so a later file
  overrides an earlier one at equal specificity.

### Values

- **Colors**: `#rrggbb`, `white`, `black`. In `.pss`/`.css`, `#abc` shorthand and
  other color names are ignored (the compiler's parser accepts only those).
  In markup attributes and inline styles the renderer also accepts `#rgb`,
  `transparent`, `red`, `green`, `lime`, `blue`, `gray`/`grey`, `yellow`,
  `orange`, and `purple`.
- **Lengths**: `24px`, a bare number (`24`, treated as px), `50%`, `10vw`,
  `10vh`, and `auto` where the property allows it.
- **Box properties take one value.** `padding: 8px 12px` and `padding: 8px 12px
  16px 20px` do not parse; use a single value or a container's `gap`.

### Properties the renderer uses

| Property | Values | Notes |
|---|---|---|
| `color` | color | Text color. Inherits. |
| `background-color`, `background` | color | `background` is an alias. |
| `font-size` | length | Inherits. |
| `padding` | length | One value, all sides. |
| `gap` | length | Space between flex children. |
| `width`, `height` | length or `auto` | |
| `min-width`, `max-width` | length | Runtime-only; forwarded as an attribute. |
| `border-radius` | px number | |
| `border`, `border-width`, `border-color`, `border-top`, `border-right`, `border-bottom`, `border-left` | `1px solid #3e3e42`, or width/color alone | |
| `display` | `flex`, `block`, `none` | `view`/`div` default to flex, `text`/`image` to block. |
| `flex-direction` | `row`, `column` | |
| `justify-content` | `center`, `flex-start`/`start`, `flex-end`/`end`, `space-between` | |
| `align-items` | same set | |
| `position` | `static`, `relative`, `absolute`, `fixed`, `sticky` | |
| `top`, `left`, `right`, `bottom` | px number | Percentages are accepted in markup/inline attributes at runtime, not in `.pss`. |
| `overflow` | `visible`, `hidden`, `scroll`, `auto` | |
| `resize` | `none`, `both`, `vertical`, `horizontal` | |
| `cursor` | `pointer`, `text`, `grab`, `crosshair`, `default` | `not-allowed`, `wait`, `help`, `col-resize`, and `row-resize` are recognized only in markup/inline attributes. |

`margin` is parsed by `pwb-web` and forwarded, but the current renderer does not
apply it. Use `padding` on the parent, `gap` between flex children, or a spacer
`<view>`.

Any other property (`flex`, `font-family`, `text-align`, …) is passed through as
a plain attribute. It costs nothing, but has no effect unless the renderer
implements that name. Unsupported properties are ignored, not errors.

### CSS files

`.css` files are bundled with `lightningcss`: `@import` chains are resolved,
the result stays non-minified, and it is applied exactly like `.pss`. This is
the easiest path if you already have a stylesheet, or use a CSS toolchain. Note
the runtime subset above still applies — there is no browser engine behind it.

## PlainJS

All `.pjs`/`.js` files in the site folder are concatenated in alphabetical order
with `// ── file: x.pjs ──` separators. The entry file (`app.pjs`, `main.pjs`, or
`index.pjs`, in that preference order) is placed last so definitions it uses
exist first.

`import`/`export` lines are stripped, because the runtime is script-mode QuickJS.
Use the bundle's named-module map instead:

```js
pwb.import("lib.js");      // evaluate a bundled file by name
pwb.hotload("lib.js");     // re-evaluate it
```

`console.log(...)` output goes to the browser's developer tools (`F12`). Scripts
run in a fresh context per top-level navigation, so globals do not leak between
pages, and each page gets `document`, `pwb`, `fetch`, timers, storage, and
WebSocket globals.

The full API is documented in the
[browser README](../../pwb-browser/README.md#plainjs-pjs-document-api):

- `document.selector(css)`, `document.getElementById(id)`, `document.createElement(tag)`, `document.body`
- `element.getText`/`setText`, `getValue`/`setValue`, `getStyle`/`setStyle`
- `element.onClick(code)`, `element.addEventHandler(event, code)`
- `element.appendChild`/`removeChild`/`remove`, `setAttribute`/`getAttribute`/`removeAttribute`
- `pwb.components()`, `pwb.createComponent(name, props, parentSelector)`, `pwb.loadStyle(css)`, `pwb.import`, `pwb.hotload`
- `console`, `fetch`, `setTimeout`/`setInterval`, `localStorage`/`sessionStorage`, `alert`/`confirm`/`prompt`, `crypto`, `btoa`/`atob`, `performance.now`, `WebSocket`

Notes that surprise web developers:

- `onClick` and `addEventHandler` accept either a code string or a function.
  The handler runs with `this` bound to the element; for `contextmenu`-style
  events it also receives an event object with `preventDefault()`.
- `fetch` returns a Promise but resolves with a plain response object whose
  `text()`/`json()`/`arrayBuffer()` are already resolved Promises.
- `localStorage` is per bundle and persisted by the browser, not by the site.
- There is no `window`, no `document.querySelectorAll`, no `innerHTML` parser.
  Build DOM nodes with `createElement` and `appendChild`.

## Limits to design around

| Limit | Consequence |
|---|---|
| One page per bundle | No routing; build a second bundle and claim a second name for another page, or swap content in place with JS. |
| Top-level files only | Keep assets flat in the site folder, or point `src` at a relative path. |
| PNG images only | Other formats render a placeholder; downloads work for unlinked files. |
| No HTML/CSS engine | Unsupported tags are compile errors and unsupported properties are silent no-ops. Check the tables above. |
| Script-mode JS | `import`/`export` are removed; use `pwb.import`. No `window`, `document` is a small custom object. |
| Compile-time styles | Dynamic restyling requires `setStyle` or `pwb.loadStyle(css)`. |

## See also

- [Authoring quickstart](quickstart.md)
- [pwb-browser runtime & API](../../pwb-browser/README.md)
- [Protocol reference](../../docs/protocol.md) — wire format and bundle internals
