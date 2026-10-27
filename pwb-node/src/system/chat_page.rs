//! `chat.plain`: direct, end-to-end-encrypted chat with another node.
//!
//! The page has two shapes, both rendered from [`ChatSnapshot`] state:
//!
//! - **Picker** (`chat.plain`): this node's identity plus live peers and
//!   recent contacts, each a link into `chat.plain?peer=<hex>`.
//! - **Conversation** (`chat.plain?peer=<hex>`): the per-peer transcript, a
//!   composer input (Enter or the Send button), and the last send error.
//!
//! Sending goes through [`ChatService`](crate::chat::ChatService), which
//! seals every message with the networking layer's `SendFlags::E2EE` flag —
//! the page itself never touches crypto. The pipeline re-renders the page
//! whenever the service version changes, so inbound messages appear live.

use std::net::SocketAddr;

use pwb_core::crypto::PublicKey;
use pwb_core::system::SystemDomain;
use pwb_core::wire::Bundle;

use super::network_page::compile;
use super::registry_page::{escape, short_hex};
use super::SystemDomainHandler;

/// `HH:MM` (UTC) for a unix timestamp — enough for chat recency without
/// pulling in a date library.
fn clock(unix: u64) -> String {
    let secs_of_day = unix % 86_400;
    format!(
        "{:02}:{:02}",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60
    )
}

pub(super) const CHAT_STYLES: &str = r#"
.chat-container {
    background-color: #1e1e2e;
    padding: 25px;
    gap: 15px;
}
.chat-title {
    color: #89b4fa;
    font-size: 26px;
}
.chat-info {
    color: #a6adc8;
    font-size: 15px;
}
.chat-heading {
    color: #a6e3a1;
    font-size: 19px;
}
.chat-peer-link {
    color: #fab387;
    font-size: 15px;
    padding: 0;
}
.chat-back {
    color: #89b4fa;
    font-size: 14px;
    padding: 0;
}
.chat-peer-key {
    color: #bac2de;
    font-size: 12px;
}
.chat-warn {
    color: #f9e2af;
    font-size: 14px;
}
.chat-error {
    color: #f38ba8;
    font-size: 14px;
}
.chat-line {
    background-color: #313244;
    padding: 8px;
    border-radius: 6px;
    gap: 4px;
}
.chat-who {
    color: #89b4fa;
    font-size: 12px;
}
.chat-who-out {
    color: #a6e3a1;
    font-size: 12px;
}
.chat-text {
    color: #cdd6f4;
    font-size: 15px;
}
.chat-when {
    color: #6c7086;
    font-size: 11px;
}
.chat-hint {
    color: #6c7086;
    font-size: 14px;
}
.chat-composer {
    background-color: #313244;
    padding: 12px;
    border-radius: 8px;
    gap: 10px;
    flex-direction: row;
    align-items: center;
    min-width: 260px;
}
#chat-input {
    flex: 1;
    padding: 10px;
    border-radius: 6px;
    background-color: #1e1e2e;
    color: #cdd6f4;
    font-size: 16px;
}
#chat-send {
    background-color: #a6e3a1;
    color: #11111b;
    padding: 10px 20px;
    border-radius: 6px;
    font-size: 16px;
    cursor: pointer;
}
"#;

impl SystemDomainHandler<'_> {
    /// Render the native `chat.plain` bundle: the peer picker for the bare
    /// domain, or the conversation when the URL carried `?peer=<hex>`.
    pub fn render_system_chat_page(
        &self,
        peer: Option<PublicKey>,
        peers: &[(PublicKey, SocketAddr)],
    ) -> Bundle {
        match peer {
            Some(peer) => render_conversation(self, peer, peers),
            None => render_picker(self, peers),
        }
    }
}

