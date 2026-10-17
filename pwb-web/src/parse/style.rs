use pwb_core::spec::{Color, PlainNode, StyleProperty, UnitValue};
use std::collections::HashMap;

/// Parse `.pss` Plain Stylesheet rules.
pub fn parse_pss_stylesheet(content: &str) -> HashMap<String, Vec<StyleProperty>> {
    let mut rules: HashMap<String, Vec<StyleProperty>> = HashMap::new();

    for block in content.split('}') {
        if let Some((selector_part, body)) = block.split_once('{') {
            let selector = selector_part.trim().to_lowercase();
            let mut properties = Vec::new();

            for line in body.split(';') {
                if let Some((prop_k, prop_v)) = line.split_once(':') {
                    let k = prop_k.trim().to_lowercase();
                    let v = prop_v.trim().to_lowercase();

                    match k.as_str() {
                        "color" => {
                            if let Some(color) = parse_hex_color(&v) {
                                properties.push(StyleProperty::Color(color));
                            }
                        }
                        "background-color" | "background" => {
                            if let Some(color) = parse_hex_color(&v) {
                                properties.push(StyleProperty::BackgroundColor(color));
                            }
                        }
                        "padding" => {
                            if let Some(uv) = parse_unit_value(&v) {
                                properties.push(StyleProperty::Padding(uv));
                            }
                        }
                        "margin" => {
                            if let Some(uv) = parse_unit_value(&v) {
                                properties.push(StyleProperty::Margin(uv));
                            }
                        }
                        "gap" => {
                            if let Some(uv) = parse_unit_value(&v) {
                                properties.push(StyleProperty::Gap(uv));
                            }
                        }
                        "font-size" => {
                            if let Some(uv) = parse_unit_value(&v) {
                                properties.push(StyleProperty::FontSize(uv));
                            }
                        }
                        "border-radius" => {
                            if let Ok(val) = v.replace("px", "").trim().parse::<f32>() {
                                properties.push(StyleProperty::BorderRadius(val));
                            }
                        }
                        "position" => match v.as_str() {
                            "absolute" => properties.push(StyleProperty::Position(
                                pwb_core::spec::PositionStyle::Absolute,
                            )),
                            "relative" => properties.push(StyleProperty::Position(
                                pwb_core::spec::PositionStyle::Relative,
                            )),
                            "static" => properties.push(StyleProperty::Position(
                                pwb_core::spec::PositionStyle::Static,
                            )),
                            "fixed" => properties.push(StyleProperty::Position(
                                pwb_core::spec::PositionStyle::Fixed,
                            )),
                            "sticky" => properties.push(StyleProperty::Position(
                                pwb_core::spec::PositionStyle::Sticky,
                            )),
                            _ => {}
                        },
                        "width" => {
                            if let Some(unit) = parse_unit_value(&v) {
                                properties.push(StyleProperty::Width(unit));
                            }
                        }
                        "height" => {
                            if let Some(unit) = parse_unit_value(&v) {
                                properties.push(StyleProperty::Height(unit));
                            }
                        }
                        "top" => {
                            if let Ok(val) = v.replace("px", "").trim().parse::<f32>() {
                                properties.push(StyleProperty::Top(val));
                            }
                        }
                        "left" => {
                            if let Ok(val) = v.replace("px", "").trim().parse::<f32>() {
                                properties.push(StyleProperty::Left(val));
                            }
                        }
                        "right" => {
                            if let Ok(val) = v.replace("px", "").trim().parse::<f32>() {
                                properties.push(StyleProperty::Right(val));
                            }
                        }
                        "bottom" => {
                            if let Ok(val) = v.replace("px", "").trim().parse::<f32>() {
                                properties.push(StyleProperty::Bottom(val));
                            }
                        }
                        "cursor" => match v.as_str() {
                            "pointer" => properties
                                .push(StyleProperty::Cursor(pwb_core::spec::CursorStyle::Pointer)),
                            "text" => properties
                                .push(StyleProperty::Cursor(pwb_core::spec::CursorStyle::Text)),
                            "grab" => properties
                                .push(StyleProperty::Cursor(pwb_core::spec::CursorStyle::Grab)),
                            "crosshair" => properties.push(StyleProperty::Cursor(
                                pwb_core::spec::CursorStyle::Crosshair,
                            )),
                            "default" => properties
                                .push(StyleProperty::Cursor(pwb_core::spec::CursorStyle::Default)),
                            _ => {}
                        },
                        "display" => match v.as_str() {
                            "flex" => properties
                                .push(StyleProperty::Display(pwb_core::spec::DisplayStyle::Flex)),
                            "block" => properties
                                .push(StyleProperty::Display(pwb_core::spec::DisplayStyle::Block)),
                            "none" => properties
                                .push(StyleProperty::Display(pwb_core::spec::DisplayStyle::None)),
                            _ => {}
                        },
                        "flex-direction" => match v.as_str() {
                            "row" => properties
                                .push(StyleProperty::Direction(pwb_core::spec::FlexDirection::Row)),
                            "column" => properties.push(StyleProperty::Direction(
                                pwb_core::spec::FlexDirection::Column,
                            )),
                            _ => {}
                        },
                        "justify-content" => {
                            if let Some(alignment) = parse_alignment(&v) {
                                properties.push(StyleProperty::JustifyContent(alignment));
                            }
                        }
                        "align-items" => {
                            if let Some(alignment) = parse_alignment(&v) {
                                properties.push(StyleProperty::AlignItems(alignment));
                            }
                        }
                        "resize" => {
                            if let Some(resize) = parse_resize(&v) {
                                properties.push(StyleProperty::Resize(resize));
                            }
                        }
                        "overflow" => match v.as_str() {
                            "visible" => properties.push(StyleProperty::Overflow(
                                pwb_core::spec::OverflowStyle::Visible,
                            )),
                            "hidden" => properties.push(StyleProperty::Overflow(
                                pwb_core::spec::OverflowStyle::Hidden,
                            )),
                            "scroll" => properties.push(StyleProperty::Overflow(
                                pwb_core::spec::OverflowStyle::Scroll,
                            )),
                            "auto" => properties
                                .push(StyleProperty::Overflow(pwb_core::spec::OverflowStyle::Auto)),
                            _ => {}
                        },
                        _ => {}
                    }
                }
            }

            if !selector.is_empty() && !properties.is_empty() {
                rules.entry(selector).or_default().extend(properties);
            }
        }
    }

    rules
}

