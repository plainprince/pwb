/// System Domain and URI Resolver (`pwb-browser::resolver`).
///
/// Naming rule: `.plain` is the **official** namespace. The `.plain` TLD is
/// strictly reserved — it holds the built-in system protocols (`search.plain`,
/// `register.plain`, `gossip.plain`, `nodes.plain`, `network.plain`,
/// `help.plain`, `chat.plain`, `devtools.plain`) and the
/// browser-shipped content pages (`welcome.plain`, `demo.plain`). The
/// **unofficial** namespace is the [`pwb_core::system::UNOFFICIAL_TLDS`] list
/// (one entry per claimable TLD — see `docs/tlds.md`): user/network sites
/// claimed by participants (e.g. `mysite.pwb`, `club.non`). The bare `devtools`
/// domain is a special internal browser tool with no TLD.
///
/// URL matching is centralized in `pwb-core::system` ([`SystemUrl`] /
/// [`SystemDomain`]); this module only picks the native page to render for
/// each recognized domain.
use pwb_core::crypto::ContentId;
use pwb_core::system::{is_unofficial_domain, SystemDomain, SystemUrl};
use pwb_node::domain::DomainRegistry;
use pwb_node::host::ArtifactHost;
use pwb_node::identity::IdentityManager;
use pwb_node::search::SearchEngine;
use pwb_node::system::{ChatSnapshot, NetworkSnapshot, RoomSnapshot, SystemDomainHandler};
use std::fs;

#[derive(Debug)]
pub enum ResolverError {
    DomainNotFound(String),
    SystemError(String),
    Io(std::io::Error),
}

impl From<std::io::Error> for ResolverError {
    fn from(err: std::io::Error) -> Self {
        ResolverError::Io(err)
    }
}

/// Extract the `q=` value when `target` addresses `search.plain` (or a bare
/// `search`), else `None`.
pub fn search_query_of(target: &str) -> Option<String> {
    let url = SystemUrl::parse(target)?;
    (url.domain() == SystemDomain::Search)
        .then(|| url.search_query())
        .flatten()
}

/// Whether `target` addresses a reserved `.plain` system domain (which must not
/// be published to the search index).
pub fn is_system_target(target: &str) -> bool {
    SystemUrl::parse(target).is_some()
}

/// Whether `target` addresses `register.plain` (the live domain-claim console).
pub fn is_register_target(target: &str) -> bool {
    SystemUrl::parse(target)
        .map(|url| url.domain() == SystemDomain::Register)
        .unwrap_or(false)
}

/// Whether `target` addresses `chat.plain` (the direct peer chat page).
pub fn is_chat_target(target: &str) -> bool {
    SystemUrl::parse(target)
        .map(|url| url.domain() == SystemDomain::Chat)
        .unwrap_or(false)
}

/// The public room a `chat.plain?room=<name>` URL addresses, normalized the
/// way the wire expects it. `None` for other URLs — including private
/// `chat.plain?peer=<hex>` conversations.
pub fn room_of(target: &str) -> Option<String> {
    SystemUrl::parse(target)?.room()
}

/// The `(domain, cid)` pair when `target` is a `register.plain?claim=…&cid=…`
/// claim submission.
pub fn register_claim_of(target: &str) -> Option<(String, String)> {
    let url = SystemUrl::parse(target)?;
    (url.domain() == SystemDomain::Register).then(|| {
        let domain = url.param("claim")?;
        let cid = url.param("cid")?;
        Some((domain, cid))
    })?
}

