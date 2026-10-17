//! The computed style of a DOM node.

use pwb_core::spec::{
    Alignment, Color, CursorStyle, DisplayStyle, FlexDirection, OverflowStyle, PositionStyle,
    ResizeStyle, UnitValue,
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
