//! Native System Domains Protocol Handler (`pwb-node::system`).
//!
//! PlainWeb replaces traditional centralized web services with serverless system domains:
//! - `search.plain`: Decentralized search engine API & dynamic interface generation.
//! - `devtools.plain`: Built-in Developer Tools DOM & Style Inspector.
//! - `gossip.plain`: P2P artifact gossip and index propagation.
//! - `nodes.plain`: Peer node routing and health registry.
//! - `register.plain`: Domain registration interface powered by sparse credit ledger.

use crate::domain::DomainRegistry;
use crate::host::ArtifactHost;
use crate::identity::IdentityManager;
use crate::search::SearchEngine;
use pwb_core::crypto::PublicKey;
use pwb_core::net::{NetworkMessage, SearchResultWire};
use pwb_core::wire::Bundle;

pub struct SystemDomainHandler<'a> {
    pub search: &'a SearchEngine,
    pub host: &'a ArtifactHost,
    pub registry: &'a DomainRegistry,
    pub identity: &'a IdentityManager,
}

impl<'a> SystemDomainHandler<'a> {
    pub fn new(
        search: &'a SearchEngine,
        host: &'a ArtifactHost,
        registry: &'a DomainRegistry,
        identity: &'a IdentityManager,
    ) -> Self {
        Self {
            search,
            host,
            registry,
            identity,
        }
    }

    /// Process incoming system domain requests and return response messages.
    pub fn handle_message(&self, message: NetworkMessage) -> Option<NetworkMessage> {
        match message {
            NetworkMessage::SearchQuery { query, .. } => {
                let raw_results = self.search.search(&query);
                let wire_results = raw_results
                    .into_iter()
                    .map(|r| SearchResultWire {
                        content_id: r.content_id,
                        title: r.title,
                        rank_score: r.rank_score,
                        snippet: format!("Content ID: {}", r.content_id),
                    })
                    .collect();

                Some(NetworkMessage::SearchResponse {
                    query,
                    results: wire_results,
                })
            }
            NetworkMessage::FetchArtifact { content_id } => {
                let bundle_bytes = self.host.get(&content_id).cloned().unwrap_or_default();
                Some(NetworkMessage::ArtifactResponse {
                    content_id,
                    bundle_bytes,
                })
            }
            _ => None,
        }
    }

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

    /// Generate a native `.pwb` bundle for `register.plain` domain claiming dashboard!
    pub fn render_system_register_page(&self, owner_pubkey: PublicKey) -> Bundle {
        let title = "register.plain — Domain Claim Registry".to_string();
        let count = self.registry.count();
        let pml_markup = format!(
            r##"<plain>
            <head>
                <title>register.plain</title>
            </head>
            <view class="reg-container">
                <text class="reg-title">register.plain — Sparse Credit Domain Registry</text>
                <text class="reg-info">Total Registered Domains: {}</text>
                <text class="reg-info">Owner Identity Public Key: {}</text>
                <button id="claim-btn">Claim New Domain (Cost: 50 Credits)</button>
            </view>
            </plain>"##,
            count, owner_pubkey
        );

        let parsed = pwb_web::parse::parse_pml_markup(&pml_markup).unwrap();
        pwb_web::bundle::create_bundle(
            parsed,
            title,
            None,
            None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::DomainRegistry;
    use crate::host::ArtifactHost;
    use crate::identity::IdentityManager;
    use crate::search::SearchEngine;

    #[test]
    fn test_system_devtools_domain() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        let handler = SystemDomainHandler::new(&search, &host, &registry, &identity);
        let bundle = handler.render_system_devtools_page(None);

        assert_eq!(bundle.title, "devtools.plain — Developer Tools Inspector");
        assert!(bundle.script_payload.is_some());
    }
}
