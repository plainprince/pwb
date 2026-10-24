use super::*;
use crate::domain::DomainRegistry;
use crate::host::ArtifactHost;
use crate::identity::IdentityManager;
use crate::search::SearchEngine;
use pwb_core::crypto::PublicKey;
use pwb_core::net::NetworkMessage;

#[test]
fn test_system_devtools_domain() {
    let search = SearchEngine::new();
    let host = ArtifactHost::new();
    let registry = DomainRegistry::new();
    let identity = IdentityManager::new();

    let handler = SystemDomainHandler::new(&search, &host, &registry, &identity, 100);
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

    let handler = SystemDomainHandler::new(&search, &host, &registry, &identity, 100);
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

    let handler = SystemDomainHandler::new(&search, &host, &registry, &identity, 100);

    // No connection target and no peers: the page renders the pairing hint.
    let bundle = handler.render_system_nodes_page(None, &[]);
    assert_eq!(bundle.title, "nodes.plain — Peer Node Registry");
    assert!(bundle.script_payload.is_some());

    // With a parsed connection-link target and a live peer: the connect card
    // and the peer row both render, and the button gets a click handler.
    let addr: std::net::SocketAddr = "127.0.0.1:9000".parse().unwrap();
    let peer = PublicKey([9u8; 32]);
    let bundle = handler.render_system_nodes_page(Some((addr, peer)), &[(peer, addr)]);
    assert_eq!(bundle.title, "nodes.plain — Peer Node Registry");
    let script = String::from_utf8(bundle.script_payload.unwrap()).unwrap();
    assert!(script.contains("__pwb_connect"));
}

#[test]
fn test_handle_message_search_query_and_fetch_artifact() {
    let search = SearchEngine::new();
    let host = ArtifactHost::new();
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

    let handler = SystemDomainHandler::new(&search, &host, &registry, &identity, 100);

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
    let handler = SystemDomainHandler::new(&search, &host, &registry, &identity, 100);

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
fn test_register_page_renders_claim_form_and_known_domains() {
    let identity = IdentityManager::new();
    let mut registry = DomainRegistry::new();
    let cid = pwb_core::crypto::ContentId([4u8; 32]);
    registry
        .register("alice.pwb", cid, identity.public_key())
        .expect("registers");

    let bundle = render_register_page_bundle(
        identity.public_key(),
        100,
        registry.claim_cost(),
        &registry.records_snapshot(),
        &[(cid, "Alice Site".to_string())],
        Some(cid),
        Some("Claimed alice.pwb"),
    );

    assert_eq!(bundle.title, "register.plain — Domain Claim Registry");
    assert!(bundle.script_payload.is_some());

    // The claim form (domain + content-id inputs and a claim button) and the
    // hosted-artifact filler are present.
    fn has_id(n: &pwb_core::spec::PlainNode, id: &str) -> bool {
        n.attributes.get("id").map(|v| v == id).unwrap_or(false)
            || n.children.iter().any(|c| has_id(c, id))
    }
    assert!(has_id(&bundle.root_node, "reg-domain"), "domain input");
    assert!(has_id(&bundle.root_node, "reg-cid"), "content-id input");
    assert!(has_id(&bundle.root_node, "claim-btn"), "claim button");
    assert!(has_id(&bundle.root_node, "use_0"), "artifact filler button");

    // The status message is interpolated into the page.
    fn has_text_containing(n: &pwb_core::spec::PlainNode, needle: &str) -> bool {
        n.text_content
            .as_deref()
            .map(|t| t.contains(needle))
            .unwrap_or(false)
            || n.children.iter().any(|c| has_text_containing(c, needle))
    }
    assert!(
        has_text_containing(&bundle.root_node, "Claimed alice.pwb"),
        "status message renders"
    );
}

#[test]
fn test_message_bundle_renders_title_and_message() {
    let bundle = render_message_bundle("domain not found", "No node knows that name.");
    assert_eq!(bundle.title, "domain not found");
}

#[test]
fn test_search_page_renders_result_cards_when_hits_exist() {
    let search = SearchEngine::new();
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

    let handler = SystemDomainHandler::new(&search, &host, &registry, &identity, 100);
    let page = handler.render_system_search_page("decentralized");
    assert!(page.title.contains("decentralized"));

    // The non-empty branch emits per-result cards whose title and "Visit Site"
    // affordance are native links to the content-addressed page — no JS needed.
    let href = format!("content.pwb/{}", cid.to_hex());
    fn find_link(n: &pwb_core::spec::PlainNode, href: &str) -> Option<String> {
        if n.tag == pwb_core::spec::Tag::Link
            && n.attributes.get("href").map(String::as_str) == Some(href)
        {
            return n.text_content.clone();
        }
        n.children.iter().find_map(|c| find_link(c, href))
    }
    let label = find_link(&page.root_node, &href).expect("result cards link to the hit");
    assert!(
        label.contains("Searchable"),
        "result link is labelled with the hit's title, got {label:?}"
    );
    assert!(
        page.script_payload.is_none(),
        "native links must not need a script payload"
    );
}
