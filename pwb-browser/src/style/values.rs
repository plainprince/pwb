//! Value parsers: colors, lengths, unit values, alignment and borders.

use pwb_core::spec::{Alignment, Color, UnitValue};

/// Parse `border: <width> [solid] <color>` (width and color both optional).
pub(super) fn parse_border_shorthand(v: &str) -> (Option<f32>, Option<Color>) {
    let mut width = None;
    let mut color = None;
    for token in v.split_whitespace() {
        if token == "solid" || token == "dashed" || token == "dotted" {
            continue;
        }
        if let Some(px) = parse_px(token) {
            width = Some(px);
        } else if let Some(c) = parse_color(token) {
            color = Some(c);
        }
    }
    (width, color)
}

/// Apply `border-<side>: <width> [solid] <color>` to one side's width + color.
pub(super) fn set_border_side(width: &mut f32, color: &mut Color, v: &str) {
    let (w, c) = parse_border_shorthand(v);
    if let Some(w) = w {
        *width = w;
    }
    if let Some(c) = c {
        *color = c;
    }
}

pub(super) fn parse_alignment(v: &str) -> Option<Alignment> {
    match v {
        "center" => Some(Alignment::Center),
        "flex-start" | "start" => Some(Alignment::Start),
        "flex-end" | "end" => Some(Alignment::End),
        "space-between" => Some(Alignment::SpaceBetween),
        _ => None,
    }
}

pub(super) fn parse_px(v: &str) -> Option<f32> {
    if let Some(percent) = v.strip_suffix('%') {
        return percent.trim().parse::<f32>().ok();
    }
    v.replace("px", "").trim().parse::<f32>().ok()
}

pub(super) fn parse_unit_value(v: &str) -> Option<UnitValue> {
    if v == "auto" {
        return Some(UnitValue::Auto);
    }
    if let Some(percent) = v.strip_suffix('%') {
        if let Ok(val) = percent.trim().parse::<f32>() {
            return Some(UnitValue::Percent(val / 100.0));
        }
    }
    if let Some(rest) = v.strip_suffix("vw") {
        if let Ok(val) = rest.trim().parse::<f32>() {
            return Some(UnitValue::Vw(val / 100.0));
        }
    }
    if let Some(rest) = v.strip_suffix("vh") {
        if let Ok(val) = rest.trim().parse::<f32>() {
            return Some(UnitValue::Vh(val / 100.0));
        }
    }
    if let Some(px) = parse_px(v) {
        return Some(UnitValue::Px(px));
    }
    None
}

pub(super) fn parse_color(v: &str) -> Option<Color> {
    let clean = v.trim().trim_start_matches('#');
    let named = |r: u8, g: u8, b: u8| Some(Color { r, g, b, a: 255 });
    match clean {
        "white" => return Some(Color::WHITE),
        "black" => return Some(Color::BLACK),
        "transparent" => return Some(Color::TRANSPARENT),
        "red" => return named(255, 0, 0),
        "green" => return named(0, 128, 0),
        "lime" => return named(0, 255, 0),
        "blue" => return named(0, 0, 255),
        "gray" | "grey" => return named(128, 128, 128),
        "yellow" => return named(255, 255, 0),
        "orange" => return named(255, 165, 0),
        "purple" => return named(128, 0, 128),
        _ => {}
    }
    if clean.len() == 6 {
        let r = u8::from_str_radix(&clean[0..2], 16).ok()?;
        let g = u8::from_str_radix(&clean[2..4], 16).ok()?;
        let b = u8::from_str_radix(&clean[4..6], 16).ok()?;
        return Some(Color { r, g, b, a: 255 });
    }
    if clean.len() == 3 {
        let r = u8::from_str_radix(&clean[0..1], 16).ok()?;
        let g = u8::from_str_radix(&clean[1..2], 16).ok()?;
        let b = u8::from_str_radix(&clean[2..3], 16).ok()?;
        return Some(Color {
            r: r * 17,
            g: g * 17,
            b: b * 17,
            a: 255,
        });
    }
    None
}
