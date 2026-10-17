//! Positioning passes: flow layout, out-of-flow resolution and translation.

use super::boxes::build_box;
use crate::dom::DomNode;
use crate::layout::box_types::{LayoutBox, LayoutCtx};
use crate::layout::text::{resolve_clamped_width, resolve_unit};
use crate::style::{compute_style, ComputedStyle};
use pwb_core::spec::{DisplayStyle, OverflowStyle, PositionStyle};
use tiny_skia::Rect;

/// Lay out a node and its subtree, returning its LayoutBox.
#[allow(clippy::too_many_arguments)]
pub(super) fn layout_node(
    node: &DomNode,
    ctx: &LayoutCtx,
    flow_x: f32,
    y: &mut f32,
    max_width: f32,
    containing: Rect,
    shrink: bool,
    explicit_height: Option<f32>,
) -> LayoutBox {
    let _ = explicit_height;
    let style = compute_style(node);

    // `display: none` elements are removed from layout entirely.
    if style.display == DisplayStyle::None {
        return LayoutBox {
            node_id: node.id,
            tag: node.tag,
            element_id: node.attributes.get("id").cloned(),
            rect: Rect::from_xywh(flow_x, *y, 0.0, 0.0).unwrap(),
            style,
            text: node.text.clone(),
            value: node.attributes.get("value").cloned(),
            placeholder: node.attributes.get("placeholder").cloned(),
            icon: node.attributes.get("icon").cloned(),
            children: Vec::new(),
            overflow: OverflowStyle::Visible,
            content_height: 0.0,
        };
    }

    // Out-of-flow positioning: resolved against the containing block, never advances
    // the flow cursor. Children are laid out in a local coordinate system and the whole
    // subtree is translated to its final position.
    match style.position {
        PositionStyle::Fixed => {
            // Resolve the explicit width first so children lay out against the final
            // box width (previously they sized to the full viewport). The width
            // honors `min-width`/`max-width` (the resize drag clamps the same way).
            let containing_rect = ctx.viewport_rect();
            let w = resolve_clamped_width(
                &style,
                containing_rect.width(),
                ctx.viewport_w,
                ctx.viewport_h,
            )
            .unwrap_or(containing_rect.width());
            // When the height is explicit the final box rect is fully determined
            // before children are laid out, so out-of-flow children (e.g. a
            // bottom-pinned panel) resolve against the real box instead of the
            // content-sized one.
            let explicit_height = style.height.and_then(|uv| {
                resolve_unit(uv, containing_rect.height(), ctx.viewport_w, ctx.viewport_h)
            });
            let mut local_y = 0.0;
            let mut box_node = build_box(
                node,
                ctx,
                &style,
                0.0,
                &mut local_y,
                w,
                containing,
                false,
                explicit_height,
            );
            let (natural_w, natural_h) = (box_node.rect.width(), box_node.rect.height());
            let rect = resolve_oof_rect(
                &style,
                &containing_rect,
                natural_w,
                natural_h,
                flow_x,
                *y,
                ctx.viewport_w,
                ctx.viewport_h,
            );
            let (dx, dy) = (
                rect.left() - box_node.rect.left(),
                rect.top() - box_node.rect.top(),
            );
            translate_box(&mut box_node, dx, dy);
            box_node.rect = rect;
            return box_node;
        }
        PositionStyle::Absolute => {
            let w =
                resolve_clamped_width(&style, containing.width(), ctx.viewport_w, ctx.viewport_h)
                    .unwrap_or(containing.width());
            // Same explicit-height handling as Fixed so a bottom-pinned absolute
            // child resolves against the box's true height.
            let explicit_height = style.height.and_then(|uv| {
                resolve_unit(uv, containing.height(), ctx.viewport_w, ctx.viewport_h)
            });
            let mut local_y = 0.0;
            let mut box_node = build_box(
                node,
                ctx,
                &style,
                0.0,
                &mut local_y,
                w,
                containing,
                false,
                explicit_height,
            );
            let (natural_w, natural_h) = (box_node.rect.width(), box_node.rect.height());
            let rect = resolve_oof_rect(
                &style,
                &containing,
                natural_w,
                natural_h,
                flow_x,
                *y,
                ctx.viewport_w,
                ctx.viewport_h,
            );
            let (dx, dy) = (
                rect.left() - box_node.rect.left(),
                rect.top() - box_node.rect.top(),
            );
            translate_box(&mut box_node, dx, dy);
            box_node.rect = rect;
            return box_node;
        }
        _ => {}
    }

    // In-flow boxes also honor an explicit `height` (used by scroll containers
    // like the devtools tree: the box is fixed-size and its content overflows).
    let explicit_height = style
        .height
        .and_then(|uv| resolve_unit(uv, containing.height(), ctx.viewport_w, ctx.viewport_h));

    let mut box_node = build_box(
        node,
        ctx,
        &style,
        flow_x,
        y,
        max_width,
        containing,
        shrink,
        explicit_height,
    );

    // Relative: shift the box (and its subtree) by the resolved offsets.
    if style.position == PositionStyle::Relative {
        let dx = style.left.unwrap_or(0.0) - style.right.unwrap_or(0.0);
        let dy = style.top.unwrap_or(0.0) - style.bottom.unwrap_or(0.0);
        if dx != 0.0 || dy != 0.0 {
            translate_box(&mut box_node, dx, dy);
        }
    }

    // Sticky: keep the box at its flow position, but clamp it so it never scrolls
    // above `top` (viewport-relative, since flow y already accounts for scroll).
    if style.position == PositionStyle::Sticky {
        if let Some(top) = style.top {
            let dy = (top - box_node.rect.top()).max(0.0);
            if dy > 0.0 {
                translate_box(&mut box_node, 0.0, dy);
            }
        }
    }

    box_node
}

/// Resolve the final rect for an out-of-flow box against its containing block.
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_oof_rect(
    style: &ComputedStyle,
    containing: &Rect,
    natural_w: f32,
    natural_h: f32,
    flow_x: f32,
    flow_y: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> Rect {
    let mut w = natural_w;
    let mut h = natural_h;

    // The explicit width, clamped to `min-width`/`max-width` (matching the
    // box used for children layout, so the final rect agrees with the drag).
    if let Some(v) = resolve_clamped_width(style, containing.width(), viewport_w, viewport_h) {
        w = v;
    }
    if let Some(uv) = style.height {
        if let Some(v) = resolve_unit(uv, containing.height(), viewport_w, viewport_h) {
            h = v;
        }
    }

    let x = if let Some(l) = style.left {
        containing.left() + l
    } else if let Some(r) = style.right {
        (containing.right() - r - w).max(containing.left())
    } else {
        flow_x
    };

    let y = if let Some(t) = style.top {
        containing.top() + t
    } else if let Some(b) = style.bottom {
        (containing.bottom() - b - h).max(containing.top())
    } else {
        flow_y
    };

    Rect::from_xywh(x, y, w.max(1.0), h.max(1.0))
        .unwrap_or_else(|| Rect::from_xywh(x, y, natural_w.max(1.0), natural_h.max(1.0)).unwrap())
}

/// Translate a box and all its children by (dx, dy).
pub(super) fn translate_box(box_node: &mut LayoutBox, dx: f32, dy: f32) {
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    box_node.rect = Rect::from_xywh(
        box_node.rect.left() + dx,
        box_node.rect.top() + dy,
        box_node.rect.width(),
        box_node.rect.height(),
    )
    .unwrap_or(box_node.rect);
    for child in &mut box_node.children {
        translate_box(child, dx, dy);
    }
}
