//! `pwb-node` CLI node launcher.

use pwb_core::wire::Bundle;
use pwb_node::domain::{DomainRegistry, DOMAIN_CLAIM_COST};
use pwb_node::host::ArtifactHost;
use pwb_node::identity::IdentityManager;
use pwb_node::search::SearchEngine;
use pwb_node::verifier::{IndexProof, PeerVerifier};
use std::fs;

fn main() {
    println!("PlainWeb Layer 3 Serverless Node (pwb-node)");
    println!("Starting node process...");

    // Parse CLI arguments (TODO §3.2 Connection Links & URI Handlers)
    let args: Vec<String> = std::env::args().collect();
    let mut arg_index = 1;
    let mut connect_target: Option<String> = None;
    while arg_index < args.len() {
        match args[arg_index].as_str() {
            "-c" | "--connect" => {
                if arg_index + 1 < args.len() {
                    connect_target = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --connect requires a link argument");
                    std::process::exit(1);
                }
            }
            _ => {
                arg_index += 1;
            }
        }
    }

    let mut identity = IdentityManager::new();
    println!("  Node Identity Public Key : {}", identity.public_key());
    println!("  Starting Credit Balance  : {} credits", identity.credits);

    let mut host = ArtifactHost::new();
    let mut search = SearchEngine::new();
    let mut registry = DomainRegistry::new();

    let demo_path = "demo.pwb";
    if let Ok(bundle_bytes) = fs::read(demo_path) {
        match host.store(bundle_bytes.clone()) {
            Ok(content_id) => {
                println!("\n[Host] Ingested artifact into local store!");
                println!("       Content ID: {}", content_id);

                if let Ok(bundle) = Bundle::deserialize(&bundle_bytes) {
                    search.index_bundle(content_id, &bundle);
                    println!("[Search] Indexed bundle terms into decentralized search engine.");

                    let proof = IndexProof {
                        miner: identity.public_key(),
                        content_id,
                        term_hash: content_id,
                        signature: identity.sign_message(b"index_proof"),
                    };
                    let is_valid = PeerVerifier::verify_indexing_proof(&proof);
                    println!(
                        "[Verifier] Peer verification check: {}",
                        if is_valid { "PASSED" } else { "FAILED" }
                    );

                    // Claim user domain under .pwb TLD (security enforced: .plain is reserved)
                    let user_domain = "welcome.pwb";
                    if identity.spend_credits(DOMAIN_CLAIM_COST).is_ok()
                        && registry
                            .register(user_domain, content_id, identity.public_key())
                            .is_ok()
                    {
                        println!(
                            "[Domain] Claimed user domain '{}' -> {} (Spent {} credits)",
                            user_domain, content_id, DOMAIN_CLAIM_COST
                        );
                    }
                }
            }
            Err(e) => eprintln!("Failed to ingest artifact: {}", e),
        }
    } else {
        println!("Note: demo.pwb not found. Run `cargo run -p pwb-web` to generate it.");
    }

    let query = "PlainWeb";
    println!("\n[Search Query] Searching index for '{}':", query);
    let results = search.search(query);
    if results.is_empty() {
        println!("  No search results found.");
    } else {
        for (idx, res) in results.iter().enumerate() {
            println!(
                "  #{}: \"{}\" | CID: {} | Rank Score: {:.2}",
                idx + 1,
                res.title,
                res.content_id,
                res.rank_score
            );
        }
    }

    if let Some(record) = registry.resolve("welcome.pwb") {
        println!(
            "\n[Domain Resolver] 'welcome.pwb' resolved to Content ID: {}",
            record.target_content_id
        );
    }

    // Display the connection link for this node
    let own_link = pwb_core::net::NodeEngine::build_connection_link(
        std::net::SocketAddr::new("127.0.0.1".parse().unwrap(), 8080),
        identity.public_key(),
    );
    println!("\n[Network] Your connection link:");
    println!("  {}", own_link);

    // Render the link as a scannable QR code (TODO §3.3).
    match pwb_core::qr::QrCode::encode(&own_link) {
        Ok(qr) => {
            println!(
                "\n[Network] Scan-to-pair QR (version {}, mask {}):",
                qr.version(),
                qr.mask()
            );
            println!("{}", qr.to_ascii());
        }
        Err(e) => eprintln!("[Network] QR encode failed: {}", e),
    }

    // Launch a connection to a peer described by a `nodes.plain` link or a
    // scanned QR pairing payload (TODO §3.2/§3.3).
    if let Some(target) = connect_target {
        println!(
            "\n[Network] Launching connection to peer via link: {}",
            target
        );
        let config = pwb_core::net::NodeConfig {
            listen_addr: std::net::SocketAddr::new("127.0.0.1".parse().unwrap(), 0),
            keypair: identity.keypair.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: std::time::Duration::from_secs(30),
        };
        let engine = pwb_core::net::NodeEngine::new(config);
        let result = engine.launch_connection(&target).or_else(|_| {
            // Scanned QR payloads may use the compact `<ip:port>/<hex>` form.
            match pwb_core::qr::parse_pairing_payload(&target) {
                Some((addr, key)) => engine.connect_to(addr, key),
                None => Err(pwb_core::net::NetworkError::InvalidConnectionLink(
                    target.clone(),
                )),
            }
        });
        match result {
            Ok(peer) => println!(
                "[Network] Connected to peer {} at {} (reputation 0)",
                peer.public_key, peer.address
            ),
            Err(e) => eprintln!("[Network] Could not launch connection: {:?}", e),
        }
    }

    println!("\nNode remaining balance: {} credits.", identity.credits);
    println!("Node daemon active.");
}
