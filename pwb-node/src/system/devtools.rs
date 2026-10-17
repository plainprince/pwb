//! `devtools.plain` DOM & style inspector page.

use super::SystemDomainHandler;
use pwb_core::wire::Bundle;

impl<'a> SystemDomainHandler<'a> {
    /// Generate a native `.pwb` bundle for `devtools.plain` DOM & Style Inspector page!
    pub fn render_system_devtools_page(&self, current_bundle: Option<&Bundle>) -> Bundle {
        let title = "devtools.plain — Developer Tools Inspector".to_string();

        let mut pml_markup = String::from(
            r##"<plain>
            <head>
                <title>devtools.plain</title>
            </head>
            <view class="devtools-container">
                <text class="devtools-header">devtools.plain — PlainWeb Developer Tools</text>
            "##,
        );

        if let Some(bundle) = current_bundle {
            pml_markup.push_str(&format!(
                r##"<view class="info-card">
                    <text class="info-title">Bundle Title: {}</text>
                    <text class="info-sub">Wire Version: {} | Root Tag: {:?}</text>
                </view>
                <text class="section-heading">DOM Tree Structure:</text>
                "##,
                bundle.title, bundle.version, bundle.root_node.tag
            ));

            render_node_inspector(&bundle.root_node, &mut pml_markup, 0);
        } else {
            pml_markup.push_str(
                r##"<text class="no-results">No active bundle loaded to inspect.</text>"##,
            );
        }

        pml_markup.push_str(r##"</view></plain>"##);

        let pss_styles = r#"
.devtools-container {
    background-color: #181825;
    padding: 20px;
    gap: 12px;
}
.devtools-header {
    color: #cba6f7;
    font-size: 24px;
}
.info-card {
    background-color: #313244;
    padding: 12px;
    border-radius: 6px;
    gap: 4px;
}
.info-title {
    color: #89b4fa;
    font-size: 18px;
}
.info-sub {
    color: #a6adc8;
    font-size: 14px;
}
.section-heading {
    color: #a6e3a1;
    font-size: 20px;
}
.dom-node-box {
    background-color: #1e1e2e;
    padding: 8px;
    border-radius: 4px;
    gap: 2px;
}
.dom-tag {
    color: #f9e2af;
    font-size: 15px;
}
"#;

        let js_script = r#"console.log("[DevTools] Attached to current bundle.");"#;

        let parsed = pwb_web::parse::parse_pml_markup(&pml_markup).unwrap();
        let mut root_node = parsed;
        let rules = pwb_web::parse::parse_pss_stylesheet(pss_styles);
        pwb_web::parse::apply_stylesheet(&mut root_node, &rules);

        pwb_web::bundle::create_bundle(
            root_node,
            title,
            None,
            Some(js_script.as_bytes().to_vec()),
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        )
    }
}

fn render_node_inspector(node: &pwb_core::spec::PlainNode, out: &mut String, depth: usize) {
    let indent = "  ".repeat(depth);
    let txt = node.text_content.as_deref().unwrap_or("");
    let attr_str = node
        .attributes
        .iter()
        .map(|(k, v)| format!("{}=\"{}\"", k, v))
        .collect::<Vec<_>>()
        .join(" ");

    out.push_str(&format!(
        r##"<view class="dom-node-box">
            <text class="dom-tag">{} &lt;{:?}&gt; {} [txt: "{}"]</text>
        </view>"##,
        indent, node.tag, attr_str, txt
    ));

    for child in &node.children {
        render_node_inspector(child, out, depth + 1);
    }
}
