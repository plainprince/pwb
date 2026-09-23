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
