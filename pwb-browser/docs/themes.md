# Themes (`.ptheme`)

A theme changes the browser chrome **and** the default styling that pages
inherit when their own PSS says nothing. Themes live in `$PWB_DATA/themes`
(default `~/.pwb/themes`) as one `.ptheme` file each; the browser writes
`default-dark.ptheme` and `default-light.ptheme` there on first start and
never overwrites an existing file.

Pick a theme in **Settings** (the gear in the toolbar), which writes the choice
to the session so it survives a restart. Editing the active `.ptheme` reloads it
while the browser runs: the chrome is rebuilt in place, and colors/sizes apply
to the current page immediately (page-level `page-pss` defaults apply on the
next navigation).

Broken or partial files are safe: every missing field falls back to the
hardcoded dark theme, and a file that fails to parse is reported and ignored.

## Format

The syntax is JSON-like: `//` and `/* */` comments, unquoted keys, trailing
commas, single- or double-quoted strings, and `"""…"""` raw blocks for PSS all
work.

```ptheme
// ~/.pwb/themes/midnight.ptheme
{
  name: "Midnight",
  dark: true,

  colors: {
    chrome: "#101014",
    surface: "#17171d",
    surface-hover: "#22222b",
    border: "#2a2a34",
    text: "#e8e8f0",
    text-muted: "#9a9aa8",
    accent: "#8ab4ff",
    address-bg: "#0b0b0f",
    address-text: "#f4f4fa",
    tab-active: "#282833",
    tab-inactive: "#17171d",
    tab-hover: "#20202a",
    ok: "#5fd08a",
    danger: "#ff6b6b",
  },

  // Fallback colors for pages that do not style themselves. Authored PSS
  // always wins over these.
  page: {
    background: "#0b0b0f",
    text: "#e8e8f0",
    link: "#7fb3ff",
    button-bg: "#2a2a36",
    button-text: "#f5f5fa",
    input-bg: "#15151b",
    input-text: "#f2f2f8",
    input-border: "#33333f",
    placeholder: "#7c7c8a",
    scrollbar-track: "#111116",
    scrollbar-thumb: "#3d3d4a",
  },

  // Pixel sizes. They scale gently with the window instead of tracking `vh`,
  // so controls stay usable on short and tall windows alike.
  sizes: {
    "ui-font": 14,
    "ui-font-small": 12,
    "tab-height": 40,
    "toolbar-height": 46,
    control: 30,
    gap: 7,
    radius: 9,
    icon: 17,
    "page-font": 18,
    "page-padding": 20,
    "page-gap": 12,
  },

  layout: {
    "close-button": "hover",     // hover | always | never
    "address-width": 50,         // percent of the toolbar
    "tab-min-width": 96,
    "tab-max-width": 230,
    "tabs-align": "start",       // start | center | end
    "scroll-buttons": true,
  },

  // Chrome icon roles → feather glyph names (or names from icons-custom).
  icons: {
    back: "arrow-left",
    forward: "arrow-right",
    reload: "refresh-cw",
    "new-tab": "plus",
    close: "x",
    bookmark: "bookmark",
    bookmarks: "book-open",
    history: "clock",
    settings: "settings",
  },

  // Optional glyphs drawn from raw 24x24 M/L/C/Z path data.
  "icons-custom": {
    dot: "M 12 6 L 18 12 L 12 18 L 6 12 Z",
  },

  // Full PSS for the chrome, applied after the generated rules.
  ui: """
    .pwb-chrome { border-bottom: 2px solid #8ab4ff; }
    .pwb-tab { border-radius: 4px; }
    .pwb-address { border-radius: 15px; }
  """,

  // Fallback-only PSS for pages: a property is inserted only when the page
  // does not already set it.
  "page-pss": """
    view { padding: 16px; }
    link { color: #7fb3ff; }
  """,
}
```

## What a theme controls

1. **UI colors** — `colors.*` feed the generated chrome stylesheet.
2. **Website default colors** — `page.*` are the colors `compute_style` starts
   from. Site PSS and markup attributes are applied afterwards, so a page can
   always override them.
3. **UI layout** — `layout.*` for the structural knobs (tab sizes, address
   width, close-button policy), and `ui` for arbitrary PSS. PSS is applied to a
   chrome tree whose elements carry stable classes: `.pwb-chrome`,
   `.pwb-tabstrip`, `.pwb-tab` (plus `.active`/`.hovered`), `.pwb-tab-close`,
   `.pwb-toolbar`, `.pwb-nav`, `.pwb-address`, `.pwb-actions`, `.pwb-status`,
   `.pwb-iconbtn`, `.pwb-newtab`.
4. **Icon set** — `icons.*` maps each role to a glyph; `icons-custom` adds
   glyphs from raw path data.
5. **Sizes** — `sizes.*` are pixels, scaled by `sqrt((width/1200)*(height/800))`
   clamped to a sane range. Page defaults (`page-font`, `page-padding`,
   `page-gap`) use the same mechanism instead of `vh`, which used to make text
   tiny on short windows and huge on tall ones.

## Builtins

`default-dark` and `default-light` are hardcoded in
[`src/theme.rs`](../src/theme.rs) and written to the themes directory as
editable files. Deleting a file restores the hardcoded version on the next
start; deleting every field of one still leaves a working theme because the
dark defaults fill the gaps.
