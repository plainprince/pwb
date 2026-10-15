//! Layout tree builder: computes the LayoutBox tree from a DOM.

use super::box_types::{get_font, LayoutBox, LayoutCtx};
use super::text::{
    docked_devtools_width, measure_text_width, resolve_clamped_width, resolve_len, resolve_unit,
};
use crate::dom::{Dom, DomNode};
use crate::style::{compute_style, ComputedStyle};
// use ab_glyph::{Font, FontArc};  // unused
use pwb_core::spec::{
    Alignment, CursorStyle, DisplayStyle, FlexDirection, OverflowStyle, PositionStyle, Tag,
    UnitValue,
};
// use std::collections::HashMap; // unused
use tiny_skia::Rect;

/// Build a unified Layout Tree for the DOM.
pub fn build_layout_tree(
    dom: &Dom,
    viewport_width: u32,
    viewport_height: u32,
    scroll_y: f32,
) -> LayoutBox {
    // Page margins are viewport-relative (5vh ≈ 30px at a 600px-tall window,
    // ~65px at 1300px) so pages keep their proportions at any window size.
    let margin_x = viewport_height as f32 * 0.05;
    let mut current_y = margin_x - scroll_y;

    // When the devtools are docked (visible) they occupy their own column on
    // the right: the page lays out beside them and the site content shrinks to
    // make room instead of being covered by the overlay.
    let dock_w =
        docked_devtools_width(dom, viewport_width as f32, viewport_height as f32).unwrap_or(0.0);
    let max_w = (viewport_width as f32 - (margin_x * 2.0) - dock_w).max(100.0);

    let ctx = LayoutCtx {
        viewport_w: viewport_width as f32,
        viewport_h: viewport_height as f32,
    };
    let viewport = ctx.viewport_rect();
    let mut root = layout_node(
        &dom.root,
        &ctx,
        margin_x,
        &mut current_y,
        max_w,
        viewport,
        false,
        None,
    );
    // Inherit cursor from parent: children with no explicit cursor adopt
    // their parent's cursor (matching CSS `cursor: inherit` behavior).
    propagate_cursor(&mut root, None);
    root
}

/// Recursively set `cursor` on children that have `None`, using the
/// parent's cursor (CSS `cursor: inherit` behavior).
fn propagate_cursor(box_node: &mut LayoutBox, parent_cursor: Option<CursorStyle>) {
    if box_node.style.cursor.is_none() {
        box_node.style.cursor = parent_cursor;
    }
    let my_cursor = box_node.style.cursor;
    for child in &mut box_node.children {
        propagate_cursor(child, my_cursor);
    }
}

fn is_positioned(style: &ComputedStyle) -> bool {
    style.position != PositionStyle::Static
}

