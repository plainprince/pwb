//! `register.plain`, `gossip.plain`, and `nodes.plain` pages.

use std::net::SocketAddr;

use super::SystemDomainHandler;
use crate::domain::DomainRecord;
use pwb_core::crypto::{ContentId, PublicKey};
use pwb_core::system::SystemDomain;
use pwb_core::wire::Bundle;

/// Style sheet shared by the `register.plain` claim console.
const REGISTER_STYLES: &str = r#"
.reg-container {
    background-color: #1e1e2e;
    padding: 25px;
    gap: 12px;
}
.reg-title {
    color: #89b4fa;
    font-size: 26px;
}
.reg-info {
    color: #a6adc8;
    font-size: 15px;
}
.reg-msg {
    color: #f9e2af;
    font-size: 15px;
}
.reg-card {
    background-color: #313244;
    padding: 15px;
    border-radius: 6px;
    gap: 8px;
}
.reg-label {
    color: #a6e3a1;
    font-size: 18px;
}
.reg-input {
    background-color: #1e1e2e;
    color: #cdd6f4;
    padding: 8px;
    border-radius: 6px;
    font-size: 15px;
}
.reg-btn {
    background-color: #fab387;
    color: #11111b;
    padding: 8px;
    cursor: pointer;
}
.reg-domain {
    color: #bac2de;
    font-size: 14px;
}
.reg-empty {
    color: #6c7086;
    font-size: 14px;
}
"#;

