//! `network.plain` status page and `help.plain` index.
//!
//! Both are rendered from live local state: `network.plain` reports this
//! node's listen/discovery/STUN addresses, its live peer table, and the size
//! of the services it is running; `help.plain` links every native system
//! domain and explains the namespace and credit rules.

use super::handler::NetworkSnapshot;
use super::registry_page::{escape, short_hex};
use super::SystemDomainHandler;
use pwb_core::system::SystemDomain;
use pwb_core::wire::Bundle;

const NETWORK_STYLES: &str = r#"
.net-container {
    background-color: #1e1e2e;
    padding: 25px;
    gap: 15px;
}
.net-title {
    color: #89b4fa;
    font-size: 26px;
}
.net-info {
    color: #a6adc8;
    font-size: 15px;
}
.net-card {
    background-color: #313244;
    padding: 15px;
    border-radius: 6px;
    gap: 8px;
}
.net-heading {
    color: #a6e3a1;
    font-size: 19px;
}
.net-row {
    color: #bac2de;
    font-size: 14px;
}
.net-peer {
    color: #cdd6f4;
    font-size: 14px;
}
.net-empty {
    color: #6c7086;
    font-size: 14px;
}
.net-hint {
    color: #6c7086;
    font-size: 14px;
}
.net-link {
    color: #fab387;
    font-size: 15px;
    padding: 0;
}
"#;

const HELP_STYLES: &str = r#"
.help-container {
    background-color: #1e1e2e;
    padding: 25px;
    gap: 15px;
}
.help-title {
    color: #89b4fa;
    font-size: 26px;
}
.help-intro {
    color: #a6adc8;
    font-size: 15px;
}
.help-card {
    background-color: #313244;
    padding: 15px;
    border-radius: 6px;
    gap: 8px;
}
.help-heading {
    color: #a6e3a1;
    font-size: 19px;
}
.help-row {
    color: #bac2de;
    font-size: 14px;
}
.help-link {
    color: #fab387;
    font-size: 15px;
    padding: 0;
}
"#;