/// Lay out a node and its subtree, returning its LayoutBox.
#[allow(clippy::too_many_arguments)]
fn layout_node(
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

/// Resolve a box's final width given its natural content width, honoring an
/// explicit `width`, `min-width`, and `max-width` (each resolved against the
/// containing block) and capped at the available `max_width`. Used by both the
/// View box resolution and the flex-row `justify-content` distribution so the
/// free space is computed over the SAME width the box is actually drawn at.
fn final_box_width(
    style: &ComputedStyle,
    natural: f32,
    max_width: f32,
    dim: f32,
    ctx: &LayoutCtx,
) -> f32 {
    let min = style
        .min_width
        .and_then(|uv| resolve_unit(uv, dim, ctx.viewport_w, ctx.viewport_h))
        .unwrap_or(0.0);
    let max = style
        .max_width
        .and_then(|uv| resolve_unit(uv, dim, ctx.viewport_w, ctx.viewport_h))
        .unwrap_or(f32::MAX);
    let explicit = style
        .width
        .and_then(|uv| resolve_unit(uv, dim, ctx.viewport_w, ctx.viewport_h));
    let w = explicit.unwrap_or(natural).clamp(min, max);
    w.min(max_width)
}

/// Lay out a node in-flow (or in local coordinates for out-of-flow elements).
#[allow(clippy::too_many_arguments)]
fn build_box(
    node: &DomNode,
    ctx: &LayoutCtx,
    style: &ComputedStyle,
    flow_x: f32,
    y: &mut f32,
    max_width: f32,
    containing: Rect,
    shrink: bool,
    explicit_height: Option<f32>,
) -> LayoutBox {
    let element_id = node.attributes.get("id").cloned();
    let start_y = *y;
    let is_view = node.tag == Tag::View;

    // Defaults are viewport-relative (3.33vh ≈ 20px at a 600px-tall window) so
    // views keep their previous proportions on any display.
    let padding = if is_view {
        resolve_len(
            style.padding,
            UnitValue::Vh(0.0333),
            containing.width(),
            ctx.viewport_w,
            ctx.viewport_h,
        )
    } else {
        0.0
    };

    let row_layout = is_view && style.flex_direction == FlexDirection::Row;
    let gap_px = resolve_len(
        style.gap,
        UnitValue::Px(0.0),
        containing.width(),
        ctx.viewport_w,
        ctx.viewport_h,
    );

    // View containers wrap their children with padding; Document/Head/Title and unknown
    // tags pass through to their children without extra spacing.
    let pass_through = !is_view
        && !matches!(
            node.tag,
            Tag::Text | Tag::Button | Tag::Link | Tag::Code | Tag::Icon | Tag::Input | Tag::Image
        );
    let child_x = if is_view { flow_x + padding } else { flow_x };
    let child_max_w = if is_view {
        (max_width - padding * 2.0).max(50.0)
    } else {
        max_width
    };

    // First pass: lay out in-flow children sequentially, collecting out-of-flow
    // children for a second pass (so they paint and hit-test on top of flow content).
    let mut children = Vec::new();
    let mut out_of_flow = Vec::new();

    // Rightmost extent of in-flow children (used for shrink-to-fit sizing).
    let mut content_right: f32 = flow_x + padding;
    // Flex row: track tallest child for vertical centering.
    let mut row_max_h: f32 = 0.0;

    if pass_through || is_view {
        if row_layout {
            // Flex row: children are placed left-to-right with `gap` between them.
            // Their vertical position is resolved after the row height is known.
            let mut cx = flow_x + padding;
            for child in &node.children {
                let child_style = compute_style(child);
                if child_style.display == DisplayStyle::None {
                    continue;
                }
                let is_child_oof = is_positioned(&child_style)
                    && !matches!(child_style.position, PositionStyle::Relative);
                if is_child_oof {
                    // Positioned children use local coordinates; translate later.
                    // A fixed/absolute child resolves its own explicit width first
                    // and lays its subtree out against it (e.g. the fixed devtools
                    // sidebar), instead of inheriting the parent's flow width.
                    let mut local_y = 0.0;
                    let oof_max_w = resolve_clamped_width(
                        &child_style,
                        containing.width(),
                        ctx.viewport_w,
                        ctx.viewport_h,
                    )
                    .unwrap_or(child_max_w);
                    let b = build_box(
                        child,
                        ctx,
                        &child_style,
                        0.0,
                        &mut local_y,
                        oof_max_w,
                        containing,
                        false,
                        None,
                    );
                    out_of_flow.push((b, child_style));
                } else {
                    // Flex row children use a local y coordinate so they don't advance
                    // the outer flow cursor. They start at the row's top padding
                    // (like flex-column children do), so they sit in the content
                    // area and are vertically centered instead of glued to the
                    // row's top edge. Vertical alignment is handled after row
                    // height is known. Children shrink-to-fit their content
                    // (tabs, chips, controls) instead of filling the row width.
                    let mut local_y = padding;
                    let b = layout_node(
                        child,
                        ctx,
                        cx,
                        &mut local_y,
                        child_max_w,
                        containing,
                        true,
                        None,
                    );
                    // For flex row, track the rightmost extent for shrink-to-fit.
                    content_right = content_right.max(b.rect.right());
                    row_max_h = row_max_h.max(b.rect.height());
                    cx = b.rect.right() + gap_px;
                    children.push(b);
                }
            }
            // Distribute extra space per `justify-content` (flex-row only). Free space is
            // measured against the row's FINAL drawn width (honoring explicit
            // width / min/max-width) minus the children and gaps — not the
            // child cursor — so center/space-between/end leave the real
            // leftover instead of over-shifting children out of the box.
            let natural_row = (content_right - (flow_x + padding)).max(0.0) + padding * 2.0;
            let row_w = final_box_width(style, natural_row, max_width, containing.width(), ctx);
            let content_w = (row_w - padding * 2.0).max(0.0);
            let children_w: f32 = children.iter().map(|c| c.rect.width()).sum();
            let gaps_total = gap_px * (children.len().saturating_sub(1) as f32);
            let leftover = (content_w - children_w - gaps_total).max(0.0);
            match style.justify_content {
                Alignment::SpaceBetween => {
                    if children.len() > 1 {
                        let extra = leftover / (children.len() - 1) as f32;
                        let mut acc = 0.0;
                        for b in children.iter_mut().skip(1) {
                            acc += extra;
                            if acc > 0.0 {
                                translate_box(b, acc, 0.0);
                            }
                        }
                    }
                }
                Alignment::Center => {
                    let shift = leftover / 2.0;
                    if shift > 0.0 {
                        for b in &mut children {
                            translate_box(b, shift, 0.0);
                        }
                    }
                }
                Alignment::End => {
                    if leftover > 0.0 {
                        for b in &mut children {
                            translate_box(b, leftover, 0.0);
                        }
                    }
                }
                Alignment::Start => {}
            }
            // Vertically center children within the row's INNER content height. By
            // default rows have no explicit height and the inner box equals the
            // tallest child, so children stay where they were laid. But a
            // fixed-height tab/header is taller than its icon/text content, and
            // centering against only `row_max_h` leaves the icon pinned to the
            // top padding — center against the real inner height instead.
            let inner_h = explicit_height
                .map(|h| (h - padding * 2.0).max(0.0))
                .unwrap_or(row_max_h)
                .max(row_max_h);
            for b in &mut children {
                let child_h = b.rect.height();
                let dy = (inner_h - child_h) / 2.0;
                if dy > 0.0 {
                    translate_box(b, 0.0, dy);
                }
            }
            // Flex-row children were laid out in local coordinates (they don't
            // advance the outer flow cursor). Offset them into absolute flow
            // position so they sit where the row actually is in the document —
            // otherwise every row child collapses to the top of the page
            // regardless of the row's real y.
            for b in &mut children {
                if start_y != 0.0 {
                    translate_box(b, 0.0, start_y);
                }
            }
            // Advance the flow cursor: top padding + tallest child (bottom padding is
            // added by the View rect computation below).
            *y = start_y + padding + row_max_h;
        } else {
            // Flex column (or pass-through): stack children top-to-bottom.
            if is_view {
                *y += padding;
            }
            let column_gap = resolve_len(
                style.gap,
                UnitValue::Vh(0.02),
                containing.width(),
                ctx.viewport_w,
                ctx.viewport_h,
            );
            for child in &node.children {
                let child_style = compute_style(child);
                if child_style.display == DisplayStyle::None {
                    continue;
                }
                let is_child_oof = is_positioned(&child_style)
                    && !matches!(child_style.position, PositionStyle::Relative);
                if is_child_oof {
                    // Same fixed/absolute width resolution as the row branch:
                    // the child lays its subtree out against its own explicit
                    // width, not the parent's flow width.
                    let mut local_y = 0.0;
                    let oof_max_w = resolve_clamped_width(
                        &child_style,
                        containing.width(),
                        ctx.viewport_w,
                        ctx.viewport_h,
                    )
                    .unwrap_or(child_max_w);
                    let b = build_box(
                        child,
                        ctx,
                        &child_style,
                        0.0,
                        &mut local_y,
                        oof_max_w,
                        containing,
                        false,
                        None,
                    );
                    out_of_flow.push((b, child_style));
                } else {
                    let b =
                        layout_node(child, ctx, child_x, y, child_max_w, containing, false, None);
                    content_right = content_right.max(b.rect.right());
                    if is_view {
                        *y += column_gap;
                    }
                    children.push(b);
                }
            }
        }
    }

    // Second pass: lay out out-of-flow children in local coordinates, then translate
    // them to their absolute positions against the containing block.
    // A positioned box (fixed/absolute) is itself the containing block for its
    // out-of-flow descendants: their percentages and offsets resolve against
    // this box's own rect (the fixed devtools sidebar's bottom-pinned styles
    // panel), not the outer flow the box was invoked from.
    let oof_containing = if matches!(
        style.position,
        PositionStyle::Fixed | PositionStyle::Absolute
    ) {
        let w = resolve_clamped_width(style, containing.width(), ctx.viewport_w, ctx.viewport_h)
            .unwrap_or(max_width);
        let h = explicit_height
            .or_else(|| {
                style.height.and_then(|uv| {
                    resolve_unit(uv, containing.height(), ctx.viewport_w, ctx.viewport_h)
                })
            })
            .unwrap_or_else(|| (*y - start_y + padding).max(if is_view { 20.0 } else { 0.0 }));
        Rect::from_xywh(flow_x, start_y, w.max(1.0), h.max(1.0)).unwrap_or(containing)
    } else {
        containing
    };
    for (mut b, child_style) in out_of_flow {
        let (natural_w, natural_h) = (b.rect.width(), b.rect.height());
        let rect = resolve_oof_rect(
            &child_style,
            &oof_containing,
            natural_w,
            natural_h,
            flow_x,
            start_y,
            ctx.viewport_w,
            ctx.viewport_h,
        );
        let (dx, dy) = (rect.left() - b.rect.left(), rect.top() - b.rect.top());
        translate_box(&mut b, dx, dy);
        b.rect = rect;
        children.push(b);
    }

    // Text nodes: measure the text and create a leaf box.
    // Buttons/Links/Code are text-like but have their own sizing logic.
    let rect = match node.tag {
        Tag::Document
        | Tag::Head
        | Tag::Title
        | Tag::Image
        | Tag::Script
        | Tag::Component
        | Tag::Import => {
            let height = explicit_height.unwrap_or_else(|| (*y - start_y).max(0.0));
            Rect::from_xywh(flow_x, start_y, max_width, height)
                .unwrap_or_else(|| Rect::from_xywh(flow_x, start_y, max_width, 1.0).unwrap())
        }
        Tag::View => {
            // Height covers top padding + children + bottom padding so the box
            // (background, borders, hit area) reaches its true bottom edge. The
            // flow cursor is unchanged from the pre-existing behavior (it already
            // advanced past the bottom padding). An explicit height (e.g. from
            // `height: 100vh` on a fixed sidebar, or `height: 58vh` on a scroll
            // container) wins over the content size, and flow continues below it.
            let height = match explicit_height {
                Some(h) => {
                    *y = start_y + h + padding;
                    h
                }
                None => {
                    let h = (*y - start_y + padding).max(20.0);
                    *y = start_y + h;
                    h
                }
            };
            // Shrink-to-fit: size the box to its content instead of the full width
            // (used by flex-row items such as devtools tabs). Explicit width,
            // min-width, and max-width are honored in both shrink and fill modes.
            let natural = if shrink {
                (content_right - (flow_x + padding)).max(0.0) + padding * 2.0
            } else {
                max_width
            };
            let width = final_box_width(style, natural, max_width, containing.width(), ctx);
            Rect::from_xywh(flow_x, start_y, width, height)
                .unwrap_or_else(|| Rect::from_xywh(flow_x, start_y, max_width, 20.0).unwrap())
        }
        Tag::Text => {
            let font_size = resolve_len(
                style.font_size,
                UnitValue::Vh(0.03),
                containing.width(),
                ctx.viewport_w,
                ctx.viewport_h,
            );
            let text = node.text.as_deref().unwrap_or("");
            let w = measure_text_width(text, font_size, get_font());
            let h = font_size;
            let rect = Rect::from_xywh(flow_x, *y, w, h)
                .unwrap_or_else(|| Rect::from_xywh(flow_x, *y, 1.0, h).unwrap());
            *y += h;
            rect
        }
        Tag::Button | Tag::Link | Tag::Code => {
            // Buttons, links, and code boxes use text metrics with padding.
            // Defaults: 2.5vh font, 3.33vh padding (≈15px and 20px at 600px tall).
            let font_size = resolve_len(
                style.font_size,
                UnitValue::Vh(0.025),
                containing.width(),
                ctx.viewport_w,
                ctx.viewport_h,
            );
            let pad = resolve_len(
                style.padding,
                UnitValue::Vh(0.0333),
                containing.width(),
                ctx.viewport_w,
                ctx.viewport_h,
            );
            let btn_txt = node.text.as_deref().unwrap_or("");
            let text_w = measure_text_width(btn_txt, font_size, get_font());
            let btn_width = (text_w + pad * 2.0).min(max_width);
            let btn_height = match style.height {
                Some(h) => resolve_unit(h, containing.height(), ctx.viewport_w, ctx.viewport_h)
                    .unwrap_or(font_size + pad * 2.0),
                None => font_size + pad * 2.0,
            };
            // The button rect is the full button area (for background, borders, hit testing).
            // Text centering is handled by the renderer.
            let rect = Rect::from_xywh(flow_x, start_y, btn_width, btn_height)
                .unwrap_or_else(|| Rect::from_xywh(flow_x, start_y, btn_width, 40.0).unwrap());
            *y += btn_height + 12.0;
            rect
        }
        Tag::Input => {
            // Inputs show their live `value` (or `placeholder` in gray). They
            // respect `font-size`/`padding`/`width`; the default is a compact
            // box that fills available width when space allows, with a minimum
            // so empty inputs stay clickable.
            let pad = resolve_len(
                style.padding,
                UnitValue::Vh(0.012),
                containing.width(),
                ctx.viewport_w,
                ctx.viewport_h,
            );
            let value = node.attributes.get("value").cloned().unwrap_or_default();
            let font_size = resolve_len(
                style.font_size,
                UnitValue::Vh(0.025),
                containing.width(),
                ctx.viewport_w,
                ctx.viewport_h,
            );
            let text_w = measure_text_width(&value, font_size, get_font());
            let explicit = style.width.and_then(|uv| {
                resolve_unit(uv, containing.width(), ctx.viewport_w, ctx.viewport_h)
            });
            let input_width = explicit
                .unwrap_or_else(|| (text_w + pad * 2.0 + 8.0).max(140.0))
                .min(max_width);
            let input_height = font_size + pad * 2.0;
            let rect = Rect::from_xywh(flow_x, *y, input_width, input_height)
                .unwrap_or_else(|| Rect::from_xywh(flow_x, *y, 140.0, 30.0).unwrap());
            *y += input_height + 8.0;
            rect
        }
        Tag::Icon => {
            // Icons default to the font-size (3.5vh), but honor explicit
            // width / height (e.g. `style="width:1.8vh; height:1.8vh"`) so
            // inline icons fit inside their tab/button boxes.
            let font_size = resolve_len(
                style.font_size,
                UnitValue::Vh(0.035),
                containing.width(),
                ctx.viewport_w,
                ctx.viewport_h,
            );
            let w = style
                .width
                .and_then(|uv| resolve_unit(uv, containing.width(), ctx.viewport_w, ctx.viewport_h))
                .unwrap_or(font_size);
            let h = style
                .height
                .and_then(|uv| {
                    resolve_unit(uv, containing.height(), ctx.viewport_w, ctx.viewport_h)
                })
                .unwrap_or(font_size);
            let rect = Rect::from_xywh(flow_x, *y, w, h)
                .unwrap_or_else(|| Rect::from_xywh(flow_x, *y, 24.0, 24.0).unwrap());
            *y += h + 4.0;
            rect
        }
    };

    // Content height for overflow calculation: the max bottom of all children.
    let content_height = if node.tag == Tag::View {
        // For views, children are laid out starting at padding offset, so their
        // bottom extent relative to the view's top is what matters.
        children
            .iter()
            .map(|c| c.rect.bottom() - start_y)
            .fold(0.0, f32::max)
    } else {
        children
            .iter()
            .map(|c| c.rect.bottom() - start_y)
            .fold(0.0, f32::max)
    };

    LayoutBox {
        node_id: node.id,
        tag: node.tag,
        element_id,
        rect,
        style: style.clone(),
        text: node.text.clone(),
        value: node.attributes.get("value").cloned(),
        placeholder: node.attributes.get("placeholder").cloned(),
        icon: node.attributes.get("icon").cloned(),
        children,
        overflow: style.overflow,
        content_height,
    }
}

/// Resolve the final rect for an out-of-flow box against its containing block.
#[allow(clippy::too_many_arguments)]
fn resolve_oof_rect(
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
fn translate_box(box_node: &mut LayoutBox, dx: f32, dy: f32) {
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
#[cfg(test)]
mod tests {
    use crate::dom::{Dom, DomNode};
    use crate::layout::{build_layout_tree, LayoutBox};
    use pwb_core::spec::Tag;
    use std::collections::HashMap;

    fn node(
        id: usize,
        tag: Tag,
        attrs: HashMap<String, String>,
        text: Option<&str>,
        children: Vec<DomNode>,
    ) -> DomNode {
        DomNode {
            id,
            tag,
            attributes: attrs,
            text: text.map(|s| s.to_string()),
            children,
        }
    }

    fn find(b: &LayoutBox, id: usize) -> Option<&LayoutBox> {
        if b.node_id == id {
            return Some(b);
        }
        b.children.iter().find_map(|c| find(c, id))
    }

    fn dom_with(root: DomNode) -> Dom {
        Dom {
            title: "t".to_string(),
            root,
            components: HashMap::new(),
            modules: HashMap::new(),
        }
    }

    #[test]
    fn flex_row_children_start_in_content_area_not_top() {
        // A flex-row with default view padding must place its children (icons)
        // in the content area — below the top padding — not glued to the row's
        // top edge ("icons always at the top").
        let root = node(
            1,
            Tag::Document,
            HashMap::new(),
            None,
            vec![node(
                2,
                Tag::View,
                HashMap::from([("flex-direction".to_string(), "row".to_string())]),
                None,
                vec![
                    node(
                        3,
                        Tag::Icon,
                        HashMap::from([("icon".to_string(), "home".to_string())]),
                        None,
                        vec![],
                    ),
                    node(
                        4,
                        Tag::Icon,
                        HashMap::from([("icon".to_string(), "star".to_string())]),
                        None,
                        vec![],
                    ),
                ],
            )],
        );
        let layout = build_layout_tree(&dom_with(root), 900, 600, 0.0);
        let row = find(&layout, 2).expect("row in tree");
        let home = find(&layout, 3).expect("home icon in tree");
        let default_pad = 600.0 * 0.0333; // 3.33vh ≈ 20px
                                          // The icon's absolute y is the row's flow position plus the row's top
                                          // padding — it must sit inside the row below the top padding, not glued
                                          // to the top edge of the screen (it used to be left in local coordinates).
        assert!(
            (home.rect.top() - (row.rect.top() + default_pad)).abs() < 2.0,
            "row child sits in the row's content area, got top={} row.top={} padding={:.1}",
            home.rect.top(),
            row.rect.top(),
            default_pad
        );
        assert!(
            home.rect.top() > row.rect.top(),
            "icon is below the row's top edge, not at the screen top"
        );
        assert!(
            home.rect.bottom() <= row.rect.bottom() + 2.0,
            "icon stays within the row bounds (row bottom={}, icon bottom={})",
            row.rect.bottom(),
            home.rect.bottom()
        );
    }

    #[test]
    fn button_and_input_are_inline_sized_not_full_width() {
        // `<button>label</button>` and `<input value="...">` must keep their
        // old-HTML inline sizing (shrink to content) instead of stretching to
        // the container's full width.
        let root = node(
            1,
            Tag::Document,
            HashMap::new(),
            None,
            vec![node(
                2,
                Tag::View,
                HashMap::new(),
                None,
                vec![
                    node(
                        3,
                        Tag::Button,
                        HashMap::new(),
                        Some("Explore Network"),
                        vec![],
                    ),
                    node(
                        4,
                        Tag::Input,
                        HashMap::from([("value".to_string(), "hello".to_string())]),
                        None,
                        vec![],
                    ),
                ],
            )],
        );
        let layout = build_layout_tree(&dom_with(root), 900, 600, 0.0);
        let btn = find(&layout, 3).expect("button in tree");
        let input = find(&layout, 4).expect("input in tree");
        // Content width for the column is ~740px; neither control may take it.
        assert!(
            btn.rect.width() < 300.0,
            "button shrinks to its label, got width {}",
            btn.rect.width()
        );
        assert!(
            input.rect.width() < 300.0,
            "input stays a compact inline field even with a value, got width {}",
            input.rect.width()
        );
    }

    #[test]
    fn fixed_sidebar_is_containing_block_for_its_children() {
        // A `position: fixed` sidebar with an explicit width must lay its
        // subtree out against its own resolved width (not the page's flow
        // width), and its absolutely-positioned bottom-pinned panel must
        // resolve `width: 100%` / `bottom: 0` against the sidebar — this is
        // what keeps the devtools panels confined to the sidebar.
        let sidebar = node(
            2,
            Tag::View,
            HashMap::from([
                ("position".to_string(), "fixed".to_string()),
                ("top".to_string(), "0".to_string()),
                ("right".to_string(), "0".to_string()),
                ("width".to_string(), "25vw".to_string()),
                ("height".to_string(), "100vh".to_string()),
                ("padding".to_string(), "0".to_string()),
            ]),
            None,
            vec![
                node(
                    3,
                    Tag::View,
                    HashMap::from([("height".to_string(), "3vh".to_string())]),
                    None,
                    vec![],
                ),
                node(
                    4,
                    Tag::View,
                    HashMap::from([
                        ("position".to_string(), "absolute".to_string()),
                        ("left".to_string(), "0".to_string()),
                        ("bottom".to_string(), "0".to_string()),
                        ("width".to_string(), "100%".to_string()),
                        ("height".to_string(), "28vh".to_string()),
                    ]),
                    None,
                    vec![],
                ),
            ],
        );
        let root = node(1, Tag::Document, HashMap::new(), None, vec![sidebar]);
        let layout = build_layout_tree(&dom_with(root), 1800, 1300, 0.0);
        let sidebar_box = find(&layout, 2).expect("sidebar in tree");
        let header = find(&layout, 3).expect("header in tree");
        let panel = find(&layout, 4).expect("panel in tree");

        // 25vw of 1800 = 450, anchored at the right edge.
        assert!(
            (sidebar_box.rect.width() - 450.0).abs() < 1.0,
            "sidebar is 25vw = 450 wide, got {}",
            sidebar_box.rect.width()
        );
        // In-flow children are confined to the sidebar's own width.
        assert!(
            header.rect.width() <= sidebar_box.rect.width() + 1.0,
            "header fits inside the sidebar (header {} vs sidebar {})",
            header.rect.width(),
            sidebar_box.rect.width()
        );
        // The absolute panel resolves `width: 100%` and `bottom: 0` against
        // the sidebar, not the viewport.
        assert!(
            (panel.rect.width() - sidebar_box.rect.width()).abs() < 1.0,
            "panel width is 100% of the sidebar, got {}",
            panel.rect.width()
        );
        assert!(
            (panel.rect.bottom() - sidebar_box.rect.bottom()).abs() < 1.0,
            "panel is pinned to the sidebar's bottom edge (panel bottom {} vs sidebar bottom {})",
            panel.rect.bottom(),
            sidebar_box.rect.bottom()
        );
        assert!(
            panel.rect.left() >= sidebar_box.rect.left() - 1.0
                && panel.rect.right() <= sidebar_box.rect.right() + 1.0,
            "panel stays horizontally inside the sidebar"
        );
    }
}

#[cfg(test)]
mod scratchtab {
    use crate::dom::Dom;
    use crate::layout::builder::build_layout_tree;
    #[test]
    fn scratch_tab_dump() {
        let dev_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
        let bundle = crate::bundler::bundle_from_dir(&dev_dir).expect("devtools bundle");
        let dom = Dom::from_bundle(&bundle);
        let root = build_layout_tree(&dom, 1800, 1300, 0.0);
        fn dump(b: &crate::layout::LayoutBox, depth: usize) {
            let r = b.rect;
            println!(
                "{}{} id={} x={:.1} y={:.1} w={:.1} h={:.1}",
                "  ".repeat(depth),
                b.tag.name(),
                b.node_id,
                r.left(),
                r.top(),
                r.width(),
                r.height()
            );
            for c in &b.children {
                dump(c, depth + 1);
            }
        }
        dump(&root, 0);
    }
}
