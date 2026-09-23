//! CSS layout and style matching (`pwb-browser::style`).

use crate::dom::DomNode;
use pwb_core::spec::{
    Alignment, Color, CursorStyle, DisplayStyle, FlexDirection, OverflowStyle, PositionStyle,
    ResizeStyle, Tag, UnitValue,
};

#[derive(Debug, Clone)]
pub struct ComputedStyle {
    pub background_color: Color,
    pub text_color: Color,
    /// Explicit padding. `None` means "unset" → views fall back to their default
    /// so that `padding: 0` is honored. Supports `px`, `%`, `vw`, `vh`.
    pub padding: Option<UnitValue>,
    /// Font size. `None` → the layout's viewport-relative default (3vh).
    pub font_size: Option<UnitValue>,
    pub border_radius: f32,
    /// Border width per side (exact pixels); 0 = no border on that side. Drawn
    /// on the box rect edges by the renderer.
    pub border_top: f32,
    pub border_right: f32,
    pub border_bottom: f32,
    pub border_left: f32,
    pub border_color: Color,
    pub position: PositionStyle,
    pub top: Option<f32>,
    pub left: Option<f32>,
    pub right: Option<f32>,
    pub bottom: Option<f32>,
    pub width: Option<UnitValue>,
    pub height: Option<UnitValue>,
    /// `min-width`/`max-width` clamps applied when resolving the box width
    /// (layout + the `resize` drag both honor them).
    pub min_width: Option<UnitValue>,
    pub max_width: Option<UnitValue>,
    pub cursor: Option<CursorStyle>,
    pub display: DisplayStyle,
    pub flex_direction: FlexDirection,
    /// Flex gap between children. Supports `px`, `%`, `vw`, `vh`.
    pub gap: Option<UnitValue>,
    pub justify_content: Alignment,
    pub align_items: Alignment,
    pub resize: ResizeStyle,
    /// Clipping/scrolling behavior. `Scroll`/`Auto` make the box a scroll
    /// container (content is clipped, a scrollbar shows when it overflows).
    pub overflow: OverflowStyle,
}

impl Default for ComputedStyle {
    fn default() -> Self {
        Self {
            background_color: Color::TRANSPARENT,
            text_color: Color::WHITE,
            padding: None,
            font_size: None,
            border_radius: 0.0,
            border_top: 0.0,
            border_right: 0.0,
            border_bottom: 0.0,
            border_left: 0.0,
            border_color: Color::TRANSPARENT,
            position: PositionStyle::Static,
            top: None,
            left: None,
            right: None,
            bottom: None,
            width: None,
            height: None,
            min_width: None,
            max_width: None,
            cursor: None,
            display: DisplayStyle::Block,
            flex_direction: FlexDirection::Column,
            gap: None,
            justify_content: Alignment::Start,
            align_items: Alignment::Start,
            resize: ResizeStyle::None,
            overflow: OverflowStyle::Visible,
        }
    }
}

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

/// Parse `border: <width> [solid] <color>` (width and color both optional).
fn parse_border_shorthand(v: &str) -> (Option<f32>, Option<Color>) {
    let mut width = None;
    let mut color = None;
    for token in v.split_whitespace() {
        if token == "solid" || token == "dashed" || token == "dotted" {
            continue;
        }
        if let Some(px) = parse_px(token) {
            width = Some(px);
        } else if let Some(c) = parse_color(token) {
            color = Some(c);
        }
    }
    (width, color)
}

/// Apply `border-<side>: <width> [solid] <color>` to one side's width + color.
fn set_border_side(width: &mut f32, color: &mut Color, v: &str) {
    let (w, c) = parse_border_shorthand(v);
    if let Some(w) = w {
        *width = w;
    }
    if let Some(c) = c {
        *color = c;
    }
}

fn parse_alignment(v: &str) -> Option<Alignment> {
    match v {
        "center" => Some(Alignment::Center),
        "flex-start" | "start" => Some(Alignment::Start),
        "flex-end" | "end" => Some(Alignment::End),
        "space-between" => Some(Alignment::SpaceBetween),
        _ => None,
    }
}

fn parse_px(v: &str) -> Option<f32> {
    if let Some(percent) = v.strip_suffix('%') {
        return percent.trim().parse::<f32>().ok();
    }
    v.replace("px", "").trim().parse::<f32>().ok()
}