/// Render `network.plain`: a live snapshot of this node's connectivity and
/// the services it is currently running.
#[allow(clippy::too_many_arguments)]
pub fn render_network_page_bundle(
    identity: pwb_core::crypto::PublicKey,
    network: &NetworkSnapshot,
    document_count: usize,
    term_count: usize,
    artifact_count: usize,
    domain_count: usize,
    claim_cost: u64,
    balance: u64,
) -> Bundle {
    let title = format!("{} — Network Status", SystemDomain::Network.as_str());
    let addr = |value: Option<std::net::SocketAddr>| match value {
        Some(addr) => escape(&addr.to_string()),
        None => "not available".to_string(),
    };

    let mut pml_markup = format!(
        r##"<plain>
            <head><title>network.plain</title></head>
            <view class="net-container">
                <text class="net-title">network.plain — PlainWeb Network Status</text>
                <text class="net-info">This node: {identity}</text>
                <view class="net-card">
                    <text class="net-heading">Connectivity</text>
                    <text class="net-row">Listen address: {listen}</text>
                    <text class="net-row">LAN discovery: {discovery}</text>
                    <text class="net-row">Public address (STUN): {public}</text>
                    <text class="net-row">Live peers: {peers}</text>
                </view>
                <view class="net-card">
                    <text class="net-heading">Local services</text>
                    <text class="net-row">Indexed documents: {documents}</text>
                    <text class="net-row">Indexed terms: {terms}</text>
                    <text class="net-row">Hosted artifacts: {artifacts}</text>
                    <text class="net-row">Registered domains: {domains}</text>
                    <text class="net-row">Claim price: {cost} credits</text>
                    <text class="net-row">Spendable balance: {balance} credits</text>
                </view>
                <view class="net-card">
                    <text class="net-heading">Peers</text>
        "##,
        identity = identity,
        listen = addr(network.listen_addr),
        discovery = addr(network.discovery_addr),
        public = addr(network.public_addr),
        peers = network.peers.len(),
        documents = document_count,
        terms = term_count,
        artifacts = artifact_count,
        domains = domain_count,
        cost = claim_cost,
        balance = balance,
    );

    if network.peers.is_empty() {
        pml_markup.push_str(
            r##"<text class="net-empty">No peers yet. LAN discovery or a connection link will populate this table.</text>"##,
        );
    } else {
        for (pubkey, address) in &network.peers {
            pml_markup.push_str(&format!(
                r##"<text class="net-peer">{} at {}</text>"##,
                short_hex(&pubkey.0, 8),
                escape(&address.to_string()),
            ));
        }
    }
    pml_markup.push_str(r##"</view>"##);

    pml_markup.push_str(
        r##"<text class="net-hint">Pair with another node using a connection link, or manage peers on</text>
                <link class="net-link" href="nodes.plain">nodes.plain</link>"##,
    );
    pml_markup.push_str(r##"</view></plain>"##);

    compile(title, &pml_markup, NETWORK_STYLES, None)
}

/// Render `help.plain`: the entry point for the reserved system domains plus
/// the namespace and credit rules a new participant needs.
pub fn render_help_page_bundle() -> Bundle {
    let title = format!("{} — PlainWeb Help", SystemDomain::Help.as_str());
    // The namespace row is derived from UNOFFICIAL_TLDS so the page can never
    // drift from the claimable namespace (adding a TLD is a one-line change).
    let tlds = pwb_core::system::unofficial_tld_list();
    let pml_markup = format!(
        r##"<plain>
        <head><title>help.plain</title></head>
        <view class="help-container">
            <text class="help-title">help.plain — PlainWeb Help</text>
            <text class="help-intro">PlainWeb is a serverless web: every page is a content-addressed bundle served by the nodes that host or index it. These native pages are built into the client and never travel over the network.</text>

            <view class="help-card">
                <text class="help-heading">System domains</text>
                <link class="help-link" href="search.plain">search.plain — query the swarm's sharded index</link>
                <link class="help-link" href="register.plain">register.plain — claim an unofficial name and view credits</link>
                <link class="help-link" href="nodes.plain">nodes.plain — live peers and connection links</link>
                <link class="help-link" href="gossip.plain">gossip.plain — hosted artifacts and registry size</link>
                <link class="help-link" href="network.plain">network.plain — this node's addresses and services</link>
                <link class="help-link" href="devtools">devtools — inspect the current page's DOM and layout</link>
            </view>

            <view class="help-card">
                <text class="help-heading">Namespaces</text>
                <text class="help-row">.plain is reserved for official system pages and browser-shipped content and can never be claimed.</text>
                <text class="help-row">{tlds} are the unofficial namespace: names are signed claims backed by earned credits and gossiped across the swarm.</text>
                <text class="help-row">content.pwb/&lt;content-id&gt; addresses a bundle directly by its BLAKE3 hash, with no registry lookup.</text>
            </view>

            <view class="help-card">
                <text class="help-heading">Hosting and sharing</text>
                <text class="help-row">1. Compile a site directory into a .pwb bundle and publish it from a node running with --host.</text>
                <text class="help-row">2. Claim a name (e.g. mysite.pwb or club.non) on register.plain, pointing it at the bundle's content ID.</text>
                <text class="help-row">3. Share a plain://nodes.plain?connect=&lt;ip:port&gt;/&lt;pubkey&gt; link so peers can dial you.</text>
                <text class="help-row">Once published, peers that index the bundle can serve search results and fetch it from you.</text>
            </view>

            <view class="help-card">
                <text class="help-heading">Credits</text>
                <text class="help-row">Credits are earned from verified service: answering search shard queries, indexing bundles, and relaying onion cells.</text>
                <text class="help-row">Every earning is recorded in a signed hash-chained ledger; neighboring nodes attest balances so claims cannot self-declare credit.</text>
                <text class="help-row">A domain claim spends credits. Attested claims and blind (unlinkable) vouchers are available from the pwb-node CLI.</text>
            </view>
        </view>
    </plain>"##
    );

    compile(title, &pml_markup, HELP_STYLES, None)
}

fn compile(title: String, pml_markup: &str, styles: &str, script: Option<&str>) -> Bundle {
    let mut root_node = pwb_web::parse::parse_pml_markup(pml_markup)
        .unwrap_or_else(|error| panic!("native page markup must parse: {error:?}"));
    let rules = pwb_web::parse::parse_pss_stylesheet(styles);
    pwb_web::parse::apply_stylesheet(&mut root_node, &rules);
    pwb_web::bundle::create_bundle(
        root_node,
        title,
        None,
        script.map(|script| script.as_bytes().to_vec()),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    )
}

impl SystemDomainHandler<'_> {
    /// Generate a native bundle for `network.plain` from the handler's live
    /// state plus the attached [`NetworkSnapshot`].
    pub fn render_system_network_page(&self) -> Bundle {
        render_network_page_bundle(
            self.identity.public_key(),
            &self.network,
            self.search.document_count(),
            self.search.term_count(),
            self.host.len(),
            self.registry.count(),
            self.registry.claim_cost(),
            self.balance,
        )
    }

    /// Generate the native `help.plain` index.
    pub fn render_system_help_page(&self) -> Bundle {
        render_help_page_bundle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::DomainRegistry;
    use crate::host::ArtifactHost;
    use crate::identity::IdentityManager;
    use crate::search::SearchEngine;
    use std::net::SocketAddr;

    fn handler<'a>(
        search: &'a SearchEngine,
        host: &'a ArtifactHost,
        registry: &'a DomainRegistry,
        identity: &'a IdentityManager,
    ) -> SystemDomainHandler<'a> {
        SystemDomainHandler::new(search, host, registry, identity, 42)
    }

    #[test]
    fn network_page_renders_defaults_without_addresses() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();
        let bundle = handler(&search, &host, &registry, &identity).render_system_network_page();

        assert_eq!(bundle.title, "network.plain — Network Status");
        let text = format!("{:?}", bundle.root_node);
        assert!(text.contains("not available"), "missing address falls back");
        assert!(text.contains("No peers yet"));
    }

    #[test]
    fn network_page_renders_live_addresses_peers_and_service_counts() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();
        search.record_posting(&pwb_core::net::PostingWire {
            term: "plain".to_string(),
            content_id: pwb_core::crypto::ContentId([3u8; 32]),
            title: "Plain page".to_string(),
            term_frequency: 1,
        });

        let listen: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        let discovery: SocketAddr = "0.0.0.0:7922".parse().unwrap();
        let peer = pwb_core::crypto::PublicKey([7u8; 32]);
        let snapshot = NetworkSnapshot {
            listen_addr: Some(listen),
            discovery_addr: Some(discovery),
            public_addr: None,
            peers: vec![(peer, listen)],
        };
        let bundle = handler(&search, &host, &registry, &identity)
            .with_network(snapshot)
            .render_system_network_page();

        let text = format!("{:?}", bundle.root_node);
        assert!(text.contains("127.0.0.1:9000"), "listen address renders");
        assert!(text.contains("0.0.0.0:7922"), "discovery address renders");
        assert!(text.contains("0707070707070707"), "peer key renders");
        assert!(text.contains("Indexed documents: 1"));
        assert!(text.contains("Indexed terms: 1"));
    }

    #[test]
    fn help_page_lists_every_unofficial_tld() {
        // The namespace row is interpolated from UNOFFICIAL_TLDS, so every
        // claimable TLD must appear in the rendered help page.
        let bundle = render_help_page_bundle();
        let text = format!("{:?}", bundle.root_node);
        for tld in pwb_core::system::UNOFFICIAL_TLDS {
            assert!(
                text.contains(&format!(".{tld}")),
                "help page must list .{tld}"
            );
        }
    }

    #[test]
    fn help_page_links_every_native_domain() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();
        let bundle = handler(&search, &host, &registry, &identity).render_system_help_page();

        assert_eq!(bundle.title, "help.plain — PlainWeb Help");
        let text = format!("{:?}", bundle.root_node);
        for href in [
            "search.plain",
            "register.plain",
            "nodes.plain",
            "gossip.plain",
            "network.plain",
            "devtools",
        ] {
            assert!(text.contains(href), "help page must link {href}");
        }
    }
}