/// The content id addressed by a `content.pwb/<hex-cid>` URL.
pub fn content_id_of(target: &str) -> Option<ContentId> {
    let clean = target.trim().trim_start_matches("plain://");
    let hex_cid = clean.strip_prefix("content.pwb/")?;
    let bytes = hex::decode(hex_cid.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut cid = [0u8; 32];
    cid.copy_from_slice(&bytes);
    Some(ContentId(cid))
}

/// The bare `name.tld` when `target` is a claimable user domain of the
/// unofficial namespace (see [`is_unofficial_domain`]) — not a `file.pwb/`
/// path or a `content.pwb/<cid>` lookup.
pub fn pwb_name_of(target: &str) -> Option<String> {
    let clean = target.trim().trim_start_matches("plain://");
    (is_unofficial_domain(clean)
        && !clean.starts_with("file.pwb/")
        && !clean.starts_with("content.pwb/"))
    .then(|| clean.to_string())
}

/// Resolve a URL or domain string to raw `.pwb` binary bundle bytes.
pub fn resolve_url_or_file(
    target: &str,
    search: &SearchEngine,
    host: &ArtifactHost,
    registry: &DomainRegistry,
    identity: &IdentityManager,
    balance: u64,
) -> Result<Vec<u8>, ResolverError> {
    resolve_url_or_file_with_context(
        target,
        search,
        host,
        registry,
        identity,
        balance,
        &[],
        NetworkSnapshot::default(),
        ChatSnapshot::default(),
        RoomSnapshot::default(),
    )
}

/// Resolve a URL or domain string, passing the live peer table to the native
/// `nodes.plain` page so it can show real peers instead of a static card.
pub fn resolve_url_or_file_with_peers(
    target: &str,
    search: &SearchEngine,
    host: &ArtifactHost,
    registry: &DomainRegistry,
    identity: &IdentityManager,
    balance: u64,
    peers: &[(pwb_core::crypto::PublicKey, std::net::SocketAddr)],
) -> Result<Vec<u8>, ResolverError> {
    resolve_url_or_file_with_context(
        target,
        search,
        host,
        registry,
        identity,
        balance,
        peers,
        NetworkSnapshot {
            peers: peers.to_vec(),
            ..NetworkSnapshot::default()
        },
        ChatSnapshot::default(),
        RoomSnapshot::default(),
    )
}

/// Resolve a URL or domain string with the caller's live network view.
///
/// `network` feeds `network.plain`; its `peers` field is replaced with
/// `peers` so the two views cannot disagree. `chat` feeds `chat.plain`
/// (transcripts plus the last send error); `room` feeds
/// `chat.plain?room=<name>` (the public room transcript and roster).
#[allow(clippy::too_many_arguments)]
pub fn resolve_url_or_file_with_context(
    target: &str,
    search: &SearchEngine,
    host: &ArtifactHost,
    registry: &DomainRegistry,
    identity: &IdentityManager,
    balance: u64,
    peers: &[(pwb_core::crypto::PublicKey, std::net::SocketAddr)],
    network: NetworkSnapshot,
    chat: ChatSnapshot,
    room: RoomSnapshot,
) -> Result<Vec<u8>, ResolverError> {
    let clean = target.trim().trim_start_matches("plain://");

    // 1. Official built-in pages (e.g. `welcome.plain`, `demo.plain`, `devtools`).
    // These live in `webpages/` inside pwb-browser and are compiled on-the-fly via pwb-web.
    if let Some(bytes) = crate::builtin_sites::compile_builtin_page_bytes(clean) {
        return Ok(bytes);
    }

    // 2. Reserved System Domains (.plain TLD) — matching centralized in pwb-core::system.
    if let Some(system_url) = SystemUrl::parse(clean) {
        let handler = SystemDomainHandler::new(search, host, registry, identity, balance)
            .with_chat(chat)
            .with_room(room);
        let bundle = match system_url.domain() {
            SystemDomain::Search => {
                let query = system_url.search_query().unwrap_or_default();
                handler.render_system_search_page(&query)
            }
            SystemDomain::Register => handler.render_system_register_page(identity.public_key()),
            // The interactive devtools panel normally mounts from `webpages/devtools`
            // (step 1); this is the standalone fallback page.
            SystemDomain::DevTools => handler.render_system_devtools_page(None),
            SystemDomain::Gossip => handler.render_system_gossip_page(),
            // `plain://nodes.plain?connect=<ip:port>/<pubkey>` links render as a
            // native connect card; bare nodes.plain shows the peer registry.
            SystemDomain::Nodes => {
                handler.render_system_nodes_page(system_url.connection_target(), peers)
            }
            // `network.plain` reports this node's live addresses and peer
            // table. Callers that know the socket addresses pass them in;
            // otherwise the page falls back to the peer list alone.
            SystemDomain::Network => handler
                .with_network(NetworkSnapshot {
                    peers: peers.to_vec(),
                    ..network
                })
                .render_system_network_page(),
            SystemDomain::Help => handler.render_system_help_page(),
            // `chat.plain` renders the peer picker; `chat.plain?peer=<hex>`
            // renders that peer's transcript plus the composer, and
            // `chat.plain?room=<name>` the public room.
            SystemDomain::Chat => match system_url.room() {
                Some(room) => handler.render_system_room_page(&room, peers),
                None => handler.render_system_chat_page(system_url.chat_peer(), peers),
            },
            // `blog.plain` is reserved by the protocol before its feed page
            // exists; say so plainly instead of rendering an empty feed that
            // would look like a blog with no posts.
            SystemDomain::Blog => pwb_node::system::render_message_bundle(
                SystemDomain::Blog.as_str(),
                "The global feed is part of the protocol; this page is not wired up yet.",
            ),
        };
        // Native system pages ship in the same compressed container as
        // authored sites; `Bundle::deserialize` accepts both forms.
        return Ok(pwb_web::compress::compress_bundle(&bundle));
    }

    // 3. Explicit File System Access (e.g. `file.pwb/path/to/file.pwb`)
    if clean.starts_with("file.pwb/") {
        let path = clean.trim_start_matches("file.pwb/");
        let bytes = fs::read(path)?;
        return Ok(bytes);
    }

    // 3b. Content-addressed lookup (`content.pwb/<hex-cid>`): resolve a bundle
    // from the local artifact host by content id. Search-result "Visit Site"
    // buttons navigate here after the federated query has cached the bundle.
    if clean.starts_with("content.pwb/") {
        let bytes = content_id_of(clean).and_then(|cid| host.get(&cid));
        return match bytes {
            Some(bytes) => Ok(bytes),
            None => Err(ResolverError::DomainNotFound(format!(
                "{clean} is not in the local store"
            ))),
        };
    }

    // 4. Unofficial user network domains (any dotted name consults the
    // registry; the unofficial TLDs are what the browser treats as a
    // claimable address in `pwb_name_of`).
    if clean.contains('.') {
        if let Some(record) = registry.resolve(clean) {
            if let Some(bundle_bytes) = host.get(&record.target_content_id) {
                return Ok(bundle_bytes);
            }
        }
    }

    // 5. Fallback to local artifact file path (e.g. `path/to/site.pwb`)
    let bytes = fs::read(target)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_domain_resolver_security() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        // Built-in system domain search.plain MUST resolve to system search bundle
        let bytes = resolve_url_or_file(
            "search.plain?q=untrackable",
            &search,
            &host,
            &registry,
            &identity,
            0,
        )
        .unwrap();

        assert!(bytes.len() > 50);
        assert!(pwb_core::wire::Bundle::is_compressed(&bytes));
        assert!(pwb_core::wire::Bundle::deserialize(&bytes).is_ok());

        // search.plain with no query should resolve to the search home screen
        let bytes =
            resolve_url_or_file("search.plain", &search, &host, &registry, &identity, 0).unwrap();
        assert!(bytes.len() > 50);
        assert!(pwb_core::wire::Bundle::is_compressed(&bytes));
        assert!(pwb_core::wire::Bundle::deserialize(&bytes).is_ok());
    }

    #[test]
    fn test_welcome_plain_resolves_to_builtin_page() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        // Official built-in pages compile from webpages/<name>.plain sources
        // through pwb-web at resolve time (not a hand-built Rust DOM), and are
        // reachable by bare name and by canonical `.plain` name.
        for target in ["welcome.plain", "welcome", "demo.plain", "demo"] {
            let bytes = resolve_url_or_file(target, &search, &host, &registry, &identity, 0)
                .unwrap_or_else(|e| panic!("{target} should resolve: {e:?}"));
            assert!(
                pwb_core::wire::Bundle::is_compressed(&bytes),
                "{target} should be a compressed PWBZ bundle"
            );
            assert!(
                pwb_core::wire::Bundle::deserialize(&bytes).is_ok(),
                "{target} must decode"
            );
        }
    }

    #[test]
    fn test_all_native_system_domains_resolve() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        for target in [
            "search.plain?q=untrackable",
            "search.plain",
            "register.plain",
            "gossip.plain",
            "nodes.plain",
            "network.plain",
            "help.plain",
            "devtools.plain",
            // bare aliases and the nodes.plain connection-link form
            "search?q=plainweb",
            "plain://nodes.plain?connect=127.0.0.1:9000/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            let bytes = resolve_url_or_file(target, &search, &host, &registry, &identity, 0)
                .unwrap_or_else(|e| panic!("{target} should resolve: {e:?}"));
            assert!(
                bytes.len() > 50
                    && pwb_core::wire::Bundle::is_compressed(&bytes)
                    && pwb_core::wire::Bundle::deserialize(&bytes).is_ok(),
                "{target} should produce a decodable PWBZ bundle"
            );
        }
    }

    #[test]
    fn test_network_and_help_pages_resolve_natively() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        // Both reserved domains that used to error now render native pages.
        for (target, expected_title) in [
            ("network.plain", "network.plain — Network Status"),
            ("help.plain", "help.plain — PlainWeb Help"),
        ] {
            let bytes = resolve_url_or_file(target, &search, &host, &registry, &identity, 0)
                .unwrap_or_else(|e| panic!("{target} should resolve: {e:?}"));
            let bundle = pwb_core::wire::Bundle::deserialize(&bytes)
                .unwrap_or_else(|e| panic!("{target} must decode: {e:?}"));
            assert_eq!(bundle.title, expected_title);
        }
    }

    #[test]
    fn test_file_pwb_path_reads_local_file() {
        use pwb_core::spec::{PlainNode, Tag};
        use pwb_core::wire::{Bundle, CURRENT_VERSION};
        use std::collections::HashMap;

        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let mut registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        let payload = Bundle {
            version: CURRENT_VERSION,
            title: "disk".into(),
            author_pubkey: None,
            author_signature: None,
            root_node: PlainNode::new(0, Tag::Document),
            script_payload: None,
            components: HashMap::new(),
            modules: HashMap::new(),
        }
        .serialize();

        let dir = std::env::temp_dir().join(format!("pwb-resolver-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bundle.pwb");
        std::fs::write(&file, &payload).unwrap();

        // Absolute path after the file.pwb/ prefix so the read is CWD-independent.
        let rel = format!("file.pwb/{}", file.display());
        let bytes = resolve_url_or_file(&rel, &search, &host, &registry, &identity, 0)
            .expect("file.pwb/ branch reads from disk");
        assert_eq!(bytes, payload);

        // Local-path fallback (step 5): an existing path ending in .pwb that
        // is not a registered domain falls through to a filesystem read.
        let abs = file.display().to_string();
        let bytes = resolve_url_or_file(&abs, &search, &host, &registry, &identity, 0)
            .expect("unregistered .pwb path falls back to disk");
        assert_eq!(bytes, payload);

        // A stored artifact resolves through the registry branch (step 4).
        let content_id = host.store(payload.clone()).expect("stores");
        registry
            .register("mysite.pwb", content_id, identity.public_key())
            .expect("registers");
        let bytes = resolve_url_or_file("mysite.pwb", &search, &host, &registry, &identity, 0)
            .expect("registered domain resolves from the host");
        assert_eq!(bytes, payload);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_content_pwb_resolves_from_host_by_cid() {
        use pwb_core::spec::{PlainNode, Tag};
        use pwb_core::wire::{Bundle, CURRENT_VERSION};
        use std::collections::HashMap;

        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        let payload = Bundle {
            version: CURRENT_VERSION,
            title: "hit".into(),
            author_pubkey: None,
            author_signature: None,
            root_node: PlainNode::new(0, Tag::Document),
            script_payload: None,
            components: HashMap::new(),
            modules: HashMap::new(),
        }
        .serialize();
        let content_id = host.store(payload.clone()).expect("stores");

        // A cached bundle (as prefetched from a federated search) resolves by cid.
        let target = format!("content.pwb/{}", content_id.to_hex());
        let bytes = resolve_url_or_file(&target, &search, &host, &registry, &identity, 0)
            .expect("content.pwb/<cid> resolves from the host");
        assert_eq!(bytes, payload);

        // An unknown / malformed cid is a DomainNotFound, not a filesystem miss.
        let err = resolve_url_or_file(
            "content.pwb/deadbeef",
            &search,
            &host,
            &registry,
            &identity,
            0,
        )
        .expect_err("unknown cid must error");
        assert!(
            matches!(err, ResolverError::DomainNotFound(_)),
            "expected DomainNotFound, got {err:?}"
        );
    }

    #[test]
    fn test_register_target_and_claim_parsing() {
        assert!(is_register_target("register.plain"));
        assert!(is_register_target(
            "register.plain?claim=alice.pwb&cid=abcd"
        ));
        assert!(!is_register_target("search.plain?q=x"));

        assert_eq!(
            register_claim_of("register.plain?claim=alice.pwb&cid=deadbeef"),
            Some(("alice.pwb".to_string(), "deadbeef".to_string()))
        );
        assert_eq!(register_claim_of("register.plain"), None);
        assert_eq!(register_claim_of("register.plain?claim=alice.pwb"), None);

        assert_eq!(pwb_name_of("alice.pwb"), Some("alice.pwb".to_string()));
        assert_eq!(pwb_name_of("plain://bob.pwb"), Some("bob.pwb".to_string()));
        assert_eq!(pwb_name_of("file.pwb/site.pwb"), None);
        assert_eq!(pwb_name_of("content.pwb/deadbeef"), None);
        assert_eq!(pwb_name_of("welcome.plain"), None);
        // Every unofficial TLD is a claimable address (docs/tlds.md).
        for tld in pwb_core::system::UNOFFICIAL_TLDS {
            assert_eq!(
                pwb_name_of(&format!("site.{tld}")),
                Some(format!("site.{tld}")),
                ".{tld} is claimable"
            );
        }
        // Names outside the namespace are not addresses.
        assert_eq!(pwb_name_of("site.xyz"), None);
        assert_eq!(pwb_name_of("nodot"), None);
    }

    #[test]
    fn test_content_id_of_parses_only_content_urls() {
        let cid = ContentId([0xab; 32]);
        let target = format!("content.pwb/{}", cid.to_hex());
        assert_eq!(content_id_of(&target), Some(cid));
        assert_eq!(content_id_of("content.pwb/deadbeef"), None);
        assert_eq!(content_id_of("mysite.pwb"), None);
    }

    #[test]
    fn test_missing_local_file_is_io_error() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();
        let err = resolve_url_or_file(
            "no-such-file-xyz123",
            &search,
            &host,
            &registry,
            &identity,
            0,
        )
        .expect_err("missing file must error");
        assert!(
            matches!(err, ResolverError::Io(_)),
            "expected Io error, got {err:?}"
        );
    }
}
