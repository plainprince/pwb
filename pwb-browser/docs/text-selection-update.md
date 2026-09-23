# Text Selection System Update

## Problem
The previous text selection system used box selection, where clicking and dragging would select a rectangular area based on mouse position. This is not how normal browsers work - users expect text range selection like in any standard web browser.

## Solution
Implemented proper text range selection that works like standard browsers:

### Changes Made

1. **Updated `TextSelection` struct** (`render.rs`):
   - Node/character-based selection: `start_node_id`, `start_char_pos`, `end_node_id`, `end_char_pos`
   - `is_selecting` marks an in-progress drag; `selected_text_buffer` holds the finalized text for Ctrl+C

2. **Updated `is_point_selected` function** (`render.rs`):
   - Takes `node_id` and `char_index`; the renderer now passes the real per-character index
   - Normalizes reversed ranges (drag left/up) before applying range logic:
     - Same node: selects characters between start and end positions
     - Different nodes: selects all text in intermediate nodes

3. **Updated mouse event handling** (`pipeline.rs`):
   - `hit_test_text_char` (`layout.rs`) maps a pixel position to `(node_id, char_index)` using the
     same per-glyph advance stepping as the renderer
   - Click on text: anchors selection start (and end) at that character
   - Shift+Click: moves only the end boundary
   - Drag: updates the end boundary to the character under the cursor
   - Click on non-text (buttons, views): clears the selection
   - Release: finalizes `selected_text_buffer` by walking the DOM in document order

4. **Updated rendering pipeline**:
   - Passes `node_id` to text rendering functions
   - Tracks character index during text rendering
   - Applies selection highlighting based on text range

## How to Use

1. **Click and drag**: Select text by clicking and dragging (standard selection)
2. **Click to start**: Click where you want selection to begin
3. **Shift+Click**: Hold Shift and click to extend selection to that position
4. **Scroll during selection**: If you scroll while holding click, selection continues from where you started

## Testing
- All unit tests pass
- Selection logic tested with various node/character combinations
- Backward compatibility maintained for existing selection behavior