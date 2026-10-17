//! Tag defaults and attribute/style matching into a `ComputedStyle`.

use crate::dom::DomNode;
use crate::style::computed::ComputedStyle;
use crate::style::values::{
    parse_alignment, parse_border_shorthand, parse_color, parse_px, parse_unit_value,
    set_border_side,
};
use pwb_core::spec::{
    CursorStyle, DisplayStyle, FlexDirection, OverflowStyle, PositionStyle, ResizeStyle, Tag,
};

pub fn compute_style(node: &DomNode) -> ComputedStyle {
    let mut style = ComputedStyle::default();

    // Tag defaults
    match node.tag {
        Tag::View | Tag::Document | Tag::Head | Tag::Title => {
            // View containers and document structure elements are flex by default
            style.display = DisplayStyle::Flex;
        }
        Tag::Button | Tag::Link | Tag::Input | Tag::Text | Tag::Icon | Tag::Image | Tag::Code => {
            // Buttons, links, inputs, text, icons, images, and code flow inline (Block layout but inline flow)
            if matches!(node.tag, Tag::Button | Tag::Link) {
                style.cursor = Some(CursorStyle::Pointer);
            } else if matches!(node.tag, Tag::Input | Tag::Text) {
                style.cursor = Some(CursorStyle::Text);
            }
            style.display = DisplayStyle::Block;
        }
        _ => {}
    }

    // First inline style="..." declarations, then individual attributes win.
    if let Some(style_attr) = node.attributes.get("style") {
        for (k, v) in parse_inline_style(style_attr) {
            apply_style_attr(&mut style, &k, &v);
        }
    }
    for (k, v) in &node.attributes {
        apply_style_attr(&mut style, k, v);
    }

    style
}

/// Parse a CSS-ish `style="key: value; key: value"` attribute.
fn parse_inline_style(style_attr: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for decl in style_attr.split(';') {
        if let Some((k, v)) = decl.split_once(':') {
            out.push((k.trim().to_lowercase(), v.trim().to_string()));
        }
    }
    out
}

fn apply_style_attr(style: &mut ComputedStyle, key: &str, value: &str) {
    let v = value.trim();
    match key {
        "background-color" | "background" => {
            if let Some(color) = parse_color(v) {
                style.background_color = color;
            }
        }
        "color" => {
            if let Some(color) = parse_color(v) {
                style.text_color = color;
            }
        }
        "padding" => {
            // Only single-value paddings are supported.
            if let Some(uv) = parse_unit_value(v) {
                style.padding = Some(uv);
            }
        }
        "font-size" => {
            if let Some(uv) = parse_unit_value(v) {
                style.font_size = Some(uv);
            }
        }
        "border-radius" => {
            if let Some(px) = parse_px(v) {
                style.border_radius = px;
            }
        }
        "position" => {
            style.position = match v {
                "absolute" => PositionStyle::Absolute,
                "relative" => PositionStyle::Relative,
                "fixed" => PositionStyle::Fixed,
                "sticky" => PositionStyle::Sticky,
                _ => PositionStyle::Static,
            };
        }
        "top" => style.top = parse_px(v),
        "left" => style.left = parse_px(v),
        "right" => style.right = parse_px(v),
        "bottom" => style.bottom = parse_px(v),
        "width" => style.width = parse_unit_value(v),
        "height" => style.height = parse_unit_value(v),
        "min-width" => style.min_width = parse_unit_value(v),
        "max-width" => style.max_width = parse_unit_value(v),
        "border" => {
            // `border: 1px solid #3e3e42` — width + color (all sides). Also
            // accepts a bare width or a bare color.
            let (w, c) = parse_border_shorthand(v);
            if let Some(w) = w {
                style.border_top = w;
                style.border_right = w;
                style.border_bottom = w;
                style.border_left = w;
            }
            if let Some(c) = c {
                style.border_color = c;
            }
        }
        "border-width" => {
            if let Some(px) = parse_px(v) {
                style.border_top = px;
                style.border_right = px;
                style.border_bottom = px;
                style.border_left = px;
            }
        }
        "border-color" => {
            if let Some(c) = parse_color(v) {
                style.border_color = c;
            }
        }
        "border-top" => set_border_side(&mut style.border_top, &mut style.border_color, v),
        "border-right" => set_border_side(&mut style.border_right, &mut style.border_color, v),
        "border-bottom" => set_border_side(&mut style.border_bottom, &mut style.border_color, v),
        "border-left" => set_border_side(&mut style.border_left, &mut style.border_color, v),
        "cursor" => {
            style.cursor = match v {
                "pointer" => Some(CursorStyle::Pointer),
                "text" => Some(CursorStyle::Text),
                "grab" => Some(CursorStyle::Grab),
                "crosshair" => Some(CursorStyle::Crosshair),
                "default" => Some(CursorStyle::Default),
                "not-allowed" => Some(CursorStyle::NotAllowed),
                "wait" => Some(CursorStyle::Wait),
                "help" => Some(CursorStyle::Help),
                "col-resize" => Some(CursorStyle::ColResize),
                "row-resize" => Some(CursorStyle::RowResize),
                _ => None,
            };
        }
        "display" => {
            style.display = match v {
                "block" => DisplayStyle::Block,
                "none" => DisplayStyle::None,
                _ => DisplayStyle::Flex,
            };
        }
        "flex-direction" => {
            style.flex_direction = match v {
                "row" => FlexDirection::Row,
                _ => FlexDirection::Column,
            };
        }
        "gap" => {
            if let Some(uv) = parse_unit_value(v) {
                style.gap = Some(uv);
            }
        }
        "justify-content" => {
            if let Some(a) = parse_alignment(v) {
                style.justify_content = a;
            }
        }
        "align-items" => {
            if let Some(a) = parse_alignment(v) {
                style.align_items = a;
            }
        }
        "overflow" => {
            style.overflow = match v {
                "hidden" => OverflowStyle::Hidden,
                "scroll" => OverflowStyle::Scroll,
                "auto" => OverflowStyle::Auto,
                _ => OverflowStyle::Visible,
            };
        }
        "resize" => {
            style.resize = match v {
                "both" => ResizeStyle::Both,
                "vertical" => ResizeStyle::Vertical,
                "horizontal" => ResizeStyle::Horizontal,
                _ => ResizeStyle::None,
            };
        }
        _ => {}
    }
}
