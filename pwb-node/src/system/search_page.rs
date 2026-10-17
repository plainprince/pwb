//! `search.plain` results page.

use super::SystemDomainHandler;
use pwb_core::wire::Bundle;

impl<'a> SystemDomainHandler<'a> {
    /// Generate a native `.pwb` bundle for `search.plain` queries dynamically!
    pub fn render_system_search_page(&self, query: &str) -> Bundle {
        let results = self.search.search(query);
        let title = format!("search.plain — Results for \"{}\"", query);

        let mut pml_markup = format!(
            r##"<plain>
            <head>
                <title>{}</title>
            </head>
            <view class="search-container">
                <text class="search-title">search.plain — PlainWeb Decentralized Search</text>
                <text class="search-query">Query: "{}" (Found {} results)</text>
            "##,
            title,
            query,
            results.len()
        );

        if results.is_empty() {
            pml_markup.push_str(
                r##"<text class="no-results">No indexed sites found for this query.</text>"##,
            );
        } else {
            for (idx, res) in results.iter().enumerate() {
                pml_markup.push_str(&format!(
                    r##"<view class="result-card">
                        <text class="result-heading">#{}. {}</text>
                        <text class="result-cid">Content ID: {}</text>
                        <button class="visit-btn" id="res_{}">Visit Site</button>
                    </view>"##,
                    idx + 1,
                    res.title,
                    res.content_id,
                    idx
                ));
            }
        }

        pml_markup.push_str(r##"</view></plain>"##);

        let pss_styles = r#"
.search-container {
    background-color: #1e1e2e;
    padding: 25px;
    gap: 15px;
}
.search-title {
    color: #89b4fa;
    font-size: 26px;
}
.search-query {
    color: #a6adc8;
    font-size: 16px;
}
.result-card {
    background-color: #313244;
    padding: 15px;
    border-radius: 6px;
    gap: 8px;
}
.result-heading {
    color: #a6e3a1;
    font-size: 20px;
}
.result-cid {
    color: #bac2de;
    font-size: 14px;
}
.visit-btn {
    background-color: #fab387;
    color: #11111b;
    padding: 8px;
    cursor: pointer;
}
"#;

        let js_script = format!(
            r##"console.log("Loaded search.plain system domain interface for query: {}");"##,
            query
        );

        let parsed = pwb_web::parse::parse_pml_markup(&pml_markup).unwrap();
        let mut root_node = parsed;
        let rules = pwb_web::parse::parse_pss_stylesheet(pss_styles);
        pwb_web::parse::apply_stylesheet(&mut root_node, &rules);

        pwb_web::bundle::create_bundle(
            root_node,
            title,
            None,
            Some(js_script.into_bytes()),
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        )
    }
}
