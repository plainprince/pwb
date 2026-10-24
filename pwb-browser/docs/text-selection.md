# Design Decisions: Text Selection & Clipboard

## Problem

The PlainWeb Browser needs to support **text selection** with:

- Visual feedback during selection
- `Ctrl+C`/`Cmd+C` copy, `Ctrl+X`/`Cmd+X` cut, `Ctrl+V`/`Cmd+V` paste
- Cross-platform clipboard support (including Linux clipboard managers)
- Right-click context menu with Copy

## Solution

### Selection Architecture

The browser tracks selection state in a `TextSelection` struct:

```rust
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextSelection {
    pub start: Option<SelectionPoint>,
    pub current: Option<SelectionPoint>,
    pub is_selecting: bool,
    pub selected_text_buffer: String,
}
```

### Selection Flow

1. **Mouse Down**: Set selection start and begin tracking
   ```rust
   selection.start = Some(current_mouse_pos);
   selection.current = Some(current_mouse_pos);
   selection.is_selecting = true;
   ```

2. **Mouse Move**: Update selection end and redraw
   ```rust
   selection.current = Some(current_mouse_pos);
   window.request_redraw();
   ```

3. **Mouse Up**: Finalize the selected text buffer
   ```rust
   selection.is_selecting = false;
   selection.selected_text_buffer = compute_selected_text(...);
   ```

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

Keyboard shortcuts are handled in the first `KeyboardInput` arm (later arms
for pressed keys are unreachable):

- **Copy** (`Ctrl+C` / `Cmd+C`): copies page text selection, or the focused
  input's value.
- **Cut** (`Ctrl+X` / `Cmd+X`): copies then deletes the selection (or clears the
  focused input).
- **Paste** (`Ctrl+V` / `Cmd+V`): inserts clipboard text at the focused input's
  caret.

The same long-lived handle backs the right-click **Copy** action. The in-window
menu repaints on every pointer move, so the row under the cursor is highlighted.

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
2. **Drag-to-select**: Add mouse drag support for selection
3. **Touch support**: Implement touch-based selection for mobile
4. ~~**Context menu**: Add right-click context menu with copy option~~ — implemented (native on macOS, in-window elsewhere)
