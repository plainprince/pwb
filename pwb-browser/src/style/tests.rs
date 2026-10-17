use super::*;
use crate::dom::DomNode;
use pwb_core::spec::{
    Alignment, Color, CursorStyle, DisplayStyle, FlexDirection, OverflowStyle, PositionStyle,
    ResizeStyle, Tag, UnitValue,
};
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