/// Apply parsed stylesheet rules to a node tree (tag, class, and id selectors).
pub fn apply_stylesheet(node: &mut PlainNode, rules: &HashMap<String, Vec<StyleProperty>>) {
    let tag_name = node.tag.name();
    if let Some(props) = rules.get(tag_name) {
        apply_style_properties(node, props);
    }

    if let Some(class_attr) = node.attributes.get("class") {
        // Support compound selectors like `.tab.active` (all classes must be present).
        let class_attr = class_attr.clone();
        let classes: Vec<&str> = class_attr.split_whitespace().collect();
        for (selector, props) in rules {
            if let Some(sel_classes) = selector.strip_prefix('.') {
                let required: Vec<&str> =
                    sel_classes.split('.').filter(|c| !c.is_empty()).collect();
                if !required.is_empty() && required.iter().all(|c| classes.contains(c)) {
                    apply_style_properties(node, props);
                }
            }
        }
    }

    if let Some(id_attr) = node.attributes.get("id") {
        let id_selector = format!("#{}", id_attr.trim());
        if let Some(props) = rules.get(&id_selector) {
            apply_style_properties(node, props);
        }
    }

    for child in &mut node.children {
        apply_stylesheet(child, rules);
    }
}

/// Apply style properties to a node, recording them both in `styles` and flattened
/// into attributes so they survive the `.pwb` wire format (which only carries attributes).
fn apply_style_properties(node: &mut PlainNode, props: &[StyleProperty]) {
    node.styles.extend(props.iter().cloned());
    for prop in props {
        if let Some((k, v)) = style_property_to_attr(prop) {
            node.attributes.insert(k, v);
        }
    }
}

fn style_property_to_attr(prop: &StyleProperty) -> Option<(String, String)> {
    use pwb_core::spec::PositionStyle;
    let (k, v): (&str, String) = match prop {
        StyleProperty::BackgroundColor(c) => (
            "background-color",
            format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b),
        ),
        StyleProperty::Color(c) => ("color", format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)),
        StyleProperty::Padding(p) => ("padding", format_unit(p)),
        StyleProperty::Margin(m) => ("margin", format_unit(m)),
        StyleProperty::Gap(g) => ("gap", format_unit(g)),
        StyleProperty::FontSize(s) => ("font-size", format_unit(s)),
        StyleProperty::BorderRadius(r) => ("border-radius", format!("{}px", r)),
        StyleProperty::Position(p) => (
            "position",
            match p {
                PositionStyle::Static => "static".to_string(),
                PositionStyle::Relative => "relative".to_string(),
                PositionStyle::Absolute => "absolute".to_string(),
                PositionStyle::Fixed => "fixed".to_string(),
                PositionStyle::Sticky => "sticky".to_string(),
            },
        ),
        StyleProperty::Top(v) => ("top", format!("{}px", v)),
        StyleProperty::Left(v) => ("left", format!("{}px", v)),
        StyleProperty::Right(v) => ("right", format!("{}px", v)),
        StyleProperty::Bottom(v) => ("bottom", format!("{}px", v)),
        StyleProperty::Width(v) => ("width", format_unit(v)),
        StyleProperty::Height(v) => ("height", format_unit(v)),
        StyleProperty::Cursor(c) => ("cursor", format!("{:?}", c).to_lowercase()),
        StyleProperty::Display(d) => (
            "display",
            match d {
                pwb_core::spec::DisplayStyle::Flex => "flex".to_string(),
                pwb_core::spec::DisplayStyle::Block => "block".to_string(),
                pwb_core::spec::DisplayStyle::None => "none".to_string(),
            },
        ),
        StyleProperty::Direction(d) => (
            "flex-direction",
            match d {
                pwb_core::spec::FlexDirection::Row => "row".to_string(),
                pwb_core::spec::FlexDirection::Column => "column".to_string(),
            },
        ),
        StyleProperty::JustifyContent(a) => ("justify-content", format_alignment(a)),
        StyleProperty::AlignItems(a) => ("align-items", format_alignment(a)),
        StyleProperty::Resize(r) => (
            "resize",
            match r {
                pwb_core::spec::ResizeStyle::None => "none".to_string(),
                pwb_core::spec::ResizeStyle::Both => "both".to_string(),
                pwb_core::spec::ResizeStyle::Vertical => "vertical".to_string(),
                pwb_core::spec::ResizeStyle::Horizontal => "horizontal".to_string(),
            },
        ),
        StyleProperty::Overflow(o) => (
            "overflow",
            match o {
                pwb_core::spec::OverflowStyle::Visible => "visible".to_string(),
                pwb_core::spec::OverflowStyle::Hidden => "hidden".to_string(),
                pwb_core::spec::OverflowStyle::Scroll => "scroll".to_string(),
                pwb_core::spec::OverflowStyle::Auto => "auto".to_string(),
            },
        ),
        _ => return None,
    };
    Some((k.to_string(), v))
}

