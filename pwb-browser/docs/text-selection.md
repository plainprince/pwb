# Design Decisions: Text Selection & Clipboard

## Problem

The PlainWeb Browser needs to support **text selection** with:

- Visual feedback during selection
- `Ctrl+C`/`Cmd+C` clipboard copy
- Cross-platform compatibility

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

3. **Mouse Up**: Finalize selection and copy to clipboard
   ```rust
   selection.is_selecting = false;
   ```

### Clipboard Integration

The browser uses the `arboard` crate for cross-platform clipboard support:

```rust
if key_event.modifiers.state().control_key() && code == KeyCode::KeyC {
    if !selection.selected_text_buffer.is_empty() {
        if let Ok(mut clipboard) = Clipboard::new() {
            let _ = clipboard.set_text(selection.selected_text_buffer.clone());
        }
    }
}
```

### Visual Feedback

The renderer highlights selected text by:

1. Checking `selection.is_selecting` during rendering
2. Drawing a semi-transparent rectangle over the selected region
3. Updating the selection buffer with the actual text content

### Security Considerations

- **No arbitrary data**: Only text can be copied, no DOM nodes or scripts
- **Explicit opt-in**: Requires `Ctrl+C`/`Cmd+C` key combination
- **Read-only**: Clipboard access is write-only (set text)

### Performance

- Selection state is lightweight (just coordinates and flags)
- Clipboard operations are non-blocking
- Visual feedback is rendered efficiently with Skia

## Future Directions

1. **Double-click word selection**: Extend to support word-level selection
2. **Drag-to-select**: Add mouse drag support for selection
3. **Touch support**: Implement touch-based selection for mobile
4. **Context menu**: Add right-click context menu with copy option
