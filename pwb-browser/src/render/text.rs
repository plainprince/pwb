//! Text drawing, JS syntax tokenizing, and pixel blending.

use crate::layout::{glyph_advance_px, measure_text};
use crate::render::core::intersect_opt;
use ab_glyph::{Font, Glyph, PxScale};
use pwb_core::spec::Color;
use tiny_skia::{ColorU8, Paint, PathBuilder, Pixmap, Rect, Transform};

use super::core::RenderState;

/// Width of the first `caret` chars of an input value at `font_size` (the x
/// offset of the caret), matching the renderer's per-glyph advance stepping.
pub(crate) fn caret_text_width(value: &str, font_size: f32, caret: usize) -> f32 {
    let prefix: String = value.chars().take(caret).collect();
    measure_text(&prefix, font_size)
}

/// Chrome-DevTools-dark-palette token colors for `<code>` highlighting.
pub(crate) const KEYWORD_COLOR: Color = Color {
    r: 86,
    g: 156,
    b: 214,
    a: 255,
}; // #569cd6
pub(crate) const STRING_COLOR: Color = Color {
    r: 206,
    g: 145,
    b: 120,
    a: 255,
}; // #ce9178
pub(crate) const NUMBER_COLOR: Color = Color {
    r: 181,
    g: 206,
    b: 168,
    a: 255,
}; // #b5cea8
pub(crate) const COMMENT_COLOR: Color = Color {
    r: 106,
    g: 153,
    b: 85,
    a: 255,
}; // #6a9955

/// Tokenize JS-ish text into `(token, color)` runs. Identifiers, operators and
/// punctuation use `base` (the element's own text color); keywords, strings,
/// numbers and comments get the fixed syntax palette.
pub(crate) fn js_tokenize(text: &str, base: Color) -> Vec<(String, Color)> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c.is_whitespace() {
            let start = i;
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            out.push((chars[start..i].iter().collect(), base));
            continue;
        }
        // Strings ('...', "...", `...`), honoring backslash escapes.
        if c == '\'' || c == '"' || c == '`' {
            let quote = c;
            let start = i;
            i += 1;
            while i < n {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push((chars[start..i.min(n)].iter().collect(), STRING_COLOR));
            continue;
        }
        // Line comments.
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            let start = i;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            out.push((chars[start..i].iter().collect(), COMMENT_COLOR));
            continue;
        }
        // Block comments.
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            while i + 1 < n && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(n);
            out.push((chars[start..i].iter().collect(), COMMENT_COLOR));
            continue;
        }
        // Numbers (decimal, hex, floats).
        if c.is_ascii_digit() || (c == '.' && i + 1 < n && chars[i + 1].is_ascii_digit()) {
            let start = i;
            if c == '0' && i + 1 < n && (chars[i + 1] == 'x' || chars[i + 1] == 'X') {
                i += 2;
                while i < n && chars[i].is_ascii_hexdigit() {
                    i += 1;
                }
            } else {
                while i < n
                    && (chars[i].is_ascii_digit()
                        || chars[i] == '.'
                        || chars[i] == '_'
                        || chars[i] == 'e'
                        || chars[i] == 'E'
                        || ((chars[i] == '+' || chars[i] == '-')
                            && i > start
                            && (chars[i - 1] == 'e' || chars[i - 1] == 'E')))
                {
                    i += 1;
                }
            }
            out.push((chars[start..i].iter().collect(), NUMBER_COLOR));
            continue;
        }
        // Identifiers / keywords.
        if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < n && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if is_js_keyword(&word) {
                out.push((word, KEYWORD_COLOR));
            } else {
                out.push((word, base));
            }
            continue;
        }
        // Everything else: operators / punctuation, one char at a time.
        out.push((c.to_string(), base));
        i += 1;
    }
    out
}