pub(crate) fn parse_alignment(v: &str) -> Option<pwb_core::spec::Alignment> {
    match v {
        "center" => Some(pwb_core::spec::Alignment::Center),
        "flex-start" | "start" => Some(pwb_core::spec::Alignment::Start),
        "flex-end" | "end" => Some(pwb_core::spec::Alignment::End),
        "space-between" => Some(pwb_core::spec::Alignment::SpaceBetween),
        _ => None,
    }
}

pub(crate) fn format_alignment(a: &pwb_core::spec::Alignment) -> String {
    match a {
        pwb_core::spec::Alignment::Start => "flex-start".to_string(),
        pwb_core::spec::Alignment::Center => "center".to_string(),
        pwb_core::spec::Alignment::End => "flex-end".to_string(),
        pwb_core::spec::Alignment::SpaceBetween => "space-between".to_string(),
    }
}

pub(crate) fn parse_resize(v: &str) -> Option<pwb_core::spec::ResizeStyle> {
    match v {
        "both" => Some(pwb_core::spec::ResizeStyle::Both),
        "vertical" => Some(pwb_core::spec::ResizeStyle::Vertical),
        "horizontal" => Some(pwb_core::spec::ResizeStyle::Horizontal),
        "none" => Some(pwb_core::spec::ResizeStyle::None),
        _ => None,
    }
}

pub(crate) fn format_unit(v: &UnitValue) -> String {
    match v {
        UnitValue::Px(px) => format!("{}px", px),
        UnitValue::Percent(p) => format!("{}%", p * 100.0),
        UnitValue::Vw(p) => format!("{}vw", p * 100.0),
        UnitValue::Vh(p) => format!("{}vh", p * 100.0),
        UnitValue::Auto => "auto".to_string(),
    }
}

pub(crate) fn parse_unit_value(v: &str) -> Option<pwb_core::spec::UnitValue> {
    let v = v.trim();
    if v == "auto" {
        return Some(pwb_core::spec::UnitValue::Auto);
    }
    if let Some(percent) = v.strip_suffix('%') {
        if let Ok(val) = percent.trim().parse::<f32>() {
            return Some(pwb_core::spec::UnitValue::Percent(val / 100.0));
        }
    }
    if let Some(rest) = v.strip_suffix("vw") {
        if let Ok(val) = rest.trim().parse::<f32>() {
            return Some(pwb_core::spec::UnitValue::Vw(val / 100.0));
        }
    }
    if let Some(rest) = v.strip_suffix("vh") {
        if let Ok(val) = rest.trim().parse::<f32>() {
            return Some(pwb_core::spec::UnitValue::Vh(val / 100.0));
        }
    }
    if let Ok(val) = v.replace("px", "").trim().parse::<f32>() {
        return Some(pwb_core::spec::UnitValue::Px(val));
    }
    None
}

pub(crate) fn parse_hex_color(hex: &str) -> Option<Color> {
    let clean = hex.trim().trim_start_matches('#');
    if clean == "white" {
        return Some(Color::WHITE);
    }
    if clean == "black" {
        return Some(Color::BLACK);
    }
    if clean.len() == 6 {
        let r = u8::from_str_radix(&clean[0..2], 16).ok()?;
        let g = u8::from_str_radix(&clean[2..4], 16).ok()?;
        let b = u8::from_str_radix(&clean[4..6], 16).ok()?;
        Some(Color { r, g, b, a: 255 })
    } else {
        None
    }
}
