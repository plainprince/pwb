//! Text measurement utilities.

use super::box_types::get_font;
use ab_glyph::{Font, FontArc};
#[cfg(test)]
use pwb_core::spec::Tag;
use pwb_core::spec::UnitValue;

/// Resolve a `UnitValue` against a dimension (containing block size) and the viewport.
/// Returns `None` for `Auto`.
pub fn resolve_unit(uv: UnitValue, dim: f32, viewport_w: f32, viewport_h: f32) -> Option<f32> {
    match uv {
        UnitValue::Px(v) => Some(v),
        UnitValue::Percent(p) => Some(dim * p),
        UnitValue::Vw(p) => Some(viewport_w * p),
        UnitValue::Vh(p) => Some(viewport_h * p),
        UnitValue::Auto => None,
    }
}

/// Resolve an optional unit value, falling back to a viewport-relative default
/// when the property is unset. `%` resolves against `dim` (the containing
/// block); `vw`/`vh` resolve against the real viewport.
pub fn resolve_len(
    uv: Option<UnitValue>,
    default: UnitValue,
    dim: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> f32 {
    match uv {
        Some(u) => resolve_unit(u, dim, viewport_w, viewport_h).unwrap_or(0.0),
        None => resolve_unit(default, dim, viewport_w, viewport_h).unwrap_or(0.0),
    }
}

/// Resolve a box's explicit `width`, clamped to its `min-width`/`max-width`
/// (each resolved against the containing block). `None` when no width is set.
pub fn resolve_clamped_width(
    style: &crate::style::ComputedStyle,
    dim: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> Option<f32> {
    let w = style
        .width
        .and_then(|uv| resolve_unit(uv, dim, viewport_w, viewport_h))?;
    let min = style
        .min_width
        .and_then(|uv| resolve_unit(uv, dim, viewport_w, viewport_h))
        .unwrap_or(0.0);
    let max = style
        .max_width
        .and_then(|uv| resolve_unit(uv, dim, viewport_w, viewport_h))
        .unwrap_or(f32::MAX);
    Some(w.clamp(min, max))
}

/// Glyph advance for a specific glyph at a given pixel size.
pub fn glyph_advance_px(font: &FontArc, glyph_id: ab_glyph::GlyphId, size_px: f32) -> f32 {
    let height = font.height_unscaled().max(1.0);
    font.h_advance_unscaled(glyph_id) * (size_px / height)
}

/// Measure text width using the system font (or a fallback).
///
/// Uses the same per-glyph advance the renderer steps by when drawing, so a
/// text box is sized exactly to the text it paints (buttons, inputs, and the
/// input caret all agree on where the glyphs land).
pub fn measure_text_width(text: &str, size: f32, font_opt: Option<&FontArc>) -> f32 {
    #[allow(clippy::redundant_closure)]
    let font = font_opt.or_else(|| get_font());
    if let Some(font) = font {
        text.chars()
            .map(|ch| glyph_advance_px(font, font.glyph_id(ch), size))
            .sum()
    } else {
        // Fallback: rough average glyph width.
        text.chars()
            .map(|ch| {
                if ch.is_whitespace() {
                    size * 0.4
                } else {
                    size * 0.55
                }
            })
            .sum()
    }
}

/// Measure text width using the system font.
pub fn measure_text(text: &str, size_px: f32) -> f32 {
    measure_text_width(text, size_px, None)
}

/// The pixel width the devtools sidebar occupies when docked next to the page
/// (i.e. the mounted `data-devtools` subtree is visible), or `None` when the
/// devtools are closed. Reads the `.devtools-sidebar` `width` (default `25vw`)
/// clamped to its `min-width`/`max-width`, matching the layout + resize drag.
pub fn docked_devtools_width(
    dom: &crate::dom::Dom,
    viewport_w: f32,
    viewport_h: f32,
) -> Option<f32> {
    let root = find_node_with_attr(&dom.root, "data-devtools")?;
    if root
        .attributes
        .get("display")
        .map(|d| d == "none")
        .unwrap_or(false)
    {
        return None;
    }
    let sidebar = find_node_with_class(root, "devtools-sidebar")?;
    let style = crate::style::compute_style(sidebar);
    let dim = viewport_w; // sidebar width is viewport-relative (vw by default)
    resolve_clamped_width(&style, dim, viewport_w, viewport_h)
}

/// First node (depth-first) carrying `attr`.
fn find_node_with_attr<'a>(
    node: &'a crate::dom::DomNode,
    attr: &str,
) -> Option<&'a crate::dom::DomNode> {
    if node.attributes.contains_key(attr) {
        return Some(node);
    }
    for child in &node.children {
        if let Some(found) = find_node_with_attr(child, attr) {
            return Some(found);
        }
    }
    None
}

