use super::*;
use pwb_core::spec::{Color, PlainNode, StyleProperty, Tag};
use std::collections::HashMap;
use std::path::Path;

use super::components::{extract_bundlables, resolve_imports, substitute_in_string};
use super::pml::parse_tag_content;
use super::style::{
    format_alignment, format_unit, parse_alignment, parse_hex_color, parse_resize, parse_unit_value,
};

fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap();
}

fn rule<'a>(rules: &'a [(String, Vec<StyleProperty>)], selector: &str) -> &'a [StyleProperty] {
    rules
        .iter()
        .find(|(s, _)| s == selector)
        .map(|(_, props)| props.as_slice())
        .unwrap_or_else(|| panic!("missing rule for {selector}"))
}

#[test]
fn test_pml_void_elements_do_not_swallow_siblings() {
    let root = parse_pml_markup(
        "<view id='container'><input type='text'><view id='next'>after</view></view>",
    )
    .expect("parses");
    assert_eq!(
        root.attributes.get("id").map(|s| s.as_str()),
        Some("container")
    );
    assert_eq!(
        root.children.len(),
        2,
        "input and next view must be siblings"
    );
    assert_eq!(root.children[0].tag, Tag::Input);
    let next = &root.children[1];
    assert_eq!(next.attributes.get("id").map(|s| s.as_str()), Some("next"));
    assert_eq!(next.children[0].tag, Tag::Text);
    assert_eq!(next.children[0].text_content.as_deref(), Some("after"));
}

#[test]
fn test_icon_elements_parse() {
    // `<icon-home/>` and `<icon name="star">` become Tag::Icon with the
    // icon name recorded in the `icon` attribute, and they are void (the
    // following element stays a sibling, not a child).
    let root = parse_pml_markup(
        "<view id='r'><icon-home/><icon name='star'/><view id='after'>x</view></view>",
    )
    .expect("parses");
    assert_eq!(root.attributes.get("id").map(|s| s.as_str()), Some("r"));
    assert_eq!(
        root.children.len(),
        3,
        "icons are void: following view stays a sibling"
    );
    let home = &root.children[0];
    assert_eq!(home.tag, Tag::Icon);
    assert_eq!(
        home.attributes.get("icon").map(|s| s.as_str()),
        Some("home")
    );
    let star = &root.children[1];
    assert_eq!(star.tag, Tag::Icon);
    assert_eq!(
        star.attributes.get("icon").map(|s| s.as_str()),
        Some("star")
    );
    assert_eq!(
        root.children[2].attributes.get("id").map(|s| s.as_str()),
        Some("after")
    );
}

#[test]
fn test_html_container_tags_and_inline_text() {
    // HTML-spelled documents parse into the same tree hierarchy: doctype and
    // `<link rel=stylesheet>` metadata are skipped, container tags become
    // Views, and inline text becomes a renderable Text child (not a
    // `text_content` that View layout ignores).
    let root = parse_pml_markup(
        "<!DOCTYPE html>\n<html>\n<head>\n<title>DevTools</title>\n<link rel=\"stylesheet\" href=\"style.pss\">\n</head>\n<body>\n<div class=\"sidebar\"><div class=\"tab\">Elements</div><button>Pick</button></div>\n</body>\n</html>",
    )
    .expect("HTML document parses");

    assert_eq!(root.tag, Tag::View, "<html> is a View");
    assert_eq!(root.children.len(), 2, "head and body are siblings");
    assert_eq!(root.children[0].tag, Tag::Head);
    assert_eq!(root.children[1].tag, Tag::View, "<body> is a View");

    fn find_class<'a>(n: &'a PlainNode, class: &str) -> Option<&'a PlainNode> {
        if n.attributes.get("class").map(|c| c.as_str()) == Some(class) {
            return Some(n);
        }
        n.children.iter().find_map(|c| find_class(c, class))
    }
    let tab = find_class(&root, "tab").expect("tab div");
    assert_eq!(tab.children.len(), 1);
    assert_eq!(tab.children[0].tag, Tag::Text);
    assert_eq!(tab.children[0].text_content.as_deref(), Some("Elements"));

    fn has_tag(n: &PlainNode, tag: Tag) -> bool {
        n.tag == tag || n.children.iter().any(|c| has_tag(c, tag))
    }
    assert!(
        !has_tag(&root, Tag::Link),
        "<link rel=stylesheet> must not become an anchor"
    );
}

