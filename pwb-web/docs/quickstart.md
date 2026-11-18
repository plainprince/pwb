# Author a PlainWeb site — quickstart

From an empty folder to a `.pwb` bundle you can open locally and publish on the
network. Ten minutes if the workspace is already built.

You need `pwb-web` (compiler), and `pwb-browser` (window) or `pwb-node` (hoster)
to see the result.

## 1. Build the tools

```sh
git clone https://github.com/plainprince/pwb
cd pwb
cargo build --release
export PATH="$PWD/target/release:$PATH"
```

During development, `cargo run -p pwb-web -- ...` works the same way; every
command below is shown with the built binary.

## 2. Create a site folder

A site is a folder of top-level source files. Start with three:

```sh
mkdir mysite
```

`mysite/index.pml`:

```html
<plain>
    <head>
        <title>My Site</title>
    </head>
    <view class="page">
        <text class="title">Hello from PlainWeb</text>
        <text>This page is a compiled bundle, not a request to a server.</text>
        <button id="poke">Poke me</button>
        <text id="out"></text>
    </view>
</plain>
```

`mysite/style.pss`:

```css
.page {
    background-color: #1e1e2e;
    padding: 32px;
    gap: 16px;
    display: flex;
    flex-direction: column;
}

.title {
    color: #89b4fa;
    font-size: 32px;
}

#poke {
    background-color: #a6e3a1;
    color: #11111b;
    padding: 12px;
    border-radius: 8px;
}
```

`mysite/app.pjs`:

```js
var button = document.getElementById("poke");
button.onClick("this.setText('Poked!'); document.getElementById('out').setText('The script ran.');");
```

## 3. Compile

```sh
pwb-web mysite mysite.pwb
```

```text
PlainWeb Layer 1 Compiler (pwb-web)
Parsing site directory: mysite

Successfully compiled PlainWeb bundle!
  Source Folder : mysite
  Output Bundle : mysite.pwb
  Bundle Size   : 508 bytes
  Content ID    : 47aba1d4050887ff3d6e357e84febf085771b71d2ef930eec65af1ff53520462
```

The **content ID** is the BLAKE3 hash of the exact bundle bytes. It is the
permanent address of this revision: change one character and it changes.

Running `pwb-web` with no arguments generates `demo_site/` (if missing) and
writes `demo.pwb`, which is the fastest way to confirm the toolchain works.

## 4. Preview it

```sh
pwb-browser --file mysite.pwb
```

The browser window opens the bundle directly; no node, no network, no account.
Click the button to confirm `app.pjs` ran. Developer tools are on `F12`
(`Cmd+Shift+I` on macOS); `console.log` output appears there.

## 5. Publish it

Compile-on-host (the node compiles the folder at startup):

```sh
pwb-node --host ./mysite
```

Or host the bundle you already built:

```sh
pwb-node --host mysite.pwb
```

```text
[Host] Published 'mysite.pwb'
       Content ID: 47aba1d4…
       Postings replicated to responsible peers; announced via gossip.plain.
```

There is no upload step. Connected peers fetch the artifact and index it. A
name like `mysite.pwb` is a separate, optional claim paid with earned credits;
see the [hoster quickstart](../../pwb-node/docs/hosting-quickstart.md). Keep the
node running and open `mysite.pwb` in `pwb-browser` from another machine on the
network to check that it resolves.

## How a folder becomes a bundle

| File type | What happens |
|---|---|
| `.pml` | Parsed and merged into one page document. All top-level `.pml` files are read in alphabetical order and their children concatenated. |
| `.pss` | Concatenated in alphabetical order, then applied as one stylesheet at compile time. |
| `.css` | Bundled through `lightningcss` (resolves `@import`, non-minified), then applied like `.pss`. |
| `.pjs`, `.js` | Concatenated in alphabetical order into one script. `import`/`export` lines are stripped; the entry file (`app.pjs`, `main.pjs`, or `index.pjs`) runs last. |
| `<component>` / `<import>` | Expanded while compiling; components are not shipped as separate files. |
| `<script src="x.pjs">` or inline `<script>` | Extracted into the same script payload, deduplicated by name. |

Things worth knowing up front:

- **One bundle is one page.** There is no router and no per-file page mapping:
  `about.pml` and `index.pml` are merged into a single document, not two routes.
- **Only top-level files are read.** Files in subfolders are ignored unless a
  component or script `src` points at them. Add images through `src` on
  `<image>`; only PNG is decoded by the renderer.