/// First node (depth-first) with `class` in its `class` attribute.
fn find_node_with_class<'a>(
    node: &'a crate::dom::DomNode,
    class: &str,
) -> Option<&'a crate::dom::DomNode> {
    let has = node
        .attributes
        .get("class")
        .map(|c| c.split_whitespace().any(|cl| cl == class))
        .unwrap_or(false);
    if has {
        return Some(node);
    }
    for child in &node.children {
        if let Some(found) = find_node_with_class(child, class) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Dom;
    use std::collections::HashMap;

    #[test]
    fn test_unit_resolution() {
        assert_eq!(
            resolve_unit(UnitValue::Px(100.0), 500.0, 800.0, 600.0),
            Some(100.0)
        );
        assert_eq!(
            resolve_unit(UnitValue::Percent(0.5), 500.0, 800.0, 600.0),
            Some(250.0)
        );
        assert_eq!(
            resolve_unit(UnitValue::Vw(0.25), 500.0, 800.0, 600.0),
            Some(200.0)
        );
        assert_eq!(
            resolve_unit(UnitValue::Vh(0.5), 500.0, 800.0, 600.0),
            Some(300.0)
        );
        assert_eq!(resolve_unit(UnitValue::Auto, 500.0, 800.0, 600.0), None);
    }

    #[test]
    fn test_docked_devtools_width() {
        // Build a DOM with devtools sidebar
        let sidebar = crate::dom::DomNode {
            id: 10,
            tag: Tag::View,
            attributes: HashMap::from([
                ("class".into(), "devtools-sidebar".into()),
                ("width".into(), "25vw".into()),
            ]),
            text: None,
            children: vec![],
        };
        let devtools = crate::dom::DomNode {
            id: 9,
            tag: Tag::View,
            attributes: HashMap::from([("data-devtools".into(), "true".into())]),
            text: None,
            children: vec![sidebar],
        };
        let root = crate::dom::DomNode {
            id: 1,
            tag: Tag::Document,
            attributes: HashMap::new(),
            text: None,
            children: vec![devtools.clone()],
        };
        let dom = Dom {
            title: "test".to_string(),
            root: root.clone(),
            components: HashMap::new(),
            modules: HashMap::new(),
        };
        // 25% of 900 = 225
        assert_eq!(docked_devtools_width(&dom, 900.0, 600.0), Some(225.0));

        // Hidden devtools → None
        let mut hidden = devtools.clone();
        hidden.attributes.insert("display".into(), "none".into());
        let mut root_hidden = root.clone();
        root_hidden.children = vec![hidden];
        let dom_hidden = Dom {
            title: "t".to_string(),
            root: root_hidden,
            components: HashMap::new(),
            modules: HashMap::new(),
        };
        assert_eq!(docked_devtools_width(&dom_hidden, 900.0, 600.0), None);

        // With min/max-width clamping
        let sidebar_clamped = crate::dom::DomNode {
            id: 20,
            tag: Tag::View,
            attributes: HashMap::from([
                ("class".into(), "devtools-sidebar".into()),
                ("width".into(), "50vw".into()),
                ("min-width".into(), "100px".into()),
                ("max-width".into(), "450px".into()),
            ]),
            text: None,
            children: vec![],
        };
        let devtools_clamped = crate::dom::DomNode {
            id: 19,
            tag: Tag::View,
            attributes: HashMap::from([("data-devtools".into(), "true".into())]),
            text: None,
            children: vec![sidebar_clamped],
        };
        let mut root_clamped = root.clone();
        root_clamped.children = vec![devtools_clamped];
        let dom_clamped = Dom {
            title: "t".to_string(),
            root: root_clamped,
            components: HashMap::new(),
            modules: HashMap::new(),
        };
        // 50% of 900 = 450, max-width caps at 450
        assert_eq!(
            docked_devtools_width(&dom_clamped, 900.0, 600.0),
            Some(450.0),
            "max-width 50vw = 450 caps the dock width"
        );
    }
}
