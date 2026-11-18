# Design Decisions: Text Selection & Clipboard

## Problem

The PlainWeb Browser needs to support **text selection** with:

- Visual feedback during selection
- `Ctrl+C`/`Cmd+C` copy, `Ctrl+X`/`Cmd+X` cut, `Ctrl+V`/`Cmd+V` paste
- Cross-platform clipboard support (including Linux clipboard managers)
- Right-click context menu with Cut/Copy/Paste/Select All/Inspect

## Solution

### Selection Architecture

The browser tracks selection state in a `TextSelection` struct:

```rust
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextSelection {
    pub start_node_id: Option<usize>,
    pub start_char_pos: Option<usize>,
    pub end_node_id: Option<usize>,
    pub end_char_pos: Option<usize>,
    pub is_selecting: bool,
    pub selected_text_buffer: String,
}
```

Inputs have a separate selection: `BrowserApp::input_selection` stores
`(node_id, anchor, caret)` for the focused field. The anchor stays fixed while
Shift+Left/Right/Home/End moves the caret, so the range can extend in either
direction; the selected range is stored in the DOM `value` only as text, and
the renderer paints the highlight over the input's text run.

### Selection Flow

1. **Mouse Down**: Anchor the selection at the character under the cursor
   (`hit_test_text_char_scrolled` gives the text node id and character index)
   and set `is_selecting`.

2. **Mouse Move**: Update the end node/character and redraw; Shift+click
   extends from the existing anchor.

3. **Mouse Up**: Finalize `selected_text_buffer` with
   `compute_selected_text`, so Copy has the exact text without re-walking the
   DOM.

### Clipboard Integration

The browser uses the `arboard` crate for cross-platform clipboard support.
`BrowserApp` owns **one** `Clipboard` for the whole process lifetime instead of
creating and dropping one per operation:

```rust
pub struct BrowserApp {
    // Kept alive so the X11/Wayland clipboard owner thread survives the copy.
    clipboard: Option<arboard::Clipboard>,
}

fn clipboard(&mut self) -> Option<&mut Clipboard> {
    if self.clipboard.is_none() {
        self.clipboard = Clipboard::new().ok();
    }
    self.clipboard.as_mut()
}
```

Keyboard shortcuts are resolved by `shortcuts::command` and handled before
the page/input sees the key:

- **Select all** (`Ctrl/Cmd+A`): selects the whole focused input value, else
  all page text (browser chrome and devtools excluded).
- **Copy** (`Ctrl+C` / `Cmd+C`): copies the selected input range or page text.
  With nothing selected it copies nothing, matching browsers.
- **Cut** (`Ctrl+X` / `Cmd+X`): copies then deletes the selected input range or
  page text.
- **Paste** (`Ctrl+V` / `Cmd+V`): replaces the selected input range (or inserts
  at the caret) with clipboard text.
- **Editing**: typing, Backspace, and Delete replace/delete an input selection;
  Shift+Left/Right/Home/End extend or shrink it.

Browser shortcuts accept either primary modifier (`Ctrl` on Linux/Windows,
`Cmd` on macOS — both work on either platform), plus `Alt` history/navigation
bindings. See `pwb-browser/src/shortcuts.rs` for the full table.

The same long-lived handle backs the right-click menu actions. The in-window
Linux menu repaints on every pointer move, so the row under the cursor is
highlighted.

### Visual Feedback

The renderer highlights selected text by:

1. Checking `selection.is_selecting` during rendering
2. Drawing a semi-transparent rectangle over the selected region
3. Updating the selection buffer with the actual text content

### Security Considerations

- **No arbitrary data**: Only text can be copied, no DOM nodes or scripts
- **Explicit opt-in**: Clipboard reads happen only for a paste request
- **Small surface**: Paste text is inserted into the focused `<input>` only

### Performance

- Selection state is lightweight (just coordinates and flags)
- Clipboard calls happen only on an explicit shortcut or menu action
- Visual feedback is rendered efficiently with Skia

## Future Directions

1. **Double-click word selection**: Extend to support word-level selection
2. **Touch support**: Implement touch-based selection for mobile
3. **Input selection by mouse drag**: Extend a range inside an input with the
   pointer, not only with Shift+arrows
4. ~~**Context menu**: Add right-click context menu with copy option~~ — implemented:
   native AppKit (macOS) and Win32 (Windows 10/11), in-window on Linux
   (Wayland/X11 expose no popup-menu API without a desktop toolkit)
