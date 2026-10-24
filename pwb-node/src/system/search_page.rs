//! `search.plain` results page.

use super::SystemDomainHandler;
use crate::search::SearchResult;
use pwb_core::wire::Bundle;

const RESULTS_STYLES: &str = r#"
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
    padding: 0;
}
.result-cid {
    color: #bac2de;
    font-size: 14px;
}
.visit-link {
    color: #fab387;
    font-size: 15px;
    padding: 0;
}
"#;

/// Render a federated `search.plain` results page from a pre-merged result
/// list. Each card exposes the result title and a "Visit Site" link, both
/// pointing at `content.pwb/<content-id>`, which the browser resolves from its
/// artifact host (filling it from peers on a miss). Navigation is native — no
/// script payload is needed.
pub fn render_search_results_bundle(query: &str, results: &[SearchResult]) -> Bundle {
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
            let cid = res.content_id.to_hex();
            pml_markup.push_str(&format!(
                r##"<view class="result-card">
                        <link class="result-heading" href="content.pwb/{cid}">#{}. {}</link>
                        <text class="result-cid">Content ID: {}</text>
                        <link class="visit-link" href="content.pwb/{cid}">Visit Site →</link>
                    </view>"##,
                idx + 1,
                res.title,
                res.content_id
            ));
        }
    }
    pml_markup.push_str(r##"</view></plain>"##);

    let mut root_node = pwb_web::parse::parse_pml_markup(&pml_markup).unwrap();
    let rules = pwb_web::parse::parse_pss_stylesheet(RESULTS_STYLES);
    pwb_web::parse::apply_stylesheet(&mut root_node, &rules);

    pwb_web::bundle::create_bundle(
        root_node,
        title,
        None,
        None,
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    )
}

/// Render the transient "searching the swarm…" page shown while shard queries
/// are in flight.
pub fn render_search_pending_bundle(query: &str) -> Bundle {
    let title = format!("search.plain — Searching for \"{}\"", query);
    let pml_markup = format!(
        r##"<plain>
            <head><title>{title}</title></head>
            <view class="search-container">
                <text class="search-title">search.plain — PlainWeb Decentralized Search</text>
                <text class="search-query">Query: "{query}"</text>
                <text class="no-results">Querying the swarm shards…</text>
            </view>
        </plain>"##
    );
    let styles = r#"
.search-container { background-color: #1e1e2e; padding: 25px; gap: 15px; }
.search-title { color: #89b4fa; font-size: 26px; }
.search-query { color: #a6adc8; font-size: 16px; }
.no-results { color: #f9e2af; font-size: 16px; }
"#;
    let mut root_node = pwb_web::parse::parse_pml_markup(&pml_markup).unwrap();
    let rules = pwb_web::parse::parse_pss_stylesheet(styles);
    pwb_web::parse::apply_stylesheet(&mut root_node, &rules);
    pwb_web::bundle::create_bundle(
        root_node,
        title,
        None,
        None,
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    )
}

impl<'a> SystemDomainHandler<'a> {
    /// Generate a native `.pwb` bundle for `search.plain` queries dynamically!
    ///
    /// This is the *local* fallback (pre-federation behavior) used when only a
    /// [`crate::search::SearchEngine`] is available. Federated callers render
    /// the merged network results with [`render_search_results_bundle`].
    pub fn render_system_search_page(&self, query: &str) -> Bundle {
        if query.trim().is_empty() {
            return self.render_system_search_home();
        }
        let results = self.search.search(query);
        render_search_results_bundle(query, &results)
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
