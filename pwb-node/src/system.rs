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
use pwb_core::system::SystemDomain;
use pwb_core::wire::Bundle;
use std::net::SocketAddr;

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

    /// Generate a native `.pwb` bundle for `gossip.plain` — the P2P artifact
    /// gossip & index propagation status page.
    pub fn render_system_gossip_page(&self) -> Bundle {
        let title = format!("{} — Artifact Gossip", SystemDomain::Gossip.as_str());
        let artifact_count = self.host.len();
        let domain_count = self.registry.count();
        let pml_markup = format!(
            r##"<plain>
            <head>
                <title>gossip.plain</title>
            </head>
            <view class="gossip-container">
                <text class="gossip-title">gossip.plain — PlainWeb Artifact Gossip</text>
                <text class="gossip-info">Artifacts Hosted: {}</text>
                <text class="gossip-info">Registered Domains: {}</text>
                <text class="gossip-hint">Announcements propagate to peers via GossipAnnounce wire messages; indexes merge on contact.</text>
            </view>
            </plain>"##,
            artifact_count, domain_count
        );

        let pss_styles = r#"
.gossip-container {
    background-color: #1e1e2e;
    padding: 25px;
    gap: 15px;
}
.gossip-title {
    color: #89b4fa;
    font-size: 26px;
}
.gossip-info {
    color: #a6adc8;
    font-size: 16px;
}
.gossip-hint {
    color: #6c7086;
    font-size: 14px;
}
"#;

        let js_script = r#"console.log("Loaded gossip.plain system domain interface.");"#;

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

    /// Generate a native `.pwb` bundle for `nodes.plain` — the peer node
    /// registry. When the URL carried a `connect=` target, the parsed peer
    /// endpoint is shown so scanned/clicked connection links render as a
    /// native connect card.
    pub fn render_system_nodes_page(
        &self,
        connect_target: Option<(SocketAddr, PublicKey)>,
    ) -> Bundle {
        let title = format!("{} — Peer Node Registry", SystemDomain::Nodes.as_str());
        let local_key = self.identity.public_key();
        let mut pml_markup = format!(
            r##"<plain>
            <head>
                <title>nodes.plain</title>
            </head>
            <view class="nodes-container">
                <text class="nodes-title">nodes.plain — PlainWeb Peer Registry</text>
                <text class="nodes-info">This Node Identity: {}</text>
            "##,
            local_key
        );

        match connect_target {
            Some((addr, pubkey)) => {
                pml_markup.push_str(&format!(
                    r##"<view class="connect-card">
                        <text class="connect-heading">Connection Link Target</text>
                        <text class="connect-addr">Endpoint: {}</text>
                        <text class="connect-key">Peer Public Key: {}</text>
                        <button id="connect-btn">Connect to Peer</button>
                    </view>"##,
                    addr, pubkey
                ));
            }
            None => {
                pml_markup.push_str(
                    r##"<text class="connect-hint">Open a plain://nodes.plain?connect=&lt;ip:port&gt;/&lt;pubkey&gt; link to pair with a peer.</text>"##,
                );
            }
        }

        pml_markup.push_str(r##"</view></plain>"##);

        let pss_styles = r#"
.nodes-container {
    background-color: #1e1e2e;
    padding: 25px;
    gap: 15px;
}
.nodes-title {
    color: #89b4fa;
    font-size: 26px;
}
.nodes-info {
    color: #a6adc8;
    font-size: 16px;
}
.connect-card {
    background-color: #313244;
    padding: 15px;
    border-radius: 6px;
    gap: 8px;
}
.connect-heading {
    color: #a6e3a1;
    font-size: 20px;
}
.connect-addr {
    color: #bac2de;
    font-size: 14px;
}
.connect-key {
    color: #bac2de;
    font-size: 12px;
}
.connect-btn {
    background-color: #a6e3a1;
    color: #11111b;
    padding: 8px;
    cursor: pointer;
}
.connect-hint {
    color: #6c7086;
    font-size: 14px;
}
"#;

        let js_script = r#"console.log("Loaded nodes.plain system domain interface.");"#;

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

    #[test]
    fn test_system_gossip_domain() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        let handler = SystemDomainHandler::new(&search, &host, &registry, &identity);
        let bundle = handler.render_system_gossip_page();

        assert_eq!(bundle.title, "gossip.plain — Artifact Gossip");
        assert!(bundle.script_payload.is_some());
    }

    #[test]
    fn test_system_nodes_domain() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        let handler = SystemDomainHandler::new(&search, &host, &registry, &identity);

        // No connection target: the page renders the pairing hint.
        let bundle = handler.render_system_nodes_page(None);
        assert_eq!(bundle.title, "nodes.plain — Peer Node Registry");
        assert!(bundle.script_payload.is_some());

        // With a parsed connection-link target: the connect card renders.
        let addr: std::net::SocketAddr = "127.0.0.1:9000".parse().unwrap();
        let peer = PublicKey([9u8; 32]);
        let bundle = handler.render_system_nodes_page(Some((addr, peer)));
        assert_eq!(bundle.title, "nodes.plain — Peer Node Registry");
    }

    #[test]
    fn test_handle_message_search_query_and_fetch_artifact() {
        let mut search = SearchEngine::new();
        let mut host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        // Index a bundle so the search branch has something to return.
        let mut root = pwb_core::spec::PlainNode::new(0, pwb_core::spec::Tag::Document);
        let mut body = pwb_core::spec::PlainNode::new(1, pwb_core::spec::Tag::Text);
        body.text_content = Some("quantum tunneling explained".into());
        root.children.push(body);
        let bundle = pwb_web::bundle::create_bundle(
            root,
            "Quantum Site".to_string(),
            None,
            None,
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        );
        let cid = pwb_core::crypto::ContentId::compute(b"quantum-bytes");
        search.index_bundle(cid, &bundle);
        let stored_cid = host.store(bundle.serialize()).expect("stores");

        let handler = SystemDomainHandler::new(&search, &host, &registry, &identity);

        // SearchQuery → SearchResponse with wire results.
        let response = handler
            .handle_message(NetworkMessage::SearchQuery {
                query: "quantum".to_string(),
                sender: PublicKey([1u8; 32]),
            })
            .expect("search responds");
        match response {
            NetworkMessage::SearchResponse { query, results } => {
                assert_eq!(query, "quantum");
                assert!(!results.is_empty(), "indexed bundle must surface");
                assert_eq!(results[0].title, "Quantum Site");
                assert!(results[0].snippet.contains("Content ID:"));
            }
            other => panic!("expected SearchResponse, got {other:?}"),
        }

        // FetchArtifact (known id) → ArtifactResponse with the stored bytes.
        let response = handler
            .handle_message(NetworkMessage::FetchArtifact {
                content_id: stored_cid,
            })
            .expect("fetch responds");
        match response {
            NetworkMessage::ArtifactResponse {
                content_id,
                bundle_bytes,
            } => {
                assert_eq!(content_id, stored_cid);
                assert_eq!(&bundle_bytes[0..4], b"PWB1");
            }
            other => panic!("expected ArtifactResponse, got {other:?}"),
        }

        // FetchArtifact (unknown id) → empty payload, still a response.
        let response = handler
            .handle_message(NetworkMessage::FetchArtifact {
                content_id: pwb_core::crypto::ContentId([7u8; 32]),
            })
            .expect("fetch responds even for misses");
        match response {
            NetworkMessage::ArtifactResponse { bundle_bytes, .. } => {
                assert!(bundle_bytes.is_empty());
            }
            other => panic!("expected ArtifactResponse, got {other:?}"),
        }

        // Unrelated messages are not handled.
        assert!(
            handler
                .handle_message(NetworkMessage::GossipAnnounce {
                    content_id: cid,
                    title: "t".into(),
                    publisher: PublicKey([2u8; 32]),
                    signature: pwb_core::crypto::Signature([0; 64]),
                })
                .is_none(),
            "gossip is out of scope for the system handler"
        );
    }

    #[test]
    fn test_devtools_page_with_bundle_shows_dom_tree() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();
        let handler = SystemDomainHandler::new(&search, &host, &registry, &identity);

        let mut root = pwb_core::spec::PlainNode::new(0, pwb_core::spec::Tag::Document);
        let mut title = pwb_core::spec::PlainNode::new(1, pwb_core::spec::Tag::Title);
        title.text_content = Some("Sample".into());
        root.children.push(title);
        let bundle = pwb_web::bundle::create_bundle(
            root,
            "Sample Title".to_string(),
            None,
            None,
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        );

        let page = handler.render_system_devtools_page(Some(&bundle));
        assert_eq!(page.title, "devtools.plain — Developer Tools Inspector");
        // Nested nodes appear through render_node_inspector's recursion:
        // the bundle info card + tree rows become Text nodes in the page.
        fn count_tag(n: &pwb_core::spec::PlainNode, tag: pwb_core::spec::Tag) -> usize {
            let me = usize::from(n.tag == tag);
            me + n.children.iter().map(|c| count_tag(c, tag)).sum::<usize>()
        }
        assert!(
            count_tag(&page.root_node, pwb_core::spec::Tag::Text) >= 2,
            "bundle info + tree rows become Text nodes"
        );
    }

    #[test]
    fn test_search_page_renders_result_cards_when_hits_exist() {
        let mut search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        let mut root = pwb_core::spec::PlainNode::new(0, pwb_core::spec::Tag::Document);
        let mut body = pwb_core::spec::PlainNode::new(1, pwb_core::spec::Tag::Text);
        body.text_content = Some("decentralized search engine".into());
        root.children.push(body);
        let bundle = pwb_web::bundle::create_bundle(
            root,
            "Searchable".to_string(),
            None,
            None,
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        );
        let cid = pwb_core::crypto::ContentId::compute(b"searchable-bytes");
        search.index_bundle(cid, &bundle);

        let handler = SystemDomainHandler::new(&search, &host, &registry, &identity);
        let page = handler.render_system_search_page("decentralized");
        assert!(page.title.contains("decentralized"));

        // The non-empty branch emits per-result cards with Visit buttons.
        fn find_button(n: &pwb_core::spec::PlainNode) -> bool {
            if n.tag == pwb_core::spec::Tag::Button {
                return true;
            }
            n.children.iter().any(find_button)
        }
        assert!(
            find_button(&page.root_node),
            "result cards include a Visit Site button"
        );
    }
}
