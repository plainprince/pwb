//! `search.plain` results page.

use super::SystemDomainHandler;
use pwb_core::wire::Bundle;

impl<'a> SystemDomainHandler<'a> {
    /// Generate a native `.pwb` bundle for `search.plain` queries dynamically!
    ///
    /// When `query` is empty, renders a search screen with an input field and
    /// a search button instead of results. Clicking the button (or pressing
    /// Enter in the input) navigates to `search.plain?q=<value>` via
    /// `__pwb_navigate`.
    pub fn render_system_search_page(&self, query: &str) -> Bundle {
        if query.trim().is_empty() {
            return self.render_system_search_home();
        }

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

    /// Render the search.plain home screen: an input field and a search button.
    /// Enter in the input or clicking the button navigates to
    /// `search.plain?q=<value>`.
    fn render_system_search_home(&self) -> Bundle {
        let title = "search.plain — PlainWeb Search".to_string();

        let pml_markup = r##"<plain>
            <head>
                <title>search.plain — PlainWeb Search</title>
            </head>
            <view class="search-home">
                <text class="search-title">search.plain</text>
                <text class="search-subtitle">PlainWeb Decentralized Search</text>
                <view class="search-bar">
                    <input id="search-input" placeholder="Search PlainWeb..." />
                    <button id="search-btn">Search</button>
                </view>
                <text class="search-hint">Type a query and press Enter, or click Search.</text>
            </view>
        </plain>"##;

        let pss_styles = r#"
.search-home {
    background-color: #1e1e2e;
    padding: 40px;
    gap: 20px;
    flex-direction: column;
    align-items: center;
}
.search-title {
    color: #89b4fa;
    font-size: 32px;
}
.search-subtitle {
    color: #a6adc8;
    font-size: 16px;
}
.search-bar {
    background-color: #313244;
    padding: 12px;
    border-radius: 8px;
    gap: 10px;
    flex-direction: row;
    align-items: center;
    min-width: 260px;
}
#search-input {
    flex: 1;
    padding: 10px;
    border-radius: 6px;
    background-color: #1e1e2e;
    color: #cdd6f4;
    font-size: 16px;
}
#search-btn {
    background-color: #fab387;
    color: #11111b;
    padding: 10px 20px;
    border-radius: 6px;
    font-size: 16px;
    cursor: pointer;
}
.search-hint {
    color: #6c7086;
    font-size: 14px;
}
"#;

        let js_script = r##"
var input = document.selector("#search-input");
var btn = document.selector("#search-btn");
if (input) {
    input.addEventHandler("enter", "var q = __pwb_getValue('#search-input'); if (q && q.trim()) { __pwb_navigate('search.plain?q=' + q); }");
}
if (btn) {
    btn.onClick("var q = __pwb_getValue('#search-input'); if (q && q.trim()) { __pwb_navigate('search.plain?q=' + q); }");
}
"##;

        let parsed = pwb_web::parse::parse_pml_markup(pml_markup).unwrap();
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
