use pwb_core::spec::{Color, PlainNode, StyleProperty, UnitValue};

/// Parse `.pss` Plain Stylesheet rules.
///
/// Rules are returned in source order: later blocks override earlier ones
/// (`.tab-content.active { display: block }` must beat the preceding
/// `.tab-content { display: none }`). A repeated selector extends its first
/// entry instead of creating a second one.
pub fn parse_pss_stylesheet(content: &str) -> Vec<(String, Vec<StyleProperty>)> {
    let mut rules: Vec<(String, Vec<StyleProperty>)> = Vec::new();
    let content = strip_pss_comments(content);

    for block in content.split('}') {
        if let Some((selector_part, body)) = block.split_once('{') {
            let selector = selector_part.trim().to_lowercase();
            let mut properties = Vec::new();

            for line in body.split(';') {
                if let Some((prop_k, prop_v)) = line.split_once(':') {
                    let k = prop_k.trim().to_lowercase();
                    // Keep the authored value for pass-through properties
                    // (case matters for some, e.g. font names) while still
                    // lowercasing for the typed keyword matches below.
                    let raw_v = prop_v.trim();
                    let v = raw_v.to_lowercase();

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
                        // Properties outside the typed set (border sides,
                        // min/max sizes, flex, …) still reach the runtime
                        // style engine as plain attributes.
                        _ => {
                            if !k.is_empty() && !raw_v.is_empty() {
                                properties.push(StyleProperty::Raw(k.clone(), raw_v.to_string()));
                            }
                        }
                    }
                }
            }

            if !selector.is_empty() && !properties.is_empty() {
                if let Some((_, existing)) = rules.iter_mut().find(|(s, _)| *s == selector) {
                    existing.extend(properties);
                } else {
                    rules.push((selector, properties));
                }
            }
        }
    }

    rules
}

/// Apply parsed stylesheet rules to a node tree (tag, class, and id selectors).
///
/// `font-size` and `color` inherit down the tree (CSS inheritance): a `<text>`
/// inside `<view class="tab">` with `font-size: 2vh` renders at 2vh instead of
/// the renderer default, even though the text node has no matching rule.
pub fn apply_stylesheet(node: &mut PlainNode, rules: &[(String, Vec<StyleProperty>)]) {
    apply_stylesheet_inherited(node, rules, None, None);
}

fn apply_stylesheet_inherited(
    node: &mut PlainNode,
    rules: &[(String, Vec<StyleProperty>)],
    inherited_font_size: Option<&str>,
    inherited_color: Option<&str>,
) {
    if let Some(font_size) = inherited_font_size {
        node.attributes
            .entry("font-size".to_string())
            .or_insert_with(|| font_size.to_string());
    }
    if let Some(color) = inherited_color {
        node.attributes
            .entry("color".to_string())
            .or_insert_with(|| color.to_string());
    }

    apply_node_rules(node, rules);

    // The node's own rules (or its markup attributes) win over inherited
    // values; pass the resulting computed values on to the children.
    let font_size = node.attributes.get("font-size").cloned();
    let color = node.attributes.get("color").cloned();
    for child in &mut node.children {
        apply_stylesheet_inherited(child, rules, font_size.as_deref(), color.as_deref());
    }
}

fn apply_node_rules(node: &mut PlainNode, rules: &[(String, Vec<StyleProperty>)]) {
    let tag_name = node.tag.name();
    let id_selector = node
        .attributes
        .get("id")
        .map(|id| format!("#{}", id.trim()));
    let classes: Vec<String> = node
        .attributes
        .get("class")
        .map(|c| c.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();

    // Cascade by specificity (id > classes > tag), preserving source order
    // among rules of equal specificity. This makes `.tab-content.active` beat
    // the earlier `.tab-content` and keeps id rules authoritative.
    let mut matched: Vec<((u32, usize), &Vec<StyleProperty>)> = Vec::new();
    for (index, (selector, props)) in rules.iter().enumerate() {
        let specificity = if let Some(sel_id) = selector.strip_prefix('#') {
            let matches = id_selector
                .as_deref()
                .map(|id| id == format!("#{}", sel_id))
                .unwrap_or(false);
            (matches, 100)
        } else if let Some(sel_classes) = selector.strip_prefix('.') {
            // Support compound selectors like `.tab.active` (all classes must be present).
            let required: Vec<&str> = sel_classes.split('.').filter(|c| !c.is_empty()).collect();
            let matches = !required.is_empty()
                && required
                    .iter()
                    .all(|c| classes.iter().any(|class| class == c));
            (matches, 10 * required.len() as u32)
        } else {
            (selector == tag_name, 1)
        };
        if specificity.0 {
            matched.push(((specificity.1, index), props));
        }
    }
    matched.sort_by_key(|(key, _)| *key);
    for (_, props) in matched {
        apply_style_properties(node, props);
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
        // Pass-through property: the attribute name and value are already
        // exactly what the runtime style engine reads.
        StyleProperty::Raw(name, value) => return Some((name.clone(), value.clone())),
        _ => return None,
    };
    Some((k.to_string(), v))
}

/// Remove `/* … */` comments so they cannot merge into the following
/// selector (a leading comment used to swallow the first rule of a file).
fn strip_pss_comments(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(pos) = rest.find("/*") {
        out.push_str(&rest[..pos]);
        if let Some(end) = rest[pos + 2..].find("*/") {
            rest = &rest[pos + 2 + end + 2..];
        } else {
            rest = "";
        }
    }
    out.push_str(rest);
    out
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
