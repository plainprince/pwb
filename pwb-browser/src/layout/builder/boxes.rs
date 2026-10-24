//! Box construction: in-flow sizing and the main `build_box` pass.

use super::positioning::{layout_node, resolve_oof_rect, translate_box};
use crate::dom::DomNode;
use crate::layout::box_types::{get_font, LayoutBox, LayoutCtx};
use crate::layout::text::{measure_text_width, resolve_clamped_width, resolve_len, resolve_unit};
use crate::style::{compute_style, ComputedStyle};
use pwb_core::spec::{Alignment, DisplayStyle, FlexDirection, PositionStyle, Tag, UnitValue};
use tiny_skia::Rect;

fn is_positioned(style: &ComputedStyle) -> bool {
    style.position != PositionStyle::Static
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
pub(super) fn build_box(
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
            // A row's explicit `height` (a tab bar, a toolbar) is the height
            // every child is centered against and the height the flow advances
            // by. Without it a 32px row advances by its 21px button and the
            // next row overlaps the text.
            let row_height = explicit_height.or_else(|| {
                style.height.and_then(|uv| {
                    resolve_unit(uv, containing.height(), ctx.viewport_w, ctx.viewport_h)
                })
            });
            // Vertically center children within the row's inner content height,
            // but only when asked. `align-items: start` (the default) must keep
            // children at the top of the row.
            if style.align_items == Alignment::Center {
                let inner_h = row_height
                    .map(|h| (h - padding * 2.0).max(0.0))
                    .unwrap_or(row_max_h)
                    .max(row_max_h);
                for b in &mut children {
                    let dy = (inner_h - b.rect.height()) / 2.0;
                    if dy > 0.0 {
                        translate_box(b, 0.0, dy);
                    }
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
            // Advance by the box's real height: explicit height when present,
            // otherwise padding + tallest child (the View rect adds the bottom
            // padding below).
            *y = start_y + row_height.unwrap_or(padding + row_max_h);
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
        Tag::Document | Tag::Head | Tag::Title | Tag::Script | Tag::Component | Tag::Import => {
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
                    // An explicit height is the whole box; adding padding here
                    // moved the next sibling past the drawn box and opened a
                    // gap the size of the view's padding.
                    *y = start_y + h;
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
            // A link is inline text and must not reserve box padding: the old
            // 3.33vh default indented every link and stretched its hit area and
            // underline.
            let font_size = resolve_len(
                style.font_size,
                UnitValue::Vh(0.025),
                containing.width(),
                ctx.viewport_w,
                ctx.viewport_h,
            );
            let pad_default = if node.tag == Tag::Link {
                UnitValue::Px(0.0)
            } else if node.tag == Tag::Code {
                UnitValue::Px(4.0)
            } else {
                UnitValue::Vh(0.0333)
            };
            let pad = resolve_len(
                style.padding,
                pad_default,
                containing.width(),
                ctx.viewport_w,
                ctx.viewport_h,
            );
            let btn_txt = node.text.as_deref().unwrap_or("");
            let text_w = measure_text_width(btn_txt, font_size, get_font());
            // An explicit width (a form-style button) wins over the label size.
            let explicit = style.width.and_then(|uv| {
                resolve_unit(uv, containing.width(), ctx.viewport_w, ctx.viewport_h)
            });
            let btn_width = explicit.unwrap_or(text_w + pad * 2.0).clamp(1.0, max_width);
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
            // Size to whichever is longer: the value, or the placeholder shown
            // while the field is empty. Sizing only from `value` clipped every
            // placeholder to the 140px minimum.
            let placeholder = node
                .attributes
                .get("placeholder")
                .map(String::as_str)
                .unwrap_or("");
            let text_w = measure_text_width(&value, font_size, get_font()).max(measure_text_width(
                placeholder,
                font_size,
                get_font(),
            ));
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
        Tag::Image => {
            // Images honor pixel `width`/`height` attributes first, then CSS
            // lengths, and otherwise take a 320x180 default (never wider than
            // the content box). The renderer scales the decoded PNG to fill
            // the rect.
            let attr_px = |name: &str| {
                node.attributes
                    .get(name)
                    .and_then(|v| v.trim().parse::<f32>().ok())
            };
            let w = attr_px("width")
                .or_else(|| {
                    style.width.and_then(|uv| {
                        resolve_unit(uv, containing.width(), ctx.viewport_w, ctx.viewport_h)
                    })
                })
                .unwrap_or(max_width.min(320.0))
                .clamp(1.0, max_width);
            let h = attr_px("height")
                .or_else(|| {
                    style.height.and_then(|uv| {
                        resolve_unit(uv, containing.height(), ctx.viewport_w, ctx.viewport_h)
                    })
                })
                .unwrap_or(180.0)
                .max(1.0);
            let rect = Rect::from_xywh(flow_x, *y, w, h)
                .unwrap_or_else(|| Rect::from_xywh(flow_x, *y, w, 1.0).unwrap());
            *y += h + 8.0;
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
        image_src: node.attributes.get("src").cloned(),
        children,
        overflow: style.overflow,
        content_height,
    }
}