/// The bare `chat.plain` page: who I am, who I can talk to right now, and
/// everyone I have talked to before.
fn render_picker(handler: &SystemDomainHandler<'_>, peers: &[(PublicKey, SocketAddr)]) -> Bundle {
    let title = format!("{} — Direct Peer Chat", SystemDomain::Chat.as_str());
    let local = handler.chat.local;

    let mut rows = String::new();
    let mut listed: Vec<PublicKey> = Vec::new();

    // Public rooms sit above the private conversations: the default square
    // is always listed, plus every room this node has joined, with the
    // members I know about (rosters are one hop, so this is "peers I have
    // heard announce themselves", not a global count).
    let mut rooms: Vec<String> = handler
        .room
        .rooms
        .iter()
        .filter(|(_, state)| state.subscribed)
        .map(|(name, _)| name.clone())
        .collect();
    rooms.sort();
    if !rooms.iter().any(|name| name == crate::room::DEFAULT_ROOM) {
        rooms.insert(0, crate::room::DEFAULT_ROOM.to_string());
    }
    rows.push_str(r##"<text class="chat-heading">Public rooms</text>"##);
    for room in rooms {
        let here = handler.room.subscriber_count(&room);
        rows.push_str(&format!(
            r##"<link class="chat-peer-link" href="chat.plain?room={room}">{room} — public, {here} here</link>"##
        ));
    }

    if peers.is_empty() {
        rows.push_str(
            r##"<text class="chat-hint">No peers yet. Pair with a node on nodes.plain (connection link) to start chatting.</text>"##,
        );
    } else {
        rows.push_str(r##"<text class="chat-heading">Live peers</text>"##);
        for (key, addr) in peers {
            listed.push(*key);
            rows.push_str(&format!(
                r##"<link class="chat-peer-link" href="chat.plain?peer={}">{} at {} — connected</link>"##,
                key.to_hex(),
                short_hex(&key.0, 8),
                escape(&addr.to_string())
            ));
        }
    }

    // Recent contacts who are currently offline, in a stable order.
    let mut contacts: Vec<PublicKey> = handler
        .chat
        .transcripts
        .keys()
        .copied()
        .filter(|key| !listed.contains(key))
        .collect();
    contacts.sort_unstable_by_key(|key| key.0);
    if !contacts.is_empty() {
        rows.push_str(r##"<text class="chat-heading">Recent contacts</text>"##);
        for key in contacts {
            rows.push_str(&format!(
                r##"<link class="chat-peer-link" href="chat.plain?peer={}">{} — offline</link>"##,
                key.to_hex(),
                short_hex(&key.0, 8)
            ));
        }
    }

    let pml_markup = format!(
        r##"<plain>
        <head><title>chat.plain</title></head>
        <view class="chat-container">
            <text class="chat-title">chat.plain — Direct Peer Chat</text>
            <text class="chat-info">This node identity: {}</text>
            <text class="chat-info">Messages are sealed end-to-end (E2EE) and delivered directly to the peer.</text>
            {}
        </view>
    </plain>"##,
        local.to_hex(),
        rows
    );

    compile(title, &pml_markup, CHAT_STYLES, None)
}

/// `chat.plain?peer=<hex>`: the transcript with one peer plus the composer.
fn render_conversation(
    handler: &SystemDomainHandler<'_>,
    peer: PublicKey,
    peers: &[(PublicKey, SocketAddr)],
) -> Bundle {
    let title = format!("chat.plain — chat with {}", short_hex(&peer.0, 8));
    let connected = peers.iter().any(|(key, _)| *key == peer);

    let mut markup = format!(
        r##"<view class="chat-container">
        <text class="chat-title">chat.plain — chatting with {}</text>
        <text class="chat-peer-key">Peer Identity: {}</text>"##,
        short_hex(&peer.0, 8),
        peer.to_hex()
    );
    if !connected {
        markup.push_str(
            r##"<text class="chat-warn">This peer is not connected right now — pair with it on nodes.plain, then send.</text>"##,
        );
    }
    markup.push_str(r##"<link class="chat-back" href="chat.plain">← all chats</link>"##);

    let transcript = handler.chat.transcript(&peer);
    markup.push_str(r##"<view class="chat-transcript">"##);
    if transcript.is_empty() {
        markup.push_str(
            r##"<text class="chat-hint">No messages yet. What you send is sealed with the E2EE flag — only this peer can read it.</text>"##,
        );
    } else {
        for entry in transcript {
            // 🧅 marks a message that travelled over an onion circuit
            // (outbound route, or inbound through the exit).
            let onion = if entry.via_onion { " 🧅" } else { "" };
            // Outbound receipts: ✓ once the peer confirmed, ⚠ when the wait
            // expired. An entry still waiting (and all inbound history)
            // shows nothing rather than a permanent pending marker.
            let receipt = match entry.ack {
                Some(true) => " ✓",
                Some(false) => " ⚠",
                None => "",
            };
            let (who_class, who) = if entry.outbound {
                ("chat-who-out", format!("me 🔒{onion}{receipt}"))
            } else {
                (
                    "chat-who",
                    format!("{}{onion}", short_hex(&entry.from.0, 8)),
                )
            };
            markup.push_str(&format!(
                r##"<view class="chat-line">
                    <text class="{}">{} · {}</text>
                    <text class="chat-text">{}</text>
                </view>"##,
                who_class,
                who,
                clock(entry.sent_at),
                escape(&entry.text)
            ));
        }
    }
    markup.push_str(r##"</view>"##);

    if let Some(error) = &handler.chat.error {
        markup.push_str(&format!(
            r##"<text class="chat-error">{}</text>"##,
            escape(error)
        ));
    }

    markup.push_str(
        r##"<view class="chat-composer">
            <input id="chat-input" placeholder="Type a message..." />
            <button id="chat-send">Send</button>
        </view>"##,
    );
    markup.push_str(r##"</view></plain>"##);

    let pml_markup = format!(
        r##"<plain>
        <head><title>chat.plain</title></head>
        {}"##,
        markup
    );

    // Enter (or Send) hands the text to the pipeline's pending-chat action;
    // the pipeline sends it with SendFlags::E2EE and re-renders this page
    // from the updated transcript.
    let peer_hex = peer.to_hex();
    let js_script = format!(
        r##"console.log("Loaded chat.plain system domain interface.");
var __chatSend = "__pwb_chat_send('{peer_hex}', __pwb_getValue('#chat-input')); __pwb_setValue('#chat-input', '');";
var __chatBtn = document.selector("#chat-send");
if (__chatBtn) {{ __chatBtn.onClick(__chatSend); }}
var __chatInput = document.selector("#chat-input");
if (__chatInput) {{ __chatInput.addEventHandler("enter", __chatSend); }}
"##
    );

    compile(title, &pml_markup, CHAT_STYLES, Some(&js_script))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatEntry, ChatSnapshot};
    use crate::domain::DomainRegistry;
    use crate::host::ArtifactHost;
    use crate::identity::IdentityManager;
    use crate::search::SearchEngine;

    fn handler<'a>(identity: &'a IdentityManager, chat: ChatSnapshot) -> SystemDomainHandler<'a> {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        // The page only reads identity/chat state; leak the per-test services
        // so their lifetimes satisfy the handler borrow.
        let search = Box::leak(Box::new(search));
        let host = Box::leak(Box::new(host));
        let registry = Box::leak(Box::new(registry));
        SystemDomainHandler::new(search, host, registry, identity, 0).with_chat(chat)
    }

    /// The rendered bundle's root DOM node.
    fn root(bundle: &pwb_core::wire::Bundle) -> pwb_core::spec::PlainNode {
        pwb_web::compress::decompress_bundle(&pwb_web::compress::compress_bundle(bundle))
            .expect("bundle round-trips")
            .root_node
    }

    fn find_text(node: &pwb_core::spec::PlainNode, needle: &str) -> bool {
        if node
            .text_content
            .as_deref()
            .is_some_and(|text| text.contains(needle))
        {
            return true;
        }
        node.children.iter().any(|child| find_text(child, needle))
    }

    fn find_attr(node: &pwb_core::spec::PlainNode, key: &str, value: &str) -> bool {
        if node.attributes.get(key).map(String::as_str) == Some(value) {
            return true;
        }
        node.children
            .iter()
            .any(|child| find_attr(child, key, value))
    }

    #[test]
    fn picker_lists_identity_live_peers_and_offline_contacts() {
        let identity = IdentityManager::new();
        let local = identity.public_key();
        let mut chat = ChatSnapshot {
            local,
            ..ChatSnapshot::default()
        };
        let contact = PublicKey([7u8; 32]);
        chat.transcripts.insert(
            contact,
            vec![ChatEntry {
                from: contact,
                id: [0u8; 16],
                sent_at: 0,
                text: "hi".to_string(),
                outbound: false,
                via_onion: false,
                ack: None,
            }],
        );
        let live_addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        let live_key = PublicKey([9u8; 32]);

        let bundle =
            handler(&identity, chat).render_system_chat_page(None, &[(live_key, live_addr)]);
        let root = root(&bundle);

        assert!(find_text(&root, &local.to_hex()));
        assert!(find_attr(
            &root,
            "href",
            &format!("chat.plain?peer={}", live_key.to_hex())
        ));
        assert!(find_attr(
            &root,
            "href",
            &format!("chat.plain?peer={}", contact.to_hex())
        ));
        assert!(find_text(&root, "Live peers"));
        assert!(find_text(&root, "Recent contacts"));
    }

    #[test]
    fn picker_without_peers_hints_at_nodes_plain() {
        let identity = IdentityManager::new();
        let chat = ChatSnapshot {
            local: identity.public_key(),
            ..ChatSnapshot::default()
        };
        let bundle = handler(&identity, chat).render_system_chat_page(None, &[]);
        let root = root(&bundle);
        assert!(find_text(&root, "No peers yet"));
        assert!(find_text(&root, "nodes.plain"));
    }

    #[test]
    fn conversation_renders_the_transcript_composer_and_send_script() {
        let identity = IdentityManager::new();
        let mut chat = ChatSnapshot {
            local: identity.public_key(),
            ..ChatSnapshot::default()
        };
        let local = chat.local;
        let peer = PublicKey([5u8; 32]);
        chat.transcripts.insert(
            peer,
            vec![
                ChatEntry {
                    from: peer,
                    id: [1u8; 16],
                    sent_at: 3_661,
                    text: "hello <there>".to_string(),
                    outbound: false,
                    via_onion: true,
                    ack: None,
                },
                ChatEntry {
                    from: local,
                    id: [2u8; 16],
                    sent_at: 3_700,
                    text: "hi!".to_string(),
                    outbound: true,
                    via_onion: true,
                    ack: Some(true),
                },
                ChatEntry {
                    from: local,
                    id: [3u8; 16],
                    sent_at: 3_800,
                    text: "did this arrive?".to_string(),
                    outbound: true,
                    via_onion: false,
                    ack: Some(false),
                },
            ],
        );
        chat.error = Some("peer is not connected".to_string());

        let bundle = handler(&identity, chat).render_system_chat_page(Some(peer), &[]);
        let root = root(&bundle);

        // Transcript content: the markup-escaped inbound text, outbound
        // lock marker, and the clock rendering of the timestamps.
        assert!(
            find_text(&root, "there"),
            "the inbound transcript line renders"
        );
        assert!(
            !find_text(&root, "<there>"),
            "raw angle brackets must be escaped in the transcript"
        );
        assert!(find_text(&root, "me 🔒 🧅"), "onion marker renders");
        assert!(
            find_text(&root, "me 🔒 🧅 ✓"),
            "a confirmed outbound message carries its receipt"
        );
        assert!(
            find_text(&root, "me 🔒 ⚠"),
            "a send whose receipt never arrived is marked unconfirmed"
        );
        assert!(find_text(&root, "01:01"));
        // Not-connected warning and the recorded send error.
        assert!(find_text(&root, "not connected"));
        // Composer + wiring to the pending-chat action.
        assert!(find_attr(&root, "id", "chat-input"));
        assert!(find_attr(&root, "id", "chat-send"));

        let script = String::from_utf8(
            pwb_web::compress::decompress_bundle(&pwb_web::compress::compress_bundle(&bundle))
                .expect("bundle round-trips")
                .script_payload
                .expect("page has a script"),
        )
        .unwrap();
        assert!(script.contains(&format!("__pwb_chat_send('{}'", peer.to_hex())));
        assert!(script.contains("addEventHandler(\"enter\""));
    }

    #[test]
    fn conversation_without_messages_shows_the_e2ee_hint() {
        let identity = IdentityManager::new();
        let chat = ChatSnapshot {
            local: identity.public_key(),
            ..ChatSnapshot::default()
        };
        let bundle =
            handler(&identity, chat).render_system_chat_page(Some(PublicKey([6u8; 32])), &[]);
        let root = root(&bundle);
        assert!(find_text(&root, "No messages yet"));
        assert!(find_text(&root, "E2EE"));
    }
}
