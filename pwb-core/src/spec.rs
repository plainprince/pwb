//! PlainWeb Language Specification v0 (`pwb-core::spec`).

use std::collections::HashMap;

/// Primitive tag types in PlainWeb Markup v0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Tag {
    Document = 1,
    Head = 2,
    Title = 3,
    View = 4,
    Text = 5,
    Image = 6,
    Button = 7,
    Input = 8,
    Link = 9,
    /// `<script src="...">` / inline `<script>` — bundled by the HTML bundler.
    Script = 10,
    /// `<component name="...">` — a reusable PML component definition.
    Component = 11,
    /// `<import component="...">` — the custom element that inlines a component.
    Import = 12,
    /// `<icon-home>` / `<icon name="home">` — a builtin feather icon.
    Icon = 13,
    /// `<code>` — text rendered with real JS syntax highlighting (strings,
    /// numbers, keywords, comments), unlike HTML's monochrome `<code>`.
    Code = 14,
}

impl Tag {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Tag::Document),
            2 => Some(Tag::Head),
            3 => Some(Tag::Title),
            4 => Some(Tag::View),
            5 => Some(Tag::Text),
            6 => Some(Tag::Image),
            7 => Some(Tag::Button),
            8 => Some(Tag::Input),
            9 => Some(Tag::Link),
            10 => Some(Tag::Script),
            11 => Some(Tag::Component),
            12 => Some(Tag::Import),
            13 => Some(Tag::Icon),
            14 => Some(Tag::Code),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Tag::Document => "plain",
            Tag::Head => "head",
            Tag::Title => "title",
            Tag::View => "view",
            Tag::Text => "text",
            Tag::Image => "image",
            Tag::Button => "button",
            Tag::Script => "script",
            Tag::Component => "component",
            Tag::Import => "import",
            Tag::Input => "input",
            Tag::Link => "link",
            Tag::Icon => "icon",
            Tag::Code => "code",
        }
    }
}
impl std::str::FromStr for Tag {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "plain" => Ok(Tag::Document),
            "head" => Ok(Tag::Head),
            "title" => Ok(Tag::Title),
            "view" => Ok(Tag::View),
            "text" => Ok(Tag::Text),
            "image" => Ok(Tag::Image),
            "button" => Ok(Tag::Button),
            "input" => Ok(Tag::Input),
            "link" => Ok(Tag::Link),
            "icon" => Ok(Tag::Icon),
            "code" => Ok(Tag::Code),
            _ => Err(()),
        }
    }
}

/// Positioning model in PSS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionStyle {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

/// Cursor types supported in PSS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Default,
    Pointer,
    Text,
    Grab,
    Crosshair,
    /// `not-allowed` — the prohibition-sign cursor.
    NotAllowed,
    /// `wait` — the spinning-wait / busy cursor.
    Wait,
    /// `help` — the question-mark cursor.
    Help,
    /// `col-resize` — horizontal two-headed arrow (for column drags).
    ColResize,
    /// `row-resize` — vertical two-headed arrow (for row drags).
    RowResize,
}

/// Draggable-edge resize modes (the PSS `resize` builtin).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResizeStyle {
    #[default]
    None,
    Both,
    Vertical,
    Horizontal,
}

/// Display property modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayStyle {
    Flex,
    Block,
    None,
}

/// Overflow (clipping/scrolling) behavior for a box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverflowStyle {
    /// Content may overflow the box and paints outside it.
    #[default]
    Visible,
    /// Content is clipped to the box; no scrollbar.
    Hidden,
    /// Content is clipped; a scrollbar appears when it overflows.
    Scroll,
    /// Like [`OverflowStyle::Scroll`], but the scrollbar only shows on demand.
    Auto,
}

/// Flex direction for View containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlexDirection {
    Column,
    Row,
}

/// Align items & justify content options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Start,
    Center,
    End,
    SpaceBetween,
}

/// Value unit representation.
///
/// - `Px`: exactly one device pixel (never scaled by device-pixel-ratio/viewport).
/// - `Percent`: fraction of the containing block dimension.
/// - `Vw` / `Vh`: fraction of the actual viewport width/height in device pixels,
///   so `25vw` is always a quarter of the window no matter the display scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnitValue {
    Px(f32),
    Percent(f32),
    Vw(f32),
    Vh(f32),
    Auto,
}

/// Color representation (RGBA u8 0..255).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Self = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    pub const WHITE: Self = Color {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    pub const TRANSPARENT: Self = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
}

/// PlainWeb styling property set.
#[derive(Debug, Clone, PartialEq)]
pub enum StyleProperty {
    Direction(FlexDirection),
    Width(UnitValue),
    Height(UnitValue),
    /// Sizes that support `px`, `%`, `vw`, and `vh` (e.g. `padding: 5vh`).
    Padding(UnitValue),
    Margin(UnitValue),
    Gap(UnitValue),
    AlignItems(Alignment),
    JustifyContent(Alignment),
    BackgroundColor(Color),
    Color(Color),
    BorderWidth(f32),
    BorderColor(Color),
    BorderRadius(f32),
    /// Font size as a unit value (e.g. `font-size: 4.7vh`).
    FontSize(UnitValue),
    FontWeight(u16),
    Position(PositionStyle),
    Top(f32),
    Left(f32),
    Right(f32),
    Bottom(f32),
    Cursor(CursorStyle),
    Display(DisplayStyle),
    Resize(ResizeStyle),
    /// Clipping/scrolling behavior (`overflow: visible|hidden|scroll|auto`).
    Overflow(OverflowStyle),
}

/// PlainWeb Node (AST element tree node).
#[derive(Debug, Clone, PartialEq)]
pub struct PlainNode {
    pub id: usize,
    pub tag: Tag,
    pub attributes: HashMap<String, String>,
    pub styles: Vec<StyleProperty>,
    pub text_content: Option<String>,
    pub children: Vec<PlainNode>,
}

impl PlainNode {
    pub fn new(id: usize, tag: Tag) -> Self {
        Self {
            id,
            tag,
            attributes: HashMap::new(),
            styles: Vec::new(),
            text_content: None,
            children: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
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
}
