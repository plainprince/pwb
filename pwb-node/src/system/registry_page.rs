//! `register.plain`, `gossip.plain`, and `nodes.plain` pages.

use std::net::SocketAddr;

use super::SystemDomainHandler;
use pwb_core::crypto::PublicKey;
use pwb_core::system::SystemDomain;
use pwb_core::wire::Bundle;

impl<'a> SystemDomainHandler<'a> {
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