#[test]
fn test_stylesheet_inherits_font_size_and_color() {
    // CSS inheritance: a Text child of a styled View picks up font-size/color
    // unless it has its own rule.
    let mut root = parse_pml_markup(
        "<plain><view class=\"tab\">Label<text class=\"own\">Own</text></view></plain>",
    )
    .expect("parses");
    let rules =
        parse_pss_stylesheet(".tab { font-size: 2vh; color: #ff0000; }\n.own { font-size: 3vh; }");
    apply_stylesheet(&mut root, &rules);

    let tab = &root.children[0];
    assert_eq!(
        tab.attributes.get("font-size").map(|s| s.as_str()),
        Some("2vh")
    );
    assert_eq!(tab.children[0].tag, Tag::Text);
    assert_eq!(
        tab.children[0]
            .attributes
            .get("font-size")
            .map(|s| s.as_str()),
        Some("2vh"),
        "inline text inherits the container font-size"
    );
    assert_eq!(
        tab.children[0].attributes.get("color").map(|s| s.as_str()),
        Some("#ff0000")
    );
    assert_eq!(
        tab.children[1]
            .attributes
            .get("font-size")
            .map(|s| s.as_str()),
        Some("3vh"),
        "the text's own rule wins over inheritance"
    );
}

#[test]
fn test_code_elements_parse() {
    // `<code>` maps to Tag::Code and keeps its text (it's a normal
    // non-void element, unlike icons).
    let root =
        parse_pml_markup("<view id='r'><code>var x = 1;</code><view id='after'>x</view></view>")
            .expect("parses");
    let row = root
        .children
        .iter()
        .find(|c| c.tag == Tag::Code)
        .expect("code node");
    assert_eq!(row.text_content.as_deref(), Some("var x = 1;"));
    assert!(root
        .children
        .iter()
        .any(|c| c.attributes.get("id").map(|s| s.as_str()) == Some("after")));
}

#[test]
fn test_component_import_with_spaces_in_param() {
    // `<import component="chip" label="...">` must substitute `{label}` in
    // the registered component body, including values containing spaces.
    // Component registration and import resolution happen in
    // `extract_bundlables` + `resolve_imports`, not in the raw parser.
    let pml = r#"
            <plain>
                <component name="chip" params="label">
                    <view class="chip"><text>{label}</text></view>
                </component>
                <view class="chips">
                    <import component="chip" label="built from components"></import>
                </view>
            </plain>
        "#;
    let mut root = parse_pml_markup(pml).expect("parses");
    let mut components = HashMap::new();
    let mut js_entries = Vec::new();
    extract_bundlables(&mut root, Path::new("."), &mut components, &mut js_entries)
        .expect("extracts components");
    assert!(components.contains_key("chip"));
    resolve_imports(&mut root, &components, 0);

    fn find_text(n: &PlainNode) -> Option<String> {
        if n.tag == Tag::Text {
            return n.text_content.clone();
        }
        for c in &n.children {
            if let Some(t) = find_text(c) {
                return Some(t);
            }
        }
        None
    }
    let label = find_text(&root).expect("found text");
    assert_eq!(label, "built from components");
}

