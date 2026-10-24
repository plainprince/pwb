//! PSS styling model: enums, units, colors, and the property set.

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
    /// A property passed through verbatim as `(name, value)`.
    ///
    /// The runtime style engine understands more attributes than this typed
    /// enum enumerates (`border-top`, `min-width`, `flex`, …). Compiling them
    /// as raw attributes keeps authored styling from being silently dropped
    /// at bundle time.
    Raw(String, String),
}