fn is_js_keyword(word: &str) -> bool {
    matches!(
        word,
        "var"
            | "let"
            | "const"
            | "function"
            | "return"
            | "if"
            | "else"
            | "for"
            | "while"
            | "do"
            | "switch"
            | "case"
            | "break"
            | "continue"
            | "new"
            | "class"
            | "extends"
            | "super"
            | "this"
            | "typeof"
            | "instanceof"
            | "in"
            | "of"
            | "try"
            | "catch"
            | "finally"
            | "throw"
            | "delete"
            | "void"
            | "yield"
            | "async"
            | "await"
            | "import"
            | "export"
            | "default"
            | "static"
            | "get"
            | "set"
            | "null"
            | "undefined"
            | "true"
            | "false"
            | "NaN"
            | "Infinity"
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_text_ab_glyph(
    pixmap: &mut Pixmap,
    text: &str,
    start_x: f32,
    start_y: f32,
    size: f32,
    color: Color,
    node_id: usize,
    selection: &crate::render::TextSelection,
    state: RenderState,
) -> f32 {
    let sx = state.tx;
    let sy = state.ty;
    // A point is visible when it lands inside the current clip region (screen
    // coordinates). `None` means no clipping.
    let visible = |px: f32, py: f32| -> bool {
        match state.clip {
            Some(c) => px >= c.left() && px <= c.right() && py >= c.top() && py <= c.bottom(),
            None => true,
        }
    };

    if let Some(font) = super::icons::get_font() {
        let scale = PxScale::from(size);
        let mut x_offset = start_x;
        let baseline_y = start_y + size * 0.8;

        for (char_index, ch) in text.chars().enumerate() {
            let glyph_id = font.glyph_id(ch);
            let glyph: Glyph =
                glyph_id.with_scale_and_position(scale, ab_glyph::point(x_offset, baseline_y));

            // Same per-glyph stepping as the layout's `measure_text_width`, so a
            // text box's width always matches what is actually drawn.
            let advance = glyph_advance_px(font, glyph_id, size);

            let is_char_selected = is_point_selected(node_id, char_index, selection);

            if is_char_selected {
                if let Some(sel_rect) = intersect_opt(
                    state.clip,
                    Rect::from_xywh(x_offset + sx, start_y + sy, advance, size * 1.1).unwrap(),
                ) {
                    let mut sel_paint = Paint::default();
                    sel_paint.set_color(SkiaColor::from_rgba8(74, 118, 168, 200));
                    let sel_path = PathBuilder::from_rect(sel_rect);
                    pixmap.fill_path(
                        &sel_path,
                        &sel_paint,
                        tiny_skia::FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
            }

            let draw_color = if is_char_selected {
                Color::WHITE
            } else {
                color
            };

            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    if coverage > 0.02 {
                        let xf = bounds.min.x + gx as f32 + sx;
                        let yf = bounds.min.y + gy as f32 + sy;
                        if visible(xf, yf) {
                            let px = xf as u32;
                            let py = yf as u32;
                            if px < pixmap.width() && py < pixmap.height() {
                                blend_pixel(pixmap, px, py, draw_color, coverage);
                            }
                        }
                    }
                });
            }
            x_offset += advance;
        }
        // Return the advance width actually consumed so callers that chain
        // draws (the syntax-highlighted `<code>` renderer) place the next
        // token right after this one instead of overlapping it.
        x_offset - start_x
    } else {
        let mut x_offset = start_x;
        let mut paint = Paint::default();
        paint.set_color(to_skia_color(color));

        for ch in text.chars() {
            if ch.is_whitespace() {
                x_offset += size * 0.4;
                continue;
            }

            let char_w = size * 0.55;
            let char_h = size * 0.8;

            if let Some(r) = intersect_opt(
                state.clip,
                Rect::from_xywh(x_offset + sx, start_y + sy, char_w * 0.75, char_h).unwrap(),
            ) {
                let path = PathBuilder::from_rect(r);
                pixmap.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }

            x_offset += char_w + 2.0;
        }
        // Same contract as the font path: the advance width consumed.
        x_offset - start_x
    }
}

/// Whether the character at `(node_id, char_index)` falls inside the selection range.
///
/// Handles reversed ranges (dragging left/up) by normalizing to a forward range.
/// The intermediate-node test assumes node ids are assigned in document order,
/// which the bundler guarantees within a document.
pub(crate) fn is_point_selected(
    node_id: usize,
    char_index: usize,
    selection: &crate::render::TextSelection,
) -> bool {
    let (Some(start_node), Some(start_char)) = (selection.start_node_id, selection.start_char_pos)
    else {
        return false;
    };
    let (Some(end_node), Some(end_char)) = (selection.end_node_id, selection.end_char_pos) else {
        return false;
    };

    let (s_node, s_char, e_node, e_char) = if (start_node, start_char) <= (end_node, end_char) {
        (start_node, start_char, end_node, end_char)
    } else {
        (end_node, end_char, start_node, start_char)
    };

    if node_id == s_node && node_id == e_node {
        char_index >= s_char && char_index < e_char
    } else if node_id == s_node {
        char_index >= s_char
    } else if node_id == e_node {
        char_index < e_char
    } else {
        node_id > s_node && node_id < e_node
    }
}

fn blend_pixel(pixmap: &mut Pixmap, x: u32, y: u32, color: Color, coverage: f32) {
    let width = pixmap.width();
    let pixels = pixmap.pixels_mut();
    let idx = (y * width + x) as usize;
    if idx < pixels.len() {
        let existing = pixels[idx];
        let dst_r = existing.red() as f32;
        let dst_g = existing.green() as f32;
        let dst_b = existing.blue() as f32;
        let dst_a = existing.alpha() as f32 / 255.0;

        let font_alpha = (color.a as f32 / 255.0) * coverage;

        let out_r = (color.r as f32 * font_alpha + dst_r * (1.0 - font_alpha)).min(255.0) as u8;
        let out_g = (color.g as f32 * font_alpha + dst_g * (1.0 - font_alpha)).min(255.0) as u8;
        let out_b = (color.b as f32 * font_alpha + dst_b * (1.0 - font_alpha)).min(255.0) as u8;
        let out_a = ((font_alpha + dst_a * (1.0 - font_alpha)) * 255.0).min(255.0) as u8;

        let c = ColorU8::from_rgba(out_r, out_g, out_b, out_a);
        pixels[idx] = c.premultiply();
    }
}

pub(crate) fn to_skia_color(c: Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a)
}

use tiny_skia::Color as SkiaColor;
