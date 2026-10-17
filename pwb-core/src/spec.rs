//! PlainWeb Language Specification v0 (`pwb-core::spec`).

mod node;
mod style;
mod tag;

#[cfg(test)]
mod tests;

pub use node::PlainNode;
pub use style::{
    Alignment, Color, CursorStyle, DisplayStyle, FlexDirection, OverflowStyle, PositionStyle,
    ResizeStyle, StyleProperty, UnitValue,
};
pub use tag::Tag;
