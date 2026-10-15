//! Core layout box types and context.

use crate::style::ComputedStyle;
use pwb_core::spec::{OverflowStyle, Tag};
use std::sync::OnceLock;
use tiny_skia::Rect;

static SYSTEM_FONT: OnceLock<Option<ab_glyph::FontArc>> = OnceLock::new();

/// System font lookup (lazy, one-time).
pub fn get_font() -> Option<&'static ab_glyph::FontArc> {
    SYSTEM_FONT
        .get_or_init(|| {
            let font_paths = [
                "/System/Library/Fonts/Supplemental/Arial.ttf",
                "/System/Library/Fonts/Helvetica.ttc",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                // Liberation Sans is metric-compatible with Arial and ships on
                // most Linux distros (Fedora, Debian/Ubuntu non-Noto setups).
                "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
                "/usr/share/fonts/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
                "C:\\Windows\\Fonts\\arial.ttf",
            ];
            for path in font_paths {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(font) = ab_glyph::FontArc::try_from_vec(bytes) {
                        return Some(font);
                    }
                }
            }
            None
        })
        .as_ref()
}

/// A node in the computed layout box tree with exact pixel coordinates.
#[derive(Debug, Clone)]
pub struct LayoutBox {
    pub node_id: usize,
    pub tag: Tag,
    pub element_id: Option<String>,
    pub rect: Rect,
    pub style: ComputedStyle,
    pub text: Option<String>,
    /// Live `<input>` value (the `value` attribute) — rendered inside the box.
    pub value: Option<String>,
    /// `<input placeholder="...">` shown in gray while the value is empty.
    pub placeholder: Option<String>,
    /// Builtin feather icon name (`icon="home"`) for `<icon-...>` elements.
    pub icon: Option<String>,
    pub children: Vec<LayoutBox>,
    /// Overflow behavior; `Scroll`/`Auto` boxes clip their children and can
    /// scroll them (see `content_height`).
    pub overflow: OverflowStyle,
    /// Natural height of the box's content (children extent), which may exceed
    /// `rect.height()` for scroll containers.
    pub content_height: f32,
}

/// A draggable edge of a `resize`-enabled box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeEdge {
    Left,
    Right,
    Top,
    Bottom,
}

/// Shared viewport context threaded through the layout pass.
///
/// All lengths are exact device pixels: `px` is one pixel, `vw`/`vh` are
/// fractions of the real viewport size — nothing is scaled by the
/// device-pixel-ratio or the window's relative size.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LayoutCtx {
    pub(crate) viewport_w: f32,
    pub(crate) viewport_h: f32,
}

impl LayoutCtx {
    pub(crate) fn viewport_rect(&self) -> Rect {
        Rect::from_xywh(0.0, 0.0, self.viewport_w, self.viewport_h).unwrap()
    }
}

/// Convenience for the test module.
#[cfg(test)]
pub(crate) fn test_font() -> Option<&'static ab_glyph::FontArc> {
    get_font()
}