/// Render the `register.plain` claim console: identity/balance summary, a claim
/// form (`domain` + target content id), the list of known domains, and this
/// node's hosted artifacts as one-click content-id fillers.
#[allow(clippy::too_many_arguments)]
pub fn render_register_page_bundle(
    owner: PublicKey,
    balance: u64,
    cost: u64,
    domains: &[DomainRecord],
    artifacts: &[(ContentId, String)],
    prefilled_cid: Option<ContentId>,
    message: Option<&str>,
) -> Bundle {
    let title = "register.plain — Domain Claim Registry".to_string();
    let default_cid = prefilled_cid
        .or_else(|| artifacts.first().map(|(cid, _)| *cid))
        .map(|cid| cid.to_hex())
        .unwrap_or_default();

    let mut pml_markup = format!(
        r##"<plain>
            <head><title>register.plain</title></head>
            <view class="reg-container">
                <text class="reg-title">register.plain — PlainWeb Domain Registry</text>
                <text class="reg-info">Claim an unofficial name (.pwb, .non, .yep, .lam, .arch, .org, .net or .com) for a site you host. Claims are signed by your identity and gossiped across the swarm.</text>
                <text class="reg-info">Identity: {owner}</text>
                <text class="reg-info">Balance: {balance} credits — current price: {cost} credits</text>
                <text class="reg-info">Registered domains: {count}</text>
        "##,
        owner = short_hex(&owner.0, 12),
        balance = balance,
        cost = cost,
        count = domains.len(),
    );

    if let Some(message) = message {
        pml_markup.push_str(&format!(
            r##"<text class="reg-msg">{}</text>"##,
            escape(message)
        ));
    }

    pml_markup.push_str(&format!(
        r##"<view class="reg-card">
                    <text class="reg-label">Claim a new domain</text>
                    <text class="reg-domain">Domain (e.g. mysite.pwb)</text>
                    <input class="reg-input" id="reg-domain" placeholder="mysite.pwb" />
                    <text class="reg-domain">Target content ID (hex)</text>
                    <input class="reg-input" id="reg-cid" value="{}" placeholder="content id" />
                    <button class="reg-btn" id="claim-btn">Claim domain</button>
                </view>"##,
        default_cid
    ));

    pml_markup
        .push_str(r##"<view class="reg-card"><text class="reg-label">Registered domains</text>"##);
    if domains.is_empty() {
        pml_markup.push_str(
            r##"<text class="reg-empty">No domains registered on this node yet.</text>"##,
        );
    } else {
        for record in domains {
            pml_markup.push_str(&format!(
                r##"<text class="reg-domain">{} → {} (owner {})</text>"##,
                escape(&record.domain),
                record.target_content_id.to_hex(),
                short_hex(&record.owner.0, 8),
            ));
        }
    }
    pml_markup.push_str(r##"</view>"##);

    pml_markup.push_str(
        r##"<view class="reg-card"><text class="reg-label">Your hosted artifacts</text>"##,
    );
    if artifacts.is_empty() {
        pml_markup.push_str(
            r##"<text class="reg-empty">No artifacts hosted yet. Publish one, then claim a name for it.</text>"##,
        );
    } else {
        for (idx, (cid, name)) in artifacts.iter().enumerate() {
            pml_markup.push_str(&format!(
                r##"<text class="reg-domain">{}. {} · {}</text>
                    <button class="reg-btn" id="use_{idx}">Use this content ID</button>"##,
                idx + 1,
                escape(name),
                cid.to_hex(),
            ));
        }
    }
    pml_markup.push_str(r##"</view></view></plain>"##);

    let mut script = String::from(
        "var __claimBtn = document.selector(\"#claim-btn\");\n\
         if (__claimBtn) {\n\
           __claimBtn.onClick(\"var d = __pwb_getValue('#reg-domain'); var c = __pwb_getValue('#reg-cid'); if (d && c) { __pwb_navigate('register.plain?claim=' + d + '&cid=' + c); }\");\n\
         }\n",
    );
    for (idx, (cid, _)) in artifacts.iter().enumerate() {
        script.push_str(&format!(
            "var __u{idx} = document.selector(\"#use_{idx}\"); if (__u{idx}) {{ __u{idx}.onClick(\"__pwb_setValue('#reg-cid', '{}')\"); }}\n",
            cid.to_hex()
        ));
    }

    let mut root_node = pwb_web::parse::parse_pml_markup(&pml_markup).unwrap();
    let rules = pwb_web::parse::parse_pss_stylesheet(REGISTER_STYLES);
    pwb_web::parse::apply_stylesheet(&mut root_node, &rules);

    pwb_web::bundle::create_bundle(
        root_node,
        title,
        None,
        Some(script.into_bytes()),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    )
}

/// Render a simple native status/message page (used for claim confirmations and
/// errors from the browser side).
pub fn render_message_bundle(title: &str, message: &str) -> Bundle {
    let pml_markup = format!(
        r##"<plain>
            <head><title>{title}</title></head>
            <view class="reg-container">
                <text class="reg-title">{title}</text>
                <text class="reg-msg">{msg}</text>
            </view>
        </plain>"##,
        title = escape(title),
        msg = escape(message),
    );
    let mut root_node = pwb_web::parse::parse_pml_markup(&pml_markup).unwrap();
    let rules = pwb_web::parse::parse_pss_stylesheet(REGISTER_STYLES);
    pwb_web::parse::apply_stylesheet(&mut root_node, &rules);
    pwb_web::bundle::create_bundle(
        root_node,
        title.to_string(),
        None,
        None,
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    )
}

/// First `n` bytes of a byte string as lowercase hex.
pub(super) fn short_hex(bytes: &[u8], n: usize) -> String {
    bytes.iter().take(n).map(|b| format!("{:02x}", b)).collect()
}

/// Minimal HTML/PML escaping for interpolated user-controlled text.
pub(super) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl<'a> SystemDomainHandler<'a> {
    /// Generate a native bundle for the `register.plain` domain claiming
    /// dashboard. This local fallback renders from the handler's registry; the
    /// networked browser side renders the live service view with
    /// [`render_register_page_bundle`].
    pub fn render_system_register_page(&self, owner_pubkey: PublicKey) -> Bundle {
        render_register_page_bundle(
            owner_pubkey,
            self.balance,
            self.registry.claim_cost(),
            &self.registry.records_snapshot(),
            &self.host.artifacts(),
            None,
            None,
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
                <text class="gossip-hint">Peers receive signed GossipAnnounce messages, fetch the artifact, and index it locally.</text>
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
        peers: &[(PublicKey, SocketAddr)],
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
                <text class="nodes-info">Known peers: {}</text>
            "##,
            local_key,
            peers.len()
        );

        if peers.is_empty() {
            pml_markup.push_str(
                r##"<text class="connect-hint">No peers yet. LAN discovery or a connection link will populate this table.</text>"##,
            );
        } else {
            for (pubkey, addr) in peers {
                pml_markup.push_str(&format!(
                    r##"<view class="connect-card">
                        <text class="connect-addr">Peer: {}</text>
                        <text class="connect-key">Address: {}</text>
                        <text class="connect-key">Public Key: {}</text>
                    </view>"##,
                    short_hex(&pubkey.0, 8),
                    addr,
                    pubkey
                ));
            }
        }

        let mut connect_script = String::new();
        match connect_target {
            Some((addr, pubkey)) => {
                let link = pwb_core::system::build_connection_link(addr, pubkey);
                pml_markup.push_str(&format!(
                    r##"<view class="connect-card">
                        <text class="connect-heading">Connection Link Target</text>
                        <text class="connect-addr">Endpoint: {}</text>
                        <text class="connect-key">Peer Public Key: {}</text>
                        <button id="connect-btn">Connect to Peer</button>
                    </view>"##,
                    addr, pubkey
                ));
                connect_script = format!(
                    "var __connectBtn = document.selector(\"#connect-btn\");\n\
                     if (__connectBtn) {{ __connectBtn.onClick(\"__pwb_connect('{}')\"); }}\n",
                    link
                );
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

        let js_script = format!(
            "console.log(\"Loaded nodes.plain system domain interface.\");\n{connect_script}"
        );

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