fn parse_unit_value(v: &str) -> Option<UnitValue> {
    if v == "auto" {
        return Some(UnitValue::Auto);
    }
    if let Some(percent) = v.strip_suffix('%') {
        if let Ok(val) = percent.trim().parse::<f32>() {
            return Some(UnitValue::Percent(val / 100.0));
        }
    }
    if let Some(rest) = v.strip_suffix("vw") {
        if let Ok(val) = rest.trim().parse::<f32>() {
            return Some(UnitValue::Vw(val / 100.0));
        }
    }
    if let Some(rest) = v.strip_suffix("vh") {
        if let Ok(val) = rest.trim().parse::<f32>() {
            return Some(UnitValue::Vh(val / 100.0));
        }
    }
    if let Some(px) = parse_px(v) {
        return Some(UnitValue::Px(px));
    }
    None
}

fn parse_color(v: &str) -> Option<Color> {
    let clean = v.trim().trim_start_matches('#');
    let named = |r: u8, g: u8, b: u8| Some(Color { r, g, b, a: 255 });
    match clean {
        "white" => return Some(Color::WHITE),
        "black" => return Some(Color::BLACK),
        "transparent" => return Some(Color::TRANSPARENT),
        "red" => return named(255, 0, 0),
        "green" => return named(0, 128, 0),
        "lime" => return named(0, 255, 0),
        "blue" => return named(0, 0, 255),
        "gray" | "grey" => return named(128, 128, 128),
        "yellow" => return named(255, 255, 0),
        "orange" => return named(255, 165, 0),
        "purple" => return named(128, 0, 128),
        _ => {}
    }
    if clean.len() == 6 {
        let r = u8::from_str_radix(&clean[0..2], 16).ok()?;
        let g = u8::from_str_radix(&clean[2..4], 16).ok()?;
        let b = u8::from_str_radix(&clean[4..6], 16).ok()?;
        return Some(Color { r, g, b, a: 255 });
    }
    if clean.len() == 3 {
        let r = u8::from_str_radix(&clean[0..1], 16).ok()?;
        let g = u8::from_str_radix(&clean[1..2], 16).ok()?;
        let b = u8::from_str_radix(&clean[2..3], 16).ok()?;
        return Some(Color {
            r: r * 17,
            g: g * 17,
            b: b * 17,
            a: 255,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn node_with_attrs(attrs: &[(&str, &str)]) -> DomNode {
        let mut attributes = HashMap::new();
        for (k, v) in attrs {
            attributes.insert(k.to_string(), v.to_string());
        }
        DomNode {
            id: 1,
            tag: Tag::View,
            attributes,
            text: None,
            children: vec![],
        }
    }

    #[test]
    fn test_position_modes() {
        for (pos, expected) in [
            ("static", PositionStyle::Static),
            ("relative", PositionStyle::Relative),
            ("absolute", PositionStyle::Absolute),
            ("fixed", PositionStyle::Fixed),
            ("sticky", PositionStyle::Sticky),
        ] {
            let node = node_with_attrs(&[("position", pos)]);
            assert_eq!(compute_style(&node).position, expected, "position: {}", pos);
        }
    }

    #[test]
    fn test_overflow_modes() {
        for (val, expected) in [
            ("visible", OverflowStyle::Visible),
            ("hidden", OverflowStyle::Hidden),
            ("scroll", OverflowStyle::Scroll),
            ("auto", OverflowStyle::Auto),
            ("garbage", OverflowStyle::Visible),
        ] {
            let node = node_with_attrs(&[("overflow", val)]);
            assert_eq!(compute_style(&node).overflow, expected, "overflow: {}", val);
        }
    }

    #[test]
    fn test_offsets_and_size() {
        let node = node_with_attrs(&[
            ("position", "fixed"),
            ("top", "0"),
            ("right", "0"),
            ("width", "400px"),
            ("height", "100%"),
        ]);
        let style = compute_style(&node);
        assert_eq!(style.position, PositionStyle::Fixed);
        assert_eq!(style.top, Some(0.0));
        assert_eq!(style.right, Some(0.0));
        assert_eq!(style.width, Some(UnitValue::Px(400.0)));
        assert_eq!(style.height, Some(UnitValue::Percent(1.0)));
    }

    #[test]
    fn test_inline_style_attribute() {
        let node = node_with_attrs(&[(
            "style",
            "background-color: #0f172a; color: #f8fafc; padding: 40px;",
        )]);
        let style = compute_style(&node);
        assert_eq!(
            style.background_color,
            Color {
                r: 15,
                g: 23,
                b: 42,
                a: 255
            }
        );
        assert_eq!(
            style.text_color,
            Color {
                r: 248,
                g: 250,
                b: 252,
                a: 255
            }
        );
        assert_eq!(style.padding, Some(UnitValue::Px(40.0)));
    }

    #[test]
    fn test_padding_zero_is_explicit() {
        let node = node_with_attrs(&[("padding", "0")]);
        assert_eq!(compute_style(&node).padding, Some(UnitValue::Px(0.0)));
    }

    #[test]
    fn test_font_size_and_gap_units() {
        let node = node_with_attrs(&[("font-size", "2vh"), ("padding", "5vh"), ("gap", "0.5vh")]);
        let style = compute_style(&node);
        assert_eq!(style.font_size, Some(UnitValue::Vh(0.02)));
        assert_eq!(style.padding, Some(UnitValue::Vh(0.05)));
        assert_eq!(style.gap, Some(UnitValue::Vh(0.005)));
    }

    #[test]
    fn test_vw_vh_unit_parsing() {
        let node = node_with_attrs(&[("width", "25vw"), ("height", "50vh")]);
        let style = compute_style(&node);
        assert_eq!(style.width, Some(UnitValue::Vw(0.25)));
        assert_eq!(style.height, Some(UnitValue::Vh(0.5)));
        let px = node_with_attrs(&[("width", "400px")]);
        assert_eq!(compute_style(&px).width, Some(UnitValue::Px(400.0)));
    }

    #[test]
    fn test_flex_and_resize_parsing() {
        let node = node_with_attrs(&[
            ("flex-direction", "row"),
            ("gap", "4px"),
            ("justify-content", "space-between"),
            ("align-items", "center"),
            ("resize", "horizontal"),
        ]);
        let style = compute_style(&node);
        assert_eq!(style.flex_direction, FlexDirection::Row);
        assert_eq!(style.gap, Some(UnitValue::Px(4.0)));
        assert_eq!(style.justify_content, Alignment::SpaceBetween);
        assert_eq!(style.align_items, Alignment::Center);
        assert_eq!(style.resize, ResizeStyle::Horizontal);
    }

    #[test]
    fn test_display_none() {
        let node = node_with_attrs(&[("display", "none")]);
        assert_eq!(compute_style(&node).display, DisplayStyle::None);
    }

    #[test]
    fn test_border_shorthand_and_sides() {
        let node = node_with_attrs(&[("border", "1px solid #3e3e42")]);
        let style = compute_style(&node);
        assert_eq!(style.border_top, 1.0);
        assert_eq!(style.border_right, 1.0);
        assert_eq!(style.border_bottom, 1.0);
        assert_eq!(style.border_left, 1.0);
        assert_eq!(
            style.border_color,
            Color {
                r: 0x3e,
                g: 0x3e,
                b: 0x42,
                a: 255
            }
        );

        // A side-specific declaration only touches that side.
        let node = node_with_attrs(&[("border-bottom", "2px solid red")]);
        let style = compute_style(&node);
        assert_eq!(style.border_bottom, 2.0);
        assert_eq!(style.border_top, 0.0);
        assert_eq!(
            style.border_color,
            Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            }
        );

        // `border-width`/`border-color` split forms.
        let node = node_with_attrs(&[("border-width", "3px"), ("border-color", "#ff0000")]);
        let style = compute_style(&node);
        assert_eq!(style.border_left, 3.0);
        assert_eq!(
            style.border_color,
            Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            }
        );
    }

    #[test]
    fn test_min_max_width_parsing() {
        let node = node_with_attrs(&[("min-width", "15vw"), ("max-width", "50vw")]);
        let style = compute_style(&node);
        assert_eq!(style.min_width, Some(UnitValue::Vw(0.15)));
        assert_eq!(style.max_width, Some(UnitValue::Vw(0.5)));
        let px = node_with_attrs(&[("min-width", "200px")]);
        assert_eq!(compute_style(&px).min_width, Some(UnitValue::Px(200.0)));
    }

    #[test]
    fn test_all_cursor_styles() {
        for (val, expected) in [
            ("default", CursorStyle::Default),
            ("pointer", CursorStyle::Pointer),
            ("text", CursorStyle::Text),
            ("grab", CursorStyle::Grab),
            ("crosshair", CursorStyle::Crosshair),
            ("not-allowed", CursorStyle::NotAllowed),
            ("wait", CursorStyle::Wait),
            ("help", CursorStyle::Help),
            ("col-resize", CursorStyle::ColResize),
            ("row-resize", CursorStyle::RowResize),
        ] {
            let node = node_with_attrs(&[("cursor", val)]);
            assert_eq!(
                compute_style(&node).cursor,
                Some(expected),
                "cursor: {}",
                val
            );
        }
        // Unknown cursor value → None (falls back to Default).
        let node = node_with_attrs(&[("cursor", "bogus")]);
        assert_eq!(compute_style(&node).cursor, None);
    }
}
