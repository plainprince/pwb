//! `pwb-browser` CLI runner — a PlainWeb network node that joins whatever
//! bootstrap/rendezvous peers are configured (`--bootstrap` or
//! `PWB_BOOTSTRAP`), or falls back to LAN discovery when none are. STUN and
//! PeerExchange keep the path dialable and the peer table fresh.

use pwb_browser::pipeline::{build_browser_app, run_event_loop};
use pwb_browser::resolver::{
    content_id_of, is_register_target, is_system_target, pwb_name_of,
    resolve_url_or_file_with_context, search_query_of,
};
use pwb_core::crypto::PublicKey;
use pwb_core::net::{
    peer_exchange_loop, prefer_family, resolve_bootstrap_peer, BootstrapPeer, NetworkMessage,
    NodeConfig, NodeEngine,
};
use pwb_core::onion::CoverConfig;
use pwb_node::domain::{DomainRegistry, DomainService};
use pwb_node::federation::DistributedSearch;
use pwb_node::host::ArtifactHost;
use pwb_node::identity::IdentityManager;
use pwb_node::join;
use pwb_node::search::SearchEngine;
use pwb_node::system::{
    render_message_bundle, render_search_results_bundle_with_status, NetworkSnapshot,
    SystemDomainHandler,
};
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How many top search hits to proactively fetch (so "Visit Site" resolves
/// instantly from the local store).
const PREFETCH_RESULTS: usize = 10;

/// Default STUN server (Google's public STUN service) used for NAT traversal
/// so this node's connection link advertises a dialable public address.
const DEFAULT_STUN_SERVER: &str = "stun.l.google.com:19302";

/// Parse a 32-byte hex public key (an operator-pinned attestor identity).
fn parse_public_key(hex: &str) -> Option<PublicKey> {
    let bytes = hex::decode(hex.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Some(PublicKey(key))
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    // Parse flags
    let mut target = "welcome".to_string();
    let mut connect_target: Option<String> = None;
    let mut stun_server: Option<String> = None;
    let mut no_stun = false;
    let mut bootstrap_peers: Vec<BootstrapPeer> = Vec::new();
    // Raw `--bootstrap` specs, resolved once the listen address (and thus
    // the preferred address family) is known.
    let mut bootstrap_specs: Vec<String> = Vec::new();
    let mut trust_anchors: Vec<PublicKey> = Vec::new();
    let mut join_sources: Vec<String> = Vec::new();
    let mut join_signer: Option<PublicKey> = None;
    let mut no_join = false;
    let mut require_join_signature = false;
    let mut privacy = pwb_node::privacy::PrivacyMode::Onion;
    let mut cover_traffic = false;
    let mut identity_path: Option<PathBuf> = None;
    let mut listen_arg: Option<String> = None;
    let mut i = 1;

    // Help message
    if args.len() == 2 && (args[1] == "-h" || args[1] == "--help") {
        print_help();
        std::process::exit(0);
    }

    while i < args.len() {
        match args[i].as_str() {
            "-f" | "--file" => {
                if i + 1 < args.len() {
                    target = format!("file.pwb/{}", args[i + 1]);
                    i += 2;
                } else {
                    eprintln!("Error: --file flag requires a path argument");
                    std::process::exit(1);
                }
            }
            "-c" | "--connect" => {
                if i + 1 < args.len() {
                    connect_target = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: --connect requires a link argument");
                    std::process::exit(1);
                }
            }
            "--stun-server" => {
                if i + 1 < args.len() {
                    stun_server = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: --stun-server requires a host:port argument");
                    std::process::exit(1);
                }
            }
            "--no-stun" => {
                no_stun = true;
                i += 1;
            }
            "--bootstrap" => {
                if i + 1 < args.len() {
                    bootstrap_specs.push(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: --bootstrap requires a peer argument");
                    std::process::exit(1);
                }
            }
            "--join" => {
                if i + 1 < args.len() {
                    join_sources.push(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: --join requires a URL, file path, or inline JSON");
                    std::process::exit(1);
                }
            }
            "--no-join" => {
                no_join = true;
                i += 1;
            }
            "--require-join-signature" => {
                require_join_signature = true;
                i += 1;
            }
            "--join-signer" => {
                if i + 1 < args.len() {
                    match parse_public_key(&args[i + 1]) {
                        Some(key) => join_signer = Some(key),
                        None => {
                            eprintln!("Error: --join-signer requires a 32-byte hex public key");
                            std::process::exit(1);
                        }
                    }
                    i += 2;
                } else {
                    eprintln!("Error: --join-signer requires a public key");
                    std::process::exit(1);
                }
            }
            "--direct" => {
                privacy = pwb_node::privacy::PrivacyMode::Direct;
                i += 1;
            }
            "--cover-traffic" => {
                cover_traffic = true;
                i += 1;
            }
            "--trust-anchor" => {
                if i + 1 < args.len() {
                    match parse_public_key(&args[i + 1]) {
                        Some(key) => trust_anchors.push(key),
                        None => {
                            eprintln!(
                                "Error: --trust-anchor requires a 32-byte hex attestor public key"
                            );
                            std::process::exit(1);
                        }
                    }
                    i += 2;
                } else {
                    eprintln!("Error: --trust-anchor requires a public key");
                    std::process::exit(1);
                }
            }
            "--identity" => {
                if i + 1 < args.len() {
                    identity_path = Some(PathBuf::from(args[i + 1].clone()));
                    i += 2;
                } else {
                    eprintln!("Error: --identity requires a path argument");
                    std::process::exit(1);
                }
            }
            "--listen" => {
                if i + 1 < args.len() {
                    listen_arg = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: --listen requires a host:port argument");
                    std::process::exit(1);
                }
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            _ => {
                target = args[i].clone();
                i += 1;
            }
        }
    }

    println!("PlainWeb Layer 2 Runtime Browser (pwb-browser)");
    println!("Resolving target / URI: {}", target);

    let identity_path = identity_path
        .or_else(IdentityManager::default_identity_path)
        .expect("no identity path available (set PWB_IDENTITY, or a home directory via $HOME / %USERPROFILE%)");
    let identity = Arc::new(IdentityManager::load_or_create(&identity_path));
    println!("  Node Identity Public Key : {}", identity.public_key());
    println!(
        "  Remote Lookups           : {} (search, content, and .pwb resolution)",
        match privacy {
            pwb_node::privacy::PrivacyMode::Onion => "onion, 3 hops (default; --direct opts out)",
            pwb_node::privacy::PrivacyMode::Direct => "direct (your address is visible)",
        }
    );

    // The browser runs a full embedded node, so its artifacts, index,
    // registry, and signed credit ledger need a profile of their own. Sharing
    // the node's default directory would let two live processes append to the
    // same ledger and flush the same index; `PWB_DATA` still points both at
    // one directory deliberately.
    let data_dir = std::env::var_os("PWB_DATA")
        .map(PathBuf::from)
        .or_else(|| pwb_core::paths::home_dir().map(|home| home.join(".pwb").join("browser")))
        .unwrap_or_else(|| PathBuf::from("pwb-data"));

    // The listen address decides which address family this node can dial, so
    // it is parsed before anything resolves a remote host: bootstrap seeds
    // and the STUN server are both picked to match it, so an IPv6-only node
    // is never handed an IPv4 address it can never reach (and vice versa).
    let listen_addr: SocketAddr = listen_arg
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or("0.0.0.0:0".parse().unwrap());
    let preferred_family = Some(listen_addr.ip());
    for spec in &bootstrap_specs {
        match resolve_bootstrap_peer(spec, preferred_family).await {
            Some(peer) => bootstrap_peers.push(peer),
            None => {
                eprintln!("Error: --bootstrap requires <host:port>/<hex_pubkey>, got {spec}");
                std::process::exit(1);
            }
        }
    }

    // --- Network stack: join the swarm automatically ------------------------
    //
    // STUN (Google by default) discovers our public IP:port through NAT so
    // connection links are dialable from the internet. Bootstrap peers are
    // dialed on startup when configured; without them only LAN discovery is
    // available, so a standalone browser prints that it is standalone.
    //
    // The STUN server may be given as a hostname (`host:port`); resolve it to
    // an IP socket address here. A resolution failure is non-fatal — we simply
    // skip STUN and fall back to the local address.
    let stun_host = stun_server.as_deref().unwrap_or(DEFAULT_STUN_SERVER);
    let effective_stun_server = if no_stun {
        None
    } else {
        match tokio::net::lookup_host(stun_host).await {
            // Query a STUN server of the family this node's socket actually
            // dials: a v4-only socket cannot reach an AAAA-only server. Falls
            // back to any family when none match the preference.
            Ok(addrs) => prefer_family(addrs, preferred_family),
            Err(e) => {
                eprintln!("[Network] Could not resolve STUN server '{stun_host}': {e}");
                None
            }
        }
    };
    // Bootstrap peers come from `--bootstrap` or the comma-separated
    // `PWB_BOOTSTRAP` environment variable. No fake localhost default: a
    // standalone browser should say so instead of pretending it joined.
    let mut env_bootstrap: Vec<BootstrapPeer> = Vec::new();
    if let Ok(value) = std::env::var("PWB_BOOTSTRAP") {
        for entry in value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            match resolve_bootstrap_peer(entry, preferred_family).await {
                Some(peer) => env_bootstrap.push(peer),
                None => eprintln!("[Network] Ignoring unresolvable bootstrap entry '{entry}'"),
            }
        }
    }
    let explicit_bootstrap = !bootstrap_peers.is_empty() || !env_bootstrap.is_empty();
    let mut effective_bootstrap = if bootstrap_peers.is_empty() {
        env_bootstrap
    } else {
        bootstrap_peers
    };
    if let Ok(value) = env::var("PWB_TRUST_ANCHORS") {
        for entry in value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            match parse_public_key(entry) {
                Some(key) => trust_anchors.push(key),
                None => eprintln!("[Trust] Ignoring invalid trust anchor '{entry}'"),
            }
        }
    }
    if let Ok(value) = env::var("PWB_JOIN") {
        join_sources.extend(
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string),
        );
    }
    if let Ok(value) = env::var("PWB_JOIN_SIGNER") {
        match parse_public_key(&value) {
            Some(key) => join_signer = Some(key),
            None => eprintln!("[Join] Ignoring invalid PWB_JOIN_SIGNER"),
        }
    }
    trust_anchors.sort_unstable_by_key(|key| key.0);
    trust_anchors.dedup();

    // `--no-join` only suppresses the implicit default; an explicit --join /
    // PWB_JOIN source is still honoured.
    if !no_join && join_sources.is_empty() && !explicit_bootstrap {
        join_sources.push(join::DEFAULT_JOIN_URL.to_string());
    }
    if !join_sources.is_empty() {
        println!("[Join] Fetching {} source(s)…", join_sources.len());
        match join::discover(
            &join_sources,
            Some(&data_dir),
            join_signer,
            require_join_signature,
        )
        .await
        {
            Ok(descriptor) => {
                let seeds = join::resolve_seeds(&descriptor, preferred_family).await;
                for peer in seeds {
                    if !effective_bootstrap
                        .iter()
                        .any(|have| have.public_key == peer.public_key)
                    {
                        effective_bootstrap.push(peer);
                    }
                }
                if trust_anchors.is_empty() && !descriptor.trust_anchors.is_empty() {
                    trust_anchors = descriptor.trust_anchors.clone();
                    trust_anchors.sort_unstable_by_key(|key| key.0);
                    trust_anchors.dedup();
                    println!(
                        "[Trust] Using {} anchor(s) from the join descriptor",
                        trust_anchors.len()
                    );
                }
                println!(
                    "[Join] Descriptor published {} seed(s), {} anchor(s)",
                    descriptor.seeds.len(),
                    descriptor.trust_anchors.len()
                );
                if descriptor.seeds.is_empty() {
                    if descriptor.signer.is_some() {
                        eprintln!(
                            "[Join] The published descriptor withdraws all seeds; falling back to LAN discovery."
                        );
                    } else {
                        eprintln!(
                            "[Join] No public seed is published yet; falling back to LAN discovery."
                        );
                    }
                }
            }
            Err(error) => {
                eprintln!("[Join] {error}");
                eprintln!("[Join] {}", error.advice());
            }
        }
    }
    println!("  Listening on             : {}", listen_addr);
    let config = NodeConfig {
        listen_addr,
        keypair: identity.keypair.clone(),
        bootstrap_peers: effective_bootstrap.clone(),
        stun_server: effective_stun_server,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    if cover_traffic {
        engine.enable_cover_traffic(CoverConfig::default());
    }
    engine.start().await.expect("node engine start");
    if let Some(cover) = engine.cover_traffic() {
        println!(
            "  Cover Traffic            : every {:?}-{:?} (mean {:?})",
            cover.min_interval, cover.max_interval, cover.mean_interval
        );
    }

    let own_addr = engine
        .local_addr()
        .expect("engine socket bound after start")
        .expect("local socket address");
    println!("  Network Listen Address   : {}", own_addr);

    if let Some(public_addr) = engine.public_addr() {
        println!("  Network Public Address   : {}  (via STUN)", public_addr);
    } else if effective_stun_server.is_some() {
        println!("  Network Public Address   : (STUN query failed; using local address)");
    }

    if let Some(discovery) = engine.discovery_addr() {
        println!(
            "  LAN Discovery           : {} (broadcast + multicast)",
            discovery
        );
    }

    // Join the swarm through one randomly drawn bootstrap/rendezvous peer and
    // keep it only if it answered a liveness probe; the others are dialled
    // only as failover, so a shared seed list is not hit by every client at
    // once.
    let connected = engine.connect_bootstrap_verified().await;
    if !connected.is_empty() {
        println!("[Network] Joined via {} bootstrap peer(s)", connected.len());
        for peer in &connected {
            println!(
                "    -> {} at {}",
                &peer.public_key.to_hex()[..8],
                peer.address
            );
        }
    } else if !effective_bootstrap.is_empty() {
        println!("[Network] No bootstrap peers reachable; running standalone.");
    } else {
        println!("[Network] No bootstrap peers configured; LAN discovery only.");
    }

    // Generate the connection link using the public (STUN'd) address when
    // available, falling back to the local bound address.
    let own_link = engine.generate_connection_link();
    println!("\n[Network] Your connection link:");
    println!("  {}", own_link);

    // If a connection link was passed on the command line, dial it.
    if let Some(target_link) = connect_target {
        println!(
            "\n[Network] Launching connection to peer via link: {}",
            target_link
        );
        match NodeEngine::parse_connection_link(&target_link) {
            Some((addr, pubkey)) => match engine
                .dial_resilient(addr, pubkey, Duration::from_secs(2))
                .await
            {
                Ok(peer) => println!(
                    "[Network] Connected to peer {} at {} (reputation 0)",
                    peer.public_key, peer.address
                ),
                Err(e) => eprintln!("[Network] Peer never answered: {:?}", e),
            },
            None => eprintln!("[Network] Could not parse connection link"),
        }
    }

    // Background task: periodically gossip the peer table as PeerExchange so
    // bootstrap/rendezvous nodes advertise known peers and the swarm stays
    // discoverable.
    let px_handle = tokio::spawn(peer_exchange_loop(
        engine.clone(),
        identity.public_key(),
        Duration::from_secs(30),
    ));

    // Persist artifacts and the search index under the same data directory
    // as `pwb-node`. A browser-hosted claim therefore keeps serving its
    // content after a restart instead of pointing at a CID that vanished
    // with the process.
    let host = Arc::new(
        ArtifactHost::open(data_dir.join("artifacts")).unwrap_or_else(|e| {
            eprintln!("[Host] Cannot open artifact store: {e}; using memory only");
            ArtifactHost::new()
        }),
    );
    let registry = Arc::new(Mutex::new(DomainRegistry::new()));
    let search = Arc::new(SearchEngine::open(data_dir.join("index.json")));

    // Domain-claim service: signs and gossips `.pwb` claims, syncs the registry
    // with peers on startup, resolves names remotely on demand, and persists
    // records so claims survive restarts. It also owns the signed credit
    // ledger, which DomainService stores beside the registry (`credits.json`).
    // Both follow `--data`/`PWB_DATA` like the node's copy, so a separate
    // browser profile does not inherit another profile's claims or balance;
    // `PWB_DOMAIN_FILE` overrides the registry location explicitly.
    let domain_persist = Some(
        std::env::var("PWB_DOMAIN_FILE")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("domains.json")),
    );
    let domain = Arc::new(DomainService::new_with_trust(
        engine.clone(),
        Arc::clone(&registry),
        identity.keypair.clone(),
        0,
        domain_persist,
        trust_anchors,
    ));
    let domain_handle = tokio::spawn(Arc::clone(&domain).run());

    // `chat.plain`: E2EE chat over the embedded node. The receive loop
    // records inbound messages for the page and prints them, and
    // transcripts persist under the profile's `chat.json` like the daemon's,
    // so conversations survive a restart. Saving merges with what is on disk
    // already, so pointing `PWB_DATA` at the node's directory shares the
    // conversation instead of one process overwriting the other's entries.
    let chat = pwb_node::chat::ChatService::with_data_dir(engine.clone(), &data_dir);
    let chat_handle = tokio::spawn(Arc::clone(&chat).run());

    // Public rooms (`chat.plain?room=`): the browser joins the default room
    // like the daemon does, so the square is live the moment the window
    // opens. Posts are signed with this identity and flooded to peers, which
    // relay them on; membership and transcripts persist under the profile
    // beside the chat file.
    let room = pwb_node::room::RoomService::with_data_dir(
        engine.clone(),
        identity.keypair.clone(),
        &data_dir,
    );
    let room_handle = tokio::spawn(Arc::clone(&room).run());
    if let Err(error) = room.join(pwb_node::room::DEFAULT_ROOM) {
        eprintln!("[Room] {error}");
    }
    match domain.trust_quorum() {
        Some((required, total)) => {
            println!("  Funding Trust            : {required}-of-{total} pinned attestor(s)")
        }
        None => println!(
            "  Funding Trust            : local peer view (pass --trust-anchor to pin attestors)"
        ),
    }
    println!(
        "  Credit Ledger Balance    : {} credits",
        domain.available_credits()
    );

    // Federated search service: indexes local pages into the sharded index,
    // answers peers' shard queries (crediting query work in the shared ledger),
    // and routes our own queries across the swarm.
    let federation = Arc::new(
        DistributedSearch::new(
            engine.clone(),
            Arc::clone(&search),
            Arc::clone(&host),
            identity.keypair.clone(),
        )
        .with_ledger(Arc::clone(domain.ledger()))
        .with_private_mode(privacy == pwb_node::privacy::PrivacyMode::Onion),
    );
    let federation_handle = tokio::spawn(Arc::clone(&federation).run());

    // Answer onion-delivered read-only requests from the embedded node. This
    // makes a browser a useful relay/exit too, not just a client: without it a
    // swarm of browsers could not serve private searches to one another.
    let onion_identity = Arc::clone(&identity);
    let onion_search = Arc::clone(&search);
    let onion_host = Arc::clone(&host);
    let onion_registry = Arc::clone(&registry);
    let onion_ledger = Arc::clone(domain.ledger());
    let onion_federation = Arc::clone(&federation);
    let onion_domain = Arc::clone(&domain);
    let onion_engine = engine.clone();
    let onion_handle = tokio::spawn(async move {
        let mut inbound = onion_engine.subscribe_onion_requests();
        while let Some(request) = inbound.recv().await {
            if !request.can_reply() {
                continue;
            }
            let response = match &request.message {
                NetworkMessage::SearchQuery { .. } | NetworkMessage::FetchArtifact { .. } => {
                    let registry = onion_registry.lock().unwrap();
                    let handler = SystemDomainHandler::new(
                        &onion_search,
                        &onion_host,
                        &registry,
                        &onion_identity,
                        onion_ledger.balance(),
                    );
                    handler.handle_message(request.message.clone())
                }
                NetworkMessage::ShardQuery { .. } => {
                    onion_federation.answer_private_shard_query(&request.message, request.sender)
                }
                NetworkMessage::FetchArtifactChunk { .. } => {
                    onion_federation.answer_private_fetch_chunk(&request.message)
                }
                NetworkMessage::DomainQuery { .. } => {
                    onion_domain.answer_private_domain_query(&request.message, request.sender)
                }
                _ => None,
            };
            if let Some(response) = response {
                let _ = onion_engine.send_onion_reply(&request, &response);
            }
        }
    });

    // Re-announce artifacts persisted from previous sessions so peers can
    // fetch content behind an existing `.pwb` claim after a restart. In onion
    // mode this would publish the user's browsing history under their identity,
    // so it is a direct-mode-only behaviour.
    if privacy == pwb_node::privacy::PrivacyMode::Direct {
        for (content_id, title) in host.artifacts() {
            let _ = engine.broadcast(NetworkMessage::gossip_announce(
                &identity.keypair,
                content_id,
                title,
            ));
        }
    }

    // Build the browser app for the initial target, then run the winit event
    // loop once. Navigation swaps the DOM bundle in place (see
    // `BrowserApp::navigate_to` and `about_to_wait` in pipeline/app.rs), so
    // the EventLoop is never recreated.
    //
    // A `search.plain?q=` target runs the federated query up front (`main` is
    // async) so even the first page can be a network search.
    let bytes = if let Some(query) = search_query_of(&target) {
        println!(
            "[Search] Federated query across the swarm ({}): '{query}'",
            if privacy == pwb_node::privacy::PrivacyMode::Onion {
                "onion"
            } else {
                "direct"
            }
        );
        match privacy {
            pwb_node::privacy::PrivacyMode::Onion => {
                match federation.query_private_report(&query).await {
                    Ok(report) => {
                        let partial = report.partial();
                        let answered = report.shards_answered;
                        let expected = report.shards_expected;
                        let results = report.results;
                        println!(
                            "[Search] Merged {} result(s) through onion circuits ({answered}/{expected} shards answered).",
                            results.len()
                        );
                        render_search_results_bundle_with_status(&query, &results, partial)
                            .serialize()
                    }
                    Err(error) => render_message_bundle(
                        "private search unavailable",
                        &format!(
                            "Could not search anonymously: {error}. The browser will not fall back to direct traffic; join more peers or pass --direct to accept revealing your address."
                        ),
                    )
                    .serialize(),
                }
            }
            pwb_node::privacy::PrivacyMode::Direct => {
                let report = federation.query_report(&query).await;
                let partial = report.partial();
                let answered = report.shards_answered;
                let expected = report.shards_expected;
                let results = report.results;
                for result in results.iter().take(PREFETCH_RESULTS) {
                    let _ = federation.fetch(&result.content_id).await;
                }
                println!(
                    "[Search] Merged {} result(s) from the sharded index ({answered}/{expected} shards answered).",
                    results.len()
                );
                render_search_results_bundle_with_status(&query, &results, partial).serialize()
            }
        }
    } else if is_register_target(&target) {
        // The live claim console, backed by this node's domain service.
        domain.render_view(&host, None, None)
    } else {
        let local = {
            let registry = registry.lock().unwrap();
            let peers = domain.peers();
            let network = NetworkSnapshot {
                listen_addr: domain.listen_addr(),
                discovery_addr: domain.discovery_addr(),
                public_addr: domain.public_addr(),
                peers: peers.clone(),
            };
            resolve_url_or_file_with_context(
                &target,
                &search,
                &host,
                &registry,
                &identity,
                domain.available_credits(),
                &peers,
                network,
                chat.snapshot(),
                room.snapshot(),
            )
        };
        match local {
            Ok(b) => {
                // Publishing a visited page broadcasts the bundle and this
                // browser's identity; onion mode leaves that to a deliberate
                // host action instead of every navigation.
                if privacy == pwb_node::privacy::PrivacyMode::Direct && !is_system_target(&target) {
                    federation.index_and_publish(&b);
                }
                b
            }
            Err(e) => {
                // A content id that isn't cached locally is fetched from peers;
                // a `.pwb` name may be resolvable from the swarm.
                let remote = if let Some(cid) = content_id_of(&target) {
                    println!("[Content] {cid} not local; fetching from the swarm…");
                    match privacy {
                        pwb_node::privacy::PrivacyMode::Onion => {
                            federation.fetch_private(&cid).await.ok()
                        }
                        pwb_node::privacy::PrivacyMode::Direct => federation.fetch(&cid).await,
                    }
                } else {
                    match pwb_name_of(&target) {
                        Some(name) => {
                            println!("[Domain] '{name}' not local; asking the swarm…");
                            let record = match privacy {
                                pwb_node::privacy::PrivacyMode::Onion => {
                                    domain.resolve_private(&name).await.ok().flatten()
                                }
                                pwb_node::privacy::PrivacyMode::Direct => {
                                    domain.resolve(&name).await
                                }
                            };
                            match record {
                                Some(record) => match privacy {
                                    pwb_node::privacy::PrivacyMode::Onion => federation
                                        .fetch_private(&record.target_content_id)
                                        .await
                                        .ok(),
                                    pwb_node::privacy::PrivacyMode::Direct => {
                                        federation.fetch(&record.target_content_id).await
                                    }
                                },
                                None => None,
                            }
                        }
                        None => None,
                    }
                };
                match remote {
                    Some(b) => {
                        if privacy == pwb_node::privacy::PrivacyMode::Direct {
                            federation.index_and_publish(&b);
                        }
                        b
                    }
                    None => {
                        eprintln!("Failed to resolve target '{}': {:?}", target, e);
                        render_message_bundle(
                            "page unavailable",
                            &format!(
                                "PlainWeb could not resolve '{target}'{}.",
                                if privacy == pwb_node::privacy::PrivacyMode::Onion {
                                    " through an onion circuit (no direct fallback)"
                                } else {
                                    ""
                                }
                            ),
                        )
                        .serialize()
                    }
                }
            }
        }
    };

    let mut app = build_browser_app(
        &bytes,
        Arc::clone(&search),
        Arc::clone(&host),
        Arc::clone(&registry),
        Arc::clone(&identity),
        Arc::clone(&federation),
        Arc::clone(&domain),
        Arc::clone(&chat),
        Arc::clone(&room),
        privacy,
        tokio::runtime::Handle::current(),
    )
    .expect("build browser app");
    app.set_initial_url(&target);

    if let Err(e) = run_event_loop(app) {
        eprintln!("Pipeline execution error: {:?}", e);
    }

    // Persist the identity (peer ID + seed credit balance) so it survives a
    // restart; claimed-domain balances live in the domain registry file.
    let _ = identity.save(&identity_path);

    federation_handle.abort();
    onion_handle.abort();
    domain_handle.abort();
    chat_handle.abort();
    room_handle.abort();
    px_handle.abort();
    engine.stop().await;
    println!("[Network] Node stopped.");
}

fn print_help() {
    println!("PlainWeb Browser Usage:");
    println!("  pwb-browser [options] [target]");
    println!(
        "  (.plain = official; {} = unofficial user sites)",
        pwb_core::system::unofficial_tld_list()
    );
    println!();
    println!("Options:");
    println!("  -f, --file <path>    Open a local .pwb file");
    println!("  -c, --connect <link> Connect to a peer via a nodes.plain connection link");
    println!("      --stun-server <host:port>  STUN server for NAT traversal (default: stun.l.google.com:19302)");
    println!("      --no-stun           Do not contact a STUN server (LAN-only reachability)");
    println!(
        "      --bootstrap <host:port>/<hex_pubkey>  Bootstrap/rendezvous peer for an internet swarm"
    );
    println!("                          (or set PWB_BOOTSTRAP to a comma-separated list)");
    println!(
        "      --trust-anchor <hex_pubkey>  Require funding certificates from pinned attestors"
    );
    println!("                          (repeatable; or set PWB_TRUST_ANCHORS to a comma-separated list)");
    println!(
        "      --join <url|path>  Fetch a well-known join descriptor (repeatable; or set PWB_JOIN)"
    );
    println!("                          (default: the project's deploy/join.json on GitHub)");
    println!("      --no-join          Do not fetch the default join descriptor");
    println!(
        "      --join-signer <hex_pubkey>  Pin the descriptor signer (or set PWB_JOIN_SIGNER)"
    );
    println!("      --require-join-signature    Reject unsigned join descriptors");
    println!(
        "      --direct           Use the direct transport instead of onion circuits (reveals your address)"
    );
    println!(
        "      --cover-traffic     Emit decoy onion cells at randomized intervals (costs bandwidth)"
    );
    println!(
        "      --identity <path>  Persisted identity file (default: $PWB_IDENTITY or $HOME/.pwb/identity.json)"
    );
    println!("                          (home dir: $HOME, else %USERPROFILE% on Windows)");
    println!(
        "      --listen <addr>    UDP listen address (default: 0.0.0.0:0; [::]:0 for dual-stack IPv4+IPv6, 127.0.0.1:0 for localhost-only)"
    );
    println!("  -h, --help           Show this help message");
    println!();
    println!("Examples:");
    println!(
        "  pwb-browser                        (LAN discovery; add --bootstrap for an internet swarm)"
    );
    println!("  pwb-browser welcome.plain");
    println!(
        "  pwb-browser \"search.plain?q=plainweb\"   (quote the URL; `?` is a shell glob char)"
    );
    println!("  pwb-browser \"plain://nodes.plain?connect=<ip:port>/<pubkey>\"");
    println!("  pwb-browser -c \"plain://nodes.plain?connect=<ip:port>/<pubkey>\"");
    println!("  pwb-browser devtools");
    println!("  pwb-browser network.plain           (this node's addresses, peers, and services)");
    println!("  pwb-browser chat.plain              (E2EE peer chat; onion-routed in onion mode)");
    println!(
        "  pwb-browser help.plain              (system-domain index, namespace and credit rules)"
    );
    println!("  pwb-browser --file mysite.pwb");
}