- **Hidden files are skipped.** `.DS_Store` and editor swap files do not end up
  in the bundle.
- **Styles are resolved at compile time.** The bundle carries the computed
  attributes, not the stylesheet, so a selector that does not match anything at
  build time does nothing at runtime. `pwb.loadStyle(css)` is the runtime
  escape hatch.
- **`pwb-web <path>` also accepts a single `.pml` file**, which is handy for
  testing one page fragment.

## A page with navigation, a form, and a component

`index.pml`:

```html
<plain>
    <head><title>Notes</title></head>

    <component name="card">
        <view class="card">
            <text class="card-title">{title}</text>
            <text>{body}</text>
        </view>
    </component>

    <view class="page">
        <text class="title">Notes</text>

        <import component="card" title="First note" body="Components expand at compile time." />
        <import component="card" title="Second note" body="Parameters use {braces}." />

        <input id="note" placeholder="Write a note" />
        <button id="add">Add</button>
        <view id="list" class="list"></view>

        <link href="search.plain?q=notes"><text>Search the network</text></link>
    </view>
</plain>
```

`app.pjs`:

```js
var input = document.getElementById("note");
var list = document.getElementById("list");

document.getElementById("add").onClick(
    "var value = document.getElementById('note').getValue();" +
    "if (!value) return;" +
    "var item = document.createElement('text');" +
    "item.setText(value);" +
    "document.getElementById('list').appendChild(item);" +
    "document.getElementById('note').setValue('');"
);
```

Every tag, attribute, selector, and property used above is covered in the
[language reference](language.md).

## Sign the bundle (optional)

An unsigned bundle is valid. Signing binds the artifact to an Ed25519 identity,
and `Bundle::deserialize` rejects a bundle whose author signature does not
verify — so a tampered copy shows an error page instead of the content.

```sh
pwb-web mysite mysite.pwb --author-key ~/.pwb/identity.json

# Keeping the key path out of shell history:
PWB_AUTHOR_KEY=~/.pwb/identity.json pwb-web mysite mysite.pwb
```

`--author-key` accepts a `pwb-node` identity JSON (`secret_key_hex`) or a bare
32-byte hex seed. A mismatched `public_key` inside the JSON is rejected rather
than silently changing the author. The key is only used for signing here; the
bundle itself carries the public key, never the secret.

## What the bundle is not

- It is not HTML. Unknown HTML tags are compile errors, not ignored markup;
  only the aliases listed in the [language reference](language.md) are accepted.
- It does not fetch its own assets from a web server. Images are file paths or
  `data:` URLs; the browser resolves them locally or downloads the page as a
  bundle with its image folder.
- It does not contain your `.pss` source. Restyling means recompiling.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `Compilation failed for 'mysite': InvalidTag("wat")` | A tag outside the supported set. | Use a supported tag or alias from the language reference. |
| `Compilation failed …: EmptySource` | The folder has no `.pml` files, or the file is empty/whitespace. | Add a page with a root element. |
| `Compilation failed …: SyntaxError("Unclosed opening tag")` | A `<` without a matching `>`, or an unclosed element. | Check the line; self-close void tags (`<input … />`). |
| `Directory or file does not exist` | Wrong path, or a typo in the folder name. | `ls mysite`; use `.` for the current directory. |
| `Failed to write output binary …` | The output directory does not exist or is not writable. | Create the directory or pick another output path. |
| Styles do nothing | Selector did not match (no descendant selectors), or the value is unsupported. | Start with `#id`, `.class`, or a tag selector; check the property list. |
| Colors are ignored | `#abc` shorthand and most color names are not parsed. | Use `#rrggbb`, `white`, or `black`. |
| Script does nothing | Syntax error, or `import`/`export` was stripped. | Check devtools console; load bundled modules with `pwb.import("lib.js")`. |
| Component shows nothing | Import name does not match, or nesting exceeds 8 levels. | Check the `name`, then compile again; the compiler prints a warning for unknown imports. |

## Next

- [PML, PSS, and PlainJS reference](language.md) — every tag, attribute,
  selector, property, and unit.
- [pwb-browser PlainJS API](../../pwb-browser/README.md#plainjs-pjs-document-api)
  — `document`, `pwb.*`, fetch, storage, timers, WebSocket.
- [Host a site](../../pwb-node/docs/hosting-quickstart.md) — run a node, claim a
  name, and keep it serving.
