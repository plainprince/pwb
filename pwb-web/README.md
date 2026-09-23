# pwb-web — layer 1: authoring & site compilation

`pwb-web` takes a site folder containing:
- **`.pml`** — Plain Markup Language (element tags like `<view>`, `<text>`, `<button>`).
- **`.pss`** — Plain Stylesheets (`.class`, `#id`, or tag selectors).
- **`.pjs`** — PlainJS / Plain JavaScript (optional opaque script payload).

and compiles them into a single, compact, losslessly compressed binary **`.pwb`** bundle.

## Usage

```sh
cargo run -p pwb-web -- <site_directory> [output.pwb]
```
