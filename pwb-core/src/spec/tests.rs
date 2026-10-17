use super::*;
use std::str::FromStr;

#[test]
fn test_tag_from_u8_roundtrips_all_variants() {
    let all = [
        Tag::Document,
        Tag::Head,
        Tag::Title,
        Tag::View,
        Tag::Text,
        Tag::Image,
        Tag::Button,
        Tag::Input,
        Tag::Link,
        Tag::Script,
        Tag::Component,
        Tag::Import,
        Tag::Icon,
        Tag::Code,
    ];
    for (i, tag) in all.iter().enumerate() {
        assert_eq!(Tag::from_u8((i + 1) as u8), Some(*tag));
        assert!(!tag.name().is_empty());
    }
    assert_eq!(Tag::from_u8(0), None);
    assert_eq!(Tag::from_u8(15), None);
    assert_eq!(Tag::from_u8(255), None);
}

#[test]
fn test_tag_names_are_stable() {
    assert_eq!(Tag::Document.name(), "plain");
    assert_eq!(Tag::Head.name(), "head");
    assert_eq!(Tag::Title.name(), "title");
    assert_eq!(Tag::View.name(), "view");
    assert_eq!(Tag::Text.name(), "text");
    assert_eq!(Tag::Image.name(), "image");
    assert_eq!(Tag::Button.name(), "button");
    assert_eq!(Tag::Input.name(), "input");
    assert_eq!(Tag::Link.name(), "link");
    assert_eq!(Tag::Script.name(), "script");
    assert_eq!(Tag::Component.name(), "component");
    assert_eq!(Tag::Import.name(), "import");
    assert_eq!(Tag::Icon.name(), "icon");
    assert_eq!(Tag::Code.name(), "code");
}

#[test]
fn test_tag_from_str_parses_known_names_and_rejects_unknown() {
    for name in [
        "plain", "head", "title", "view", "text", "image", "button", "input", "link", "icon",
        "code",
    ] {
        assert!(Tag::from_str(name).is_ok(), "should parse {name}");
    }
    assert_eq!(Tag::from_str("plain").unwrap(), Tag::Document);
    assert_eq!(Tag::from_str("code").unwrap(), Tag::Code);
    assert!(Tag::from_str("div").is_err());
    assert!(Tag::from_str("").is_err());
    assert!(Tag::from_str("SCRIPT").is_err());
}

#[test]
fn test_style_enums_cover_every_variant() {
    assert_eq!(PositionStyle::Static as u8, 0);
    let _ = PositionStyle::Relative;
    let _ = PositionStyle::Absolute;
    let _ = PositionStyle::Fixed;
    let _ = PositionStyle::Sticky;

    assert_eq!(CursorStyle::Default as u8, 0);
    let _ = CursorStyle::Pointer;
    let _ = CursorStyle::Text;
    let _ = CursorStyle::Grab;
    let _ = CursorStyle::Crosshair;
    let _ = CursorStyle::NotAllowed;
    let _ = CursorStyle::Wait;
    let _ = CursorStyle::Help;
    let _ = CursorStyle::ColResize;
    let _ = CursorStyle::RowResize;

    assert_eq!(ResizeStyle::default(), ResizeStyle::None);
    let _ = ResizeStyle::Both;
    let _ = ResizeStyle::Vertical;
    let _ = ResizeStyle::Horizontal;

    let _ = DisplayStyle::Flex;
    let _ = DisplayStyle::Block;
    let _ = DisplayStyle::None;

    assert_eq!(OverflowStyle::default(), OverflowStyle::Visible);
    let _ = OverflowStyle::Hidden;
    let _ = OverflowStyle::Scroll;
    let _ = OverflowStyle::Auto;

    let _ = FlexDirection::Column;
    let _ = FlexDirection::Row;

    for align in [
        Alignment::Start,
        Alignment::Center,
        Alignment::End,
        Alignment::SpaceBetween,
    ] {
        let _ = align;
    }
}

#[test]
fn test_unit_values_and_colors() {
    assert_eq!(UnitValue::Px(10.0), UnitValue::Px(10.0));
    assert_ne!(UnitValue::Px(10.0), UnitValue::Percent(10.0));
    let _ = UnitValue::Vw(25.0);
    let _ = UnitValue::Vh(25.0);
    let _ = UnitValue::Auto;

    assert_eq!(Color::BLACK.a, 255);
    assert_eq!(Color::WHITE.r, 255);
    assert_eq!(Color::TRANSPARENT.a, 0);
    let custom = Color {
        r: 1,
        g: 2,
        b: 3,
        a: 4,
    };
    assert_eq!(
        custom,
        Color {
            r: 1,
            g: 2,
            b: 3,
            a: 4
        }
    );
    assert_ne!(custom, Color::BLACK);
}

#[test]
fn test_style_property_variants_construct() {
    let props = vec![
        StyleProperty::Direction(FlexDirection::Row),
        StyleProperty::Width(UnitValue::Px(100.0)),
        StyleProperty::Height(UnitValue::Percent(50.0)),
        StyleProperty::Padding(UnitValue::Vh(5.0)),
        StyleProperty::Margin(UnitValue::Vw(5.0)),
        StyleProperty::Gap(UnitValue::Auto),
        StyleProperty::AlignItems(Alignment::Center),
        StyleProperty::JustifyContent(Alignment::SpaceBetween),
        StyleProperty::BackgroundColor(Color::WHITE),
        StyleProperty::Color(Color::BLACK),
        StyleProperty::BorderWidth(2.0),
        StyleProperty::BorderColor(Color::TRANSPARENT),
        StyleProperty::BorderRadius(4.0),
        StyleProperty::FontSize(UnitValue::Px(16.0)),
        StyleProperty::FontWeight(700),
        StyleProperty::Position(PositionStyle::Absolute),
        StyleProperty::Top(1.0),
        StyleProperty::Left(2.0),
        StyleProperty::Right(3.0),
        StyleProperty::Bottom(4.0),
        StyleProperty::Cursor(CursorStyle::Pointer),
        StyleProperty::Display(DisplayStyle::Flex),
        StyleProperty::Resize(ResizeStyle::Both),
        StyleProperty::Overflow(OverflowStyle::Auto),
    ];
    assert_eq!(props.len(), 24);
    assert_eq!(props[0], props[0]);
}

#[test]
fn test_plain_node_new_defaults() {
    let node = PlainNode::new(42, Tag::View);
    assert_eq!(node.id, 42);
    assert_eq!(node.tag, Tag::View);
    assert!(node.attributes.is_empty());
    assert!(node.styles.is_empty());
    assert!(node.text_content.is_none());
    assert!(node.children.is_empty());
    assert_eq!(node, PlainNode::new(42, Tag::View));
}
