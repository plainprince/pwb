//! Entry points: rendering a DOM into a pixmap.

use crate::dom::Dom;
use crate::layout::{build_layout_tree, LayoutBox};
use crate::render::core::RenderState;
use crate::render::layout_box::render_layout_box;
use std::collections::HashMap;
use tiny_skia::{Color as SkiaColor, Paint, Pixmap, Rect, Transform};

/// Information about the rendered page scrollbar, if any.
pub struct ScrollbarInfo {
    pub track_rect: Rect,
    pub thumb_rect: Rect,
}

/// Render the DOM into an existing pixmap, reusing its buffer across frames.
///
/// Allocating a fresh full-viewport `Pixmap` on every frame leaves many megabytes of
/// freed-but-cached allocations behind (a Retina window is 1800x1300 = ~9MB per frame),
/// so the caller keeps one pixmap alive and we only reallocate when the size changes.
#[allow(clippy::too_many_arguments)]
pub fn render_dom_into_pixmap(
    pixmap: &mut Pixmap,
    dom: &Dom,
    width: u32,
    height: u32,
    scroll_y: f32,
    selection: &crate::render::TextSelection,
    element_scrolls: &HashMap<usize, f32>,
    input_focus: Option<(usize, usize)>,
) -> (LayoutBox, Option<ScrollbarInfo>) {
    if pixmap.width() != width || pixmap.height() != height {
        if let Some(new_pixmap) = Pixmap::new(width, height) {
            *pixmap = new_pixmap;
        }
    }
    pixmap.fill(SkiaColor::from_rgba8(17, 17, 27, 255));

    let layout_root = build_layout_tree(dom, width, height, scroll_y);
    render_layout_box(
        &layout_root,
        pixmap,
        width,
        height,
        selection,
        element_scrolls,
        input_focus,
        RenderState::identity(),
    );

    // Calculate scrollbar info
    let content_height = layout_root.rect.height();
    let viewport_height = height as f32;

    let scrollbar_info = if content_height > viewport_height {
        let scrollbar_width = 12.0;
        let track_height = viewport_height;
        let thumb_height = (viewport_height / content_height * viewport_height).max(30.0);
        let max_scroll = content_height - viewport_height;
        let thumb_y = (scroll_y / max_scroll * (track_height - thumb_height))
            .clamp(0.0, track_height - thumb_height);

        // When the devtools are docked the page viewport ends at the sidebar's
        // left edge — the page scrollbar sits beside it, not underneath it.
        let dock_w =
            crate::layout::docked_devtools_width(dom, width as f32, height as f32).unwrap_or(0.0);
        let right_edge = (width as f32 - dock_w).max(scrollbar_width);

        let info = ScrollbarInfo {
            track_rect: Rect::from_xywh(
                right_edge - scrollbar_width,
                0.0,
                scrollbar_width,
                track_height,
            )
            .unwrap(),
            thumb_rect: Rect::from_xywh(
                right_edge - scrollbar_width,
                thumb_y,
                scrollbar_width,
                thumb_height,
            )
            .unwrap(),
        };

        // Render Scrollbar track
        let mut track_paint = Paint::default();
        track_paint.set_color(SkiaColor::from_rgba8(40, 40, 55, 255));
        pixmap.fill_rect(info.track_rect, &track_paint, Transform::identity(), None);

        // Render Scrollbar thumb
        let mut thumb_paint = Paint::default();
        thumb_paint.set_color(SkiaColor::from_rgba8(120, 120, 140, 255));
        pixmap.fill_rect(info.thumb_rect, &thumb_paint, Transform::identity(), None);

        Some(info)
    } else {
        None
    };

    (layout_root, scrollbar_info)
}

pub fn render_dom_to_pixmap(
    dom: &Dom,
    width: u32,
    height: u32,
    scroll_y: f32,
    selection: &crate::render::TextSelection,
    _devtools_open: bool,
) -> (Pixmap, LayoutBox, Option<ScrollbarInfo>) {
    let mut pixmap = Pixmap::new(width, height).unwrap_or_else(|| Pixmap::new(800, 600).unwrap());
    let (layout_root, scrollbar_info) = render_dom_into_pixmap(
        &mut pixmap,
        dom,
        width,
        height,
        scroll_y,
        selection,
        &HashMap::new(),
        None,
    );
    (pixmap, layout_root, scrollbar_info)
}
