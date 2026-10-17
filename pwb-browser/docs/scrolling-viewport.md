# Design Decisions: Scrolling, Viewport & Scrollbar

## Content-Bound Scrolling

### Problem

Without a scroll cap the user can scroll infinitely past the end of the page,
leaving a blank viewport with no visual feedback about where the content went.

### Solution

After every scroll event the browser computes a **zero-scroll layout** to obtain
the full content height, then clamps `scroll_y`:

```rust
let layout_root = build_layout_tree(&dom, width, height, /* scroll_y = */ 0.0);
let content_height = layout_root.rect.height();
let max_scroll = (content_height - viewport_height).max(0.0);
scroll_y = scroll_y.clamp(0.0, max_scroll);
```

Key points:

- `scroll_y` is clamped to `[0, max_scroll]`.
- `max_scroll` is `0.0` when content fits entirely in the viewport (no
  scrolling possible).
- The zero-scroll layout is intentional: we need the *total* content height,
  not the height with the current scroll offset baked in.

## Visual Scrollbar

### Problem

With no scrollbar the user has no indication of:
- How much content exists below the fold.
- Where they are within the document.

### Solution

A **minimal vertical scrollbar** is rendered as a final pass on the pixmap,
drawn on top of all other content. It appears **only when the content exceeds
the viewport height**.

#### Scrollbar Geometry

| Property | Value | Rationale |
|----------|-------|-----------|
| **Track width** | 12 px | Thin enough to stay unobtrusive |
| **Track colour** | `rgba(40, 40, 55, 1.0)` | Matches the dark background palette |
| **Thumb colour** | `rgba(120, 120, 140, 1.0)` | Visible but not distracting |
| **Thumb height** | `(viewport / content) * viewport`, min 30 px | Proportional to content ratio |
| **Thumb Y** | `(scroll_y / max_scroll) * (track - thumb)` | Linear mapping of scroll state |
| **Position** | Right edge of viewport | Conventional scrollbar position |

#### Implementation

The scrollbar is computed and drawn inside `render_dom_to_pixmap` as a post-
processing step. A `ScrollbarInfo` struct is returned alongside the pixmap so
callers (hit-testing, future drag-to-scroll) can reason about the scrollbar
without re-deriving it:

```rust
pub struct ScrollbarInfo {
    pub track_rect: Rect,
    pub thumb_rect: Rect,
}
```

### Rendering Order

```
1. Clear canvas → background colour
2. Layout tree  → compute positions
3. Render boxes → text, buttons, views
4. Scrollbar    → track + thumb on top
```

Drawing the scrollbar last prevents page content from occluding it.

### No Scrollbar When Not Needed

When `content_height <= viewport_height`, `scrollbar_info` is `None`.
No track, no thumb, no wasted fill calls. This avoids visual clutter on
short pages.

## Viewport-Relative Scaling

All layout metrics — font size, padding, margin — are multiplied by a
viewport scale factor:

```rust
let viewport_scale = (viewport_height as f32 / 600.0).clamp(0.75, 2.5);
```

| Window height | Scale factor |
|---------------|-------------|
| ≤ 450 px | 0.75 (floor) |
| 600 px | 1.0 (baseline) |
| 1200 px | 2.0 |
| ≥ 1500 px | 2.5 (ceiling) |

This ensures readable typography on both small laptop screens and large
monitors without requiring an explicit zoom control.
