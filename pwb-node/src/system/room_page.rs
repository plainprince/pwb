//! `chat.plain?room=<name>`: the public room view.
//!
//! A room is the shared counterpart to the private [`chat.plain` conversation](super::chat_page):
//! everyone on the network can read it, posts are signed by the identity
//! that wrote them, and every member keeps the history — so this page shows
//! the transcript, who this node knows is in the room, and a composer that
//! hands a line to [`RoomService::post`](crate::room::RoomService::post),
//! which signs and floods it. The pipeline joins the room before rendering,
//! so an open room is always a membership.

use std::net::SocketAddr;

use pwb_core::crypto::PublicKey;
use pwb_core::wire::Bundle;

use super::chat_page::CHAT_STYLES;
use super::network_page::compile;
use super::registry_page::{escape, short_hex};
use super::SystemDomainHandler;

impl SystemDomainHandler<'_> {
    /// Render the native `chat.plain?room=<name>` bundle: the room's
    /// transcript, its roster, and the composer.
    pub fn render_system_room_page(&self, room: &str, peers: &[(PublicKey, SocketAddr)]) -> Bundle {
        let room = escape(room);
        let title = format!("chat.plain — room {room}");
        let state = self.room.state(&room);
        let joined = state.is_some_and(|state| state.subscribed);
        let members = self.room.subscriber_count(&room);

        let mut markup = format!(
            r##"<view class="chat-container">
        <text class="chat-title">chat.plain — public room "{room}"</text>
        <text class="chat-info">This node identity: {}</text>
        <text class="chat-info">Posts are signed by their author and flood the network: every member keeps the history, and anyone can read along.</text>"##,
            self.room.local.to_hex()
        );

        if !joined {
            markup.push_str(
                r##"<text class="chat-warn">Joining this room — announcing membership and asking peers for what you missed.</text>"##,
            );
        }

        let mut roster: Vec<String> = state
            .map(|state| {
                state
                    .subscribers
                    .iter()
                    .map(|key| short_hex(&key.0, 8))
                    .collect()
            })
            .unwrap_or_default();
        roster.sort();
        let roster = if roster.is_empty() {
            "nobody else yet".to_string()
        } else {
            roster.join(", ")
        };
        markup.push_str(&format!(
            r##"<text class="chat-info">{members} here (including you) — known: {roster}</text>"##
        ));

        if peers.is_empty() {
            markup.push_str(
                r##"<text class="chat-warn">No peers connected: posts will only be visible here until you pair with a node on nodes.plain.</text>"##,
            );
        }

        markup.push_str(r##"<link class="chat-back" href="chat.plain">← all chats</link>"##);

        let posts = state.map(|state| state.posts.as_slice()).unwrap_or(&[]);
        markup.push_str(r##"<view class="chat-transcript">"##);
        if posts.is_empty() {
            markup.push_str(
                r##"<text class="chat-hint">No messages yet. Say something — it goes out signed by you and reaches the network through every peer.</text>"##,
            );
        } else {
            for post in posts {
                let (who_class, who) = if post.sender == self.room.local {
                    ("chat-who-out", "me".to_string())
                } else {
                    ("chat-who", short_hex(&post.sender.0, 8))
                };
                markup.push_str(&format!(
                    r##"<view class="chat-line">
                    <text class="{who_class}">{who} · {}</text>
                    <text class="chat-text">{}</text>
                </view>"##,
                    clock(post.sent_at),
                    escape(&post.text)
                ));
            }
        }
        markup.push_str(r##"</view>"##);

        if let Some(error) = &self.room.error {
            markup.push_str(&format!(
                r##"<text class="chat-error">{}</text>"##,
                escape(error)
            ));
        }

        markup.push_str(
            r##"<view class="chat-composer">
            <input id="chat-input" placeholder="Post to the room..." />
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

        // Enter (or Send) hands the line to the pipeline's pending-room
        // action; the pipeline posts it (signed, flooded) and re-renders this
        // page from the updated transcript.
        let js_script = format!(
            r##"console.log("Loaded chat.plain room interface.");
var __roomSend = "__pwb_room_send('{room}', __pwb_getValue('#chat-input')); __pwb_setValue('#chat-input', '');";
var __roomBtn = document.selector("#chat-send");
if (__roomBtn) {{ __roomBtn.onClick(__roomSend); }}
var __roomInput = document.selector("#chat-input");
if (__roomInput) {{ __roomInput.addEventHandler("enter", __roomSend); }}
"##
        );

        compile(title, &pml_markup, CHAT_STYLES, Some(&js_script))
    }
}

/// `HH:MM` (UTC) for a unix timestamp — the same recency clock `chat.plain`
/// uses, so both views read alike.
fn clock(unix: u64) -> String {
    let secs_of_day = unix % 86_400;
    format!(
        "{:02}:{:02}",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::DomainRegistry;
    use crate::host::ArtifactHost;
    use crate::identity::IdentityManager;
    use crate::room::{RoomSnapshot, RoomState, DEFAULT_ROOM};
    use crate::search::SearchEngine;
    use pwb_core::crypto::Keypair;
    use pwb_core::net::{room_post_sign_message, RoomPostWire};

    fn handler<'a>(identity: &'a IdentityManager, room: RoomSnapshot) -> SystemDomainHandler<'a> {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let search = Box::leak(Box::new(search));
        let host = Box::leak(Box::new(host));
        let registry = Box::leak(Box::new(registry));
        SystemDomainHandler::new(search, host, registry, identity, 0).with_room(room)
    }

    /// The rendered bundle's root DOM node.
    fn root(bundle: &Bundle) -> pwb_core::spec::PlainNode {
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

    /// One signed post, as it would arrive from a peer.
    fn post(seed: u8, text: &str, sent_at: u64) -> RoomPostWire {
        let keypair = Keypair::from_seed(&[seed; 32]);
        let mut id = [0u8; 16];
        id[0] = seed;
        RoomPostWire {
            sender: keypair.public,
            room: DEFAULT_ROOM.to_string(),
            id,
            sent_at,
            text: text.to_string(),
            hops: 8,
            signature: keypair.sign(&room_post_sign_message(
                &keypair.public,
                DEFAULT_ROOM,
                &id,
                sent_at,
                text,
            )),
        }
    }

    #[test]
    fn a_room_shows_its_roster_transcript_and_post_script() {
        let identity = IdentityManager::new();
        let peer = Keypair::from_seed(&[5u8; 32]).public;
        let mut room = RoomSnapshot {
            local: identity.public_key(),
            ..RoomSnapshot::default()
        };
        room.rooms.insert(
            DEFAULT_ROOM.to_string(),
            RoomState {
                posts: vec![post(1, "first post", 3_661), post(5, "hi back", 3_700)],
                subscribers: vec![peer],
                subscribed: true,
            },
        );

        let bundle = handler(&identity, room).render_system_room_page(DEFAULT_ROOM, &[]);
        let root = root(&bundle);

        assert!(
            find_text(&root, "public room \"lobby\""),
            "the room is named"
        );
        assert!(find_text(&root, identity.public_key().to_hex().as_str()));
        assert!(find_text(&root, "first post"));
        assert!(find_text(&root, "hi back"));
        assert!(
            find_text(&root, short_hex(&peer.0, 8).as_str()),
            "the roster names the members I know"
        );
        assert!(find_text(&root, "2 here"), "the member count includes me");
        assert!(find_text(&root, "No peers connected"));
        // Composer wired to the pending-room action with this room's name.
        assert!(find_attr(&root, "id", "chat-input"));
        assert!(find_attr(&root, "id", "chat-send"));

        let script = String::from_utf8(
            pwb_web::compress::decompress_bundle(&pwb_web::compress::compress_bundle(&bundle))
                .expect("bundle round-trips")
                .script_payload
                .expect("page has a script"),
        )
        .unwrap();
        assert!(script.contains(&format!("__pwb_room_send('{DEFAULT_ROOM}'")));
        assert!(script.contains("addEventHandler(\"enter\""));
    }

    #[test]
    fn a_room_yet_to_be_joined_says_so_instead_of_looking_empty_forever() {
        let identity = IdentityManager::new();
        let room = RoomSnapshot {
            local: identity.public_key(),
            ..RoomSnapshot::default()
        };

        let bundle = handler(&identity, room).render_system_room_page("news", &[]);
        let root = root(&bundle);

        assert!(find_text(&root, "Joining this room"));
        assert!(find_text(&root, "No messages yet"));
        assert!(find_text(&root, "nobody else yet"));
    }

    #[test]
    fn a_post_failure_from_the_composer_is_shown() {
        let identity = IdentityManager::new();
        let mut room = RoomSnapshot {
            local: identity.public_key(),
            ..RoomSnapshot::default()
        };
        room.error = Some("message is too long (max 2048 bytes)".to_string());

        let bundle = handler(&identity, room).render_system_room_page(DEFAULT_ROOM, &[]);
        assert!(find_text(&root(&bundle), "message is too long"));
    }
}