#[test]
fn test_overflow_survives_stylesheet_compile() {
    // `overflow` is a real StyleProperty: parsed from PSS and flattened back
    // to an attribute so it survives the `.pwb` wire format.
    let dir = std::env::temp_dir().join(format!("pwb-overflow-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "index.pml",
        r#"
            <plain>
                <head><title>Overflow</title></head>
                <view class="scrollbox"><text>rows</text></view>
            </plain>
        "#,
    );
    write(
        &dir,
        "style.pss",
        ".scrollbox { overflow: scroll; height: 58vh; }\n",
    );

    let site = parse_site_directory(&dir).expect("site parses");
    let box_node = &site.root_node.children[1];
    assert_eq!(
        box_node.attributes.get("overflow").map(|s| s.as_str()),
        Some("scroll")
    );
    assert!(box_node.styles.iter().any(|p| matches!(
        p,
        StyleProperty::Overflow(pwb_core::spec::OverflowStyle::Scroll)
    )));
}

#[test]
fn test_components_imports_scripts_and_bundling() {
    // A multi-file site exercising the HTML bundler: a `<component>`
    // definition, a build-time `<import>` with {param} substitution, a
    // `<script src>` reference, CSS (lightningcss) + PSS, and multiple JS.
    let dir = std::env::temp_dir().join(format!("pwb-parse-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "index.pml",
        r#"
            <plain>
                <head><title>Comp Test</title></head>
                <component name="badge">
                    <view class="badge"><text>{label}</text></view>
                </component>
                <view class="page">
                    <import component="badge" label="Inline!" />
                    <script src="app.pjs"></script>
                    <script>console.log("inline script");</script>
                </view>
            </plain>
        "#,
    );
    write(&dir, "lib.js", "var greet = function(n) { return n; };\n");
    write(
        &dir,
        "app.pjs",
        "import \"./lib.js\";\nconsole.log(greet(\"hi\"));\n",
    );
    write(&dir, "style.css", ".page { background-color: #101014; }\n");
    write(&dir, "extra.pss", ".badge { border-radius: 6px; }\n");

    let site = parse_site_directory(&dir).expect("site parses");

    // Component registry holds the badge definition.
    assert!(site.components.contains_key("badge"));
    let badge = &site.components["badge"];
    assert_eq!(badge.tag, Tag::View);
    assert_eq!(badge.children[0].text_content.as_deref(), Some("{label}"));

    // The <import> was inlined at build time with substitution.
    fn find_text(node: &PlainNode, needle: &str) -> bool {
        if node.text_content.as_deref() == Some(needle) {
            return true;
        }
        node.children.iter().any(|c| find_text(c, needle))
    }
    assert!(
        find_text(&site.root_node, "Inline!"),
        "import must inline the component with the label param substituted"
    );
    assert!(
        !find_text(&site.root_node, "{label}"),
        "placeholder must not leak into the page"
    );

    // Scripts: entry (app.pjs) runs last, inline script included, and every
    // file is a named module.
    let payload = String::from_utf8(site.script_payload.clone().unwrap()).unwrap();
    assert!(payload.contains("── file: lib.js ──"));
    assert!(payload.contains("── file: app.pjs ──"));
    assert!(payload.contains("console.log(\"inline script\")"));
    assert!(!payload.contains("import "));
    assert!(site.modules.contains_key("lib.js"));
    assert!(site.modules.contains_key("inline-1.js"));

    // Styles: .css went through lightningcss, .pss stayed native; both
    // applied (page bg + badge radius as attributes on the nodes).
    fn find_attr(node: &PlainNode, class: &str, key: &str) -> bool {
        if node
            .attributes
            .get("class")
            .map(|c| c.contains(class))
            .unwrap_or(false)
            && node.attributes.contains_key(key)
        {
            return true;
        }
        node.children.iter().any(|c| find_attr(c, class, key))
    }
    assert!(find_attr(&site.root_node, "page", "background-color"));
    assert!(find_attr(&site.root_node, "badge", "border-radius"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_parse_alignment_and_resize_helpers() {
    use pwb_core::spec::{Alignment, ResizeStyle};
    assert_eq!(parse_alignment("center"), Some(Alignment::Center));
    assert_eq!(parse_alignment("flex-start"), Some(Alignment::Start));
    assert_eq!(parse_alignment("start"), Some(Alignment::Start));
    assert_eq!(parse_alignment("flex-end"), Some(Alignment::End));
    assert_eq!(parse_alignment("end"), Some(Alignment::End));
    assert_eq!(
        parse_alignment("space-between"),
        Some(Alignment::SpaceBetween)
    );
    assert_eq!(parse_alignment("stretch"), None);
    assert_eq!(format_alignment(&Alignment::Start), "flex-start");
    assert_eq!(format_alignment(&Alignment::Center), "center");
    assert_eq!(format_alignment(&Alignment::End), "flex-end");
    assert_eq!(format_alignment(&Alignment::SpaceBetween), "space-between");

    assert_eq!(parse_resize("both"), Some(ResizeStyle::Both));
    assert_eq!(parse_resize("vertical"), Some(ResizeStyle::Vertical));
    assert_eq!(parse_resize("horizontal"), Some(ResizeStyle::Horizontal));
    assert_eq!(parse_resize("none"), Some(ResizeStyle::None));
    assert_eq!(parse_resize("diagonal"), None);
}

#[test]
fn test_unit_value_parse_and_format() {
    use pwb_core::spec::UnitValue;
    assert_eq!(parse_unit_value("auto"), Some(UnitValue::Auto));
    assert_eq!(parse_unit_value(" 50% "), Some(UnitValue::Percent(0.5)));
    assert_eq!(parse_unit_value("10vw"), Some(UnitValue::Vw(0.1)));
    assert_eq!(parse_unit_value("20vh"), Some(UnitValue::Vh(0.2)));
    assert_eq!(parse_unit_value("15px"), Some(UnitValue::Px(15.0)));
    assert_eq!(parse_unit_value("15"), Some(UnitValue::Px(15.0)));
    assert_eq!(parse_unit_value("banana"), None);

    assert_eq!(format_unit(&UnitValue::Px(15.0)), "15px");
    assert_eq!(format_unit(&UnitValue::Percent(0.5)), "50%");
    assert_eq!(format_unit(&UnitValue::Vw(0.1)), "10vw");
    assert_eq!(format_unit(&UnitValue::Vh(0.2)), "20vh");
    assert_eq!(format_unit(&UnitValue::Auto), "auto");
}

#[test]
fn test_parse_hex_color_variants() {
    assert_eq!(
        parse_hex_color("#ff0000"),
        Some(Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255
        })
    );
    assert_eq!(
        parse_hex_color("00ff00"),
        Some(Color {
            r: 0,
            g: 255,
            b: 0,
            a: 255
        })
    );
    assert_eq!(parse_hex_color("white"), Some(Color::WHITE));
    assert_eq!(parse_hex_color("#black"), Some(Color::BLACK));
    assert_eq!(parse_hex_color("#fff"), None, "3-digit hex unsupported");
    assert_eq!(parse_hex_color("#gggggg"), None, "non-hex digits");
    assert_eq!(parse_hex_color("transparent"), None);
}

#[test]
fn test_parse_tag_content_attributes() {
    let (tag, attrs) = parse_tag_content("view id=\"hero\" class='big wide' data-x=42");
    assert_eq!(tag, "view");
    assert_eq!(attrs.get("id").map(|s| s.as_str()), Some("hero"));
    assert_eq!(attrs.get("class").map(|s| s.as_str()), Some("big wide"));
    assert_eq!(attrs.get("data-x").map(|s| s.as_str()), Some("42"));

    let (tag, attrs) = parse_tag_content("");
    assert_eq!(tag, "view");
    assert!(attrs.is_empty());
}

#[test]
fn test_substitute_in_string_params() {
    let params = HashMap::from([
        ("name".to_string(), "Ada".to_string()),
        ("count".to_string(), "3".to_string()),
    ]);
    assert_eq!(
        substitute_in_string("hello {name}, {count} items", &params),
        "hello Ada, 3 items"
    );
    assert_eq!(
        substitute_in_string("no placeholders", &params),
        "no placeholders"
    );
    assert_eq!(substitute_in_string("{unknown}", &params), "{unknown}");
}

#[test]
fn test_parse_pss_stylesheet_property_coverage() {
    let css = "\
.alpha {
    color: #ff0000;
    background-color: #00ff00;
    padding: 8px;
    margin: 4%;
    gap: 2vw;
    font-size: 18px;
    border-radius: 6px;
    width: 100px;
    height: 50vh;
    top: 10px;
    left: 5px;
    right: 7px;
    bottom: 9px;
    position: fixed;
    justify-content: center;
    align-items: flex-end;
    resize: both;
    overflow: scroll;
    cursor: pointer;
    display: flex;
    flex-direction: column;
    unknown-prop: whatever;
}
.beta { position: relative; }
.gamma { position: static; }
.delta { position: sticky; }
.eps { display: none; flex-direction: row; overflow: hidden; }
.zeta { display: block; overflow: auto; cursor: text; }
.eta { cursor: grab; }
.theta { cursor: crosshair; }
.iota { cursor: default; overflow: visible; }
{ color: #ffffff; }
";
    let rules = parse_pss_stylesheet(css);
    use pwb_core::spec::{
        Alignment, CursorStyle, DisplayStyle, FlexDirection, OverflowStyle, PositionStyle,
        ResizeStyle, UnitValue,
    };

    let alpha = rule(&rules, ".alpha");
    assert_eq!(
        alpha[0],
        StyleProperty::Color(Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255
        })
    );
    assert_eq!(
        alpha[1],
        StyleProperty::BackgroundColor(Color {
            r: 0,
            g: 255,
            b: 0,
            a: 255
        })
    );
    assert_eq!(alpha[2], StyleProperty::Padding(UnitValue::Px(8.0)));
    assert_eq!(alpha[3], StyleProperty::Margin(UnitValue::Percent(0.04)));
    assert_eq!(alpha[4], StyleProperty::Gap(UnitValue::Vw(0.02)));
    assert_eq!(alpha[5], StyleProperty::FontSize(UnitValue::Px(18.0)));
    assert_eq!(alpha[6], StyleProperty::BorderRadius(6.0));
    assert_eq!(alpha[7], StyleProperty::Width(UnitValue::Px(100.0)));
    assert_eq!(alpha[8], StyleProperty::Height(UnitValue::Vh(0.5)));
    assert_eq!(alpha[9], StyleProperty::Top(10.0));
    assert_eq!(alpha[10], StyleProperty::Left(5.0));
    assert_eq!(alpha[11], StyleProperty::Right(7.0));
    assert_eq!(alpha[12], StyleProperty::Bottom(9.0));
    assert_eq!(alpha[13], StyleProperty::Position(PositionStyle::Fixed));
    assert_eq!(alpha[14], StyleProperty::JustifyContent(Alignment::Center));
    assert_eq!(alpha[15], StyleProperty::AlignItems(Alignment::End));
    assert_eq!(alpha[16], StyleProperty::Resize(ResizeStyle::Both));
    assert_eq!(alpha[17], StyleProperty::Overflow(OverflowStyle::Scroll));
    assert_eq!(alpha[18], StyleProperty::Cursor(CursorStyle::Pointer));
    assert_eq!(alpha[19], StyleProperty::Display(DisplayStyle::Flex));
    assert_eq!(alpha[20], StyleProperty::Direction(FlexDirection::Column));
    assert_eq!(
        alpha.len(),
        22,
        "unknown props are passed through, not dropped"
    );
    assert_eq!(
        alpha[21],
        StyleProperty::Raw("unknown-prop".to_string(), "whatever".to_string())
    );

    assert_eq!(
        rule(&rules, ".beta")[0],
        StyleProperty::Position(PositionStyle::Relative)
    );
    assert_eq!(
        rule(&rules, ".gamma")[0],
        StyleProperty::Position(PositionStyle::Static)
    );
    assert_eq!(
        rule(&rules, ".delta")[0],
        StyleProperty::Position(PositionStyle::Sticky)
    );
    assert_eq!(
        rule(&rules, ".eps"),
        [
            StyleProperty::Display(DisplayStyle::None),
            StyleProperty::Direction(FlexDirection::Row),
            StyleProperty::Overflow(OverflowStyle::Hidden),
        ]
        .as_slice()
    );
    assert_eq!(
        rule(&rules, ".zeta"),
        [
            StyleProperty::Display(DisplayStyle::Block),
            StyleProperty::Overflow(OverflowStyle::Auto),
            StyleProperty::Cursor(CursorStyle::Text),
        ]
        .as_slice()
    );
    assert_eq!(
        rule(&rules, ".eta")[0],
        StyleProperty::Cursor(CursorStyle::Grab)
    );
    assert_eq!(
        rule(&rules, ".theta")[0],
        StyleProperty::Cursor(CursorStyle::Crosshair)
    );
    assert_eq!(
        rule(&rules, ".iota"),
        [
            StyleProperty::Cursor(CursorStyle::Default),
            StyleProperty::Overflow(OverflowStyle::Visible),
        ]
        .as_slice()
    );
    // The empty-selector block is dropped.
    assert!(rules.iter().all(|(selector, _)| !selector.is_empty()));
}

#[test]
fn test_pss_passes_through_runtime_only_properties() {
    // Properties the typed enum does not model but the runtime style engine
    // understands (`border-bottom`, `min-width`, `flex`) must survive
    // compilation as attributes instead of being silently dropped.
    let rules = parse_pss_stylesheet(
        ".row { border-bottom: 0.1vh solid #3e3e42; min-width: 3vh; flex: 1; }\n",
    );
    let mut root = parse_pml_markup("<view class='row'></view>").expect("parses");
    apply_stylesheet(&mut root, &rules);

    assert_eq!(
        root.attributes.get("border-bottom").map(|s| s.as_str()),
        Some("0.1vh solid #3e3e42")
    );
    assert_eq!(
        root.attributes.get("min-width").map(|s| s.as_str()),
        Some("3vh")
    );
    assert_eq!(root.attributes.get("flex").map(|s| s.as_str()), Some("1"));
}

#[test]
fn test_pss_leading_comments_do_not_swallow_the_first_rule() {
    // A leading `/* … */` comment must not be parsed as part of the first
    // selector (devtools' style.pss opens with a multi-line comment).
    let rules = parse_pss_stylesheet(
        "/* PlainWeb DevTools - Style Sheet */\n.sidebar { width: 10px; }\n.tab { color: #ff0000; }",
    );
    assert_eq!(
        rule(&rules, ".sidebar")[0],
        StyleProperty::Width(pwb_core::spec::UnitValue::Px(10.0))
    );
    assert_eq!(
        rule(&rules, ".tab")[0],
        StyleProperty::Color(Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255
        })
    );
    assert!(
        rules.iter().all(|(selector, _)| !selector.contains("/*")),
        "comments must not leak into selectors"
    );
}

#[test]
fn test_apply_stylesheet_selector_kinds() {
    let css = "\
view { color: #ff0000; }
.card { padding: 8px; }
.card.active { border-radius: 4px; }
#hero { margin: 2px; }
";
    let rules = parse_pss_stylesheet(css);
    let mut root = parse_pml_markup(
        "<view id='hero'><view class='card active'>x</view><view class='card'>y</view></view>",
    )
    .expect("parses");
    apply_stylesheet(&mut root, &rules);

    // Tag selector + id selector both hit the root view.
    assert_eq!(
        root.attributes.get("color").map(|s| s.as_str()),
        Some("#ff0000")
    );
    assert_eq!(
        root.attributes.get("margin").map(|s| s.as_str()),
        Some("2px")
    );

    let active = &root.children[0];
    assert_eq!(
        active.attributes.get("padding").map(|s| s.as_str()),
        Some("8px")
    );
    assert_eq!(
        active.attributes.get("border-radius").map(|s| s.as_str()),
        Some("4px"),
        "compound .card.active matches only when both classes present"
    );

    let plain = &root.children[1];
    assert_eq!(
        plain.attributes.get("padding").map(|s| s.as_str()),
        Some("8px")
    );
    assert!(
        !plain.attributes.contains_key("border-radius"),
        "single class must not match the compound selector"
    );
}
