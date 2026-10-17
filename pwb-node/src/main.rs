//! `pwb-node` CLI node launcher — runs a live PlainWeb network node.
//!
//! The node binds a UDP socket, starts the [`pwb_core::net::NodeEngine`]
//! (sender loop, receiver loop, peer maintenance, and LAN discovery), and can
//! connect to peers via `plain://nodes.plain?connect=<ip:port>/<pubkey>`
//! links or scanned QR pairing payloads.

use pwb_core::crypto::{ContentId, PublicKey};
use pwb_core::net::NodeEngine;
use pwb_core::wire::Bundle;
use pwb_node::domain::{DomainRegistry, PricingEpochStats};
use pwb_node::host::ArtifactHost;
use pwb_node::identity::IdentityManager;
use pwb_node::search::SearchEngine;
use pwb_node::verifier::{IndexProof, PeerVerifier};
use std::fs;
use std::net::SocketAddr;
use std::time::Duration;

#[tokio::main]
async fn main() {
    println!("PlainWeb Layer 3 Serverless Node (pwb-node)");
    println!("Starting node process...");

    // Parse CLI arguments (Connection Links & URI Handlers).
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

    // --- Network stack: start the NodeEngine with live UDP transport -------
    //
    // This binds a UDP socket, spawns the sender/receiver loops, the peer
    // maintenance loop, and a DiscoveryService that shares the engine's peer
    // table. LAN beacons (broadcast + multicast on port 7922) will populate
    // the peer table automatically, and the engine can send/receive
    // NetworkMessage datagrams to/from any registered peer.
    let listen_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let config = pwb_core::net::NodeConfig {
        listen_addr,
        keypair: identity.keypair.clone(),
        bootstrap_peers: Vec::new(),
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    engine.start().await.expect("node engine start");

    let own_addr = engine
        .local_addr()
        .expect("engine socket bound after start")
        .expect("local socket address");
    println!("  Network Listen Address   : {}", own_addr);

    // Generate the connection link using the *actual* bound address so peers
    // can dial us.
    let own_link = NodeEngine::build_connection_link(own_addr, identity.public_key());
    println!("\n[Network] Your connection link:");
    println!("  {}", own_link);

    // Render the link as a scannable QR code (§3.3).
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

    // --- Existing demo workload -------------------------------------------

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

                    let mut minter =
                        pwb_node::credit::CreditMinter::with_default_policy(identity.public_key());
                    let now = std::time::Instant::now();
                    minter.record_traffic(bundle_bytes.len() as u64 + 16 * 1024);

                    let mut index_receipt = None;
                    if let Some(receipt) =
                        pwb_node::credit::IndexReceipt::sign(&identity.keypair, &bundle_bytes)
                    {
                        match minter.mint_index(&receipt, &bundle_bytes, now) {
                            Ok(amount) => {
                                identity.add_credits(amount);
                                println!(
                                    "[Credit] Indexing work verified: +{amount} credits ({} earned this session)",
                                    minter.total_earned()
                                );
                            }
                            Err(e) => println!("[Credit] Index mint refused: {e}"),
                        }
                        index_receipt = Some(receipt);
                    }

                    let challenge = [0xA7; 32];
                    minter.issue_challenge(challenge, now);
                    let query_receipt = pwb_node::credit::QueryReceipt::sign(
                        &identity.keypair,
                        "PlainWeb",
                        vec![content_id],
                        challenge,
                    );
                    match minter.mint_query(&query_receipt, now) {
                        Ok(amount) => {
                            identity.add_credits(amount);
                            println!("[Credit] Query served to requester: +{amount} credits");
                        }
                        Err(e) => println!("[Credit] Query mint refused: {e}"),
                    }

                    for i in 0..35u8 {
                        minter.relay_cell(&[i; 1024]);
                    }
                    let relay_receipt = minter
                        .seal_relay_window(&identity.keypair)
                        .expect("cells were relayed");
                    match minter.mint_relay(&relay_receipt, now) {
                        Ok(amount) => {
                            identity.add_credits(amount);
                            println!("[Credit] Relay window sealed: +{amount} credits");
                        }
                        Err(e) => println!("[Credit] Relay mint refused: {e}"),
                    }

                    let user_domain = "welcome.pwb";
                    let epoch = 1u64;
                    let policy = minter.policy().clone();
                    let attestor_keys: Vec<pwb_core::crypto::Keypair> = (0..3)
                        .map(|_| pwb_core::crypto::Keypair::generate())
                        .collect();
                    let mut attestations = Vec::new();
                    let mut attestor_ledgers = Vec::new();
                    for key in &attestor_keys {
                        let mut attester = pwb_node::ledger::AttestorLedger::new(key.public);
                        let mut recorded = true;
                        if let Some(receipt) = &index_receipt {
                            recorded &= attester
                                .record_index_earned(receipt, &bundle_bytes, epoch, &policy)
                                .is_ok();
                        }
                        if query_receipt.verify() {
                            recorded &= attester
                                .record_earned(
                                    identity.public_key(),
                                    epoch,
                                    policy.query_credits,
                                    query_receipt.evidence_digest(),
                                )
                                .is_ok();
                        } else {
                            recorded = false;
                        }
                        if relay_receipt.verify() {
                            let relay_credits =
                                relay_receipt.bytes * policy.relay_credits_per_kib / 1024;
                            recorded &= attester
                                .record_earned(
                                    identity.public_key(),
                                    epoch,
                                    relay_credits,
                                    relay_receipt.evidence_digest(),
                                )
                                .is_ok();
                        } else {
                            recorded = false;
                        }
                        if recorded {
                            if let Ok(attestation) =
                                attester.attest(identity.public_key(), epoch, key)
                            {
                                attestations.push(attestation);
                            }
                        }
                        attestor_ledgers.push(attester);
                    }
                    match pwb_node::ledger::QuorumCertificate::from_attestations(attestations) {
                        Ok(certificate) => {
                            let neighborhood: Vec<_> =
                                attestor_keys.iter().map(|k| k.public).collect();
                            let claim_price = registry.claim_cost();
                            match registry.register_attested(
                                user_domain,
                                content_id,
                                &certificate,
                                2,
                                &neighborhood,
                            ) {
                                Ok(()) => println!(
                                    "[Domain] Claimed '{user_domain}' -> {content_id} via 2-of-3 attestor quorum (attested balance {}, price {claim_price})",
                                    certificate.balance
                                ),
                                Err(e) => println!("[Domain] Attested claim refused: {e}"),
                            }

                            if !attestor_ledgers.is_empty() {
                                let denomination = pwb_node::blind::DEFAULT_DENOMINATIONS
                                    .iter()
                                    .copied()
                                    .find(|d| *d >= claim_price)
                                    .unwrap_or_else(|| {
                                        *pwb_node::blind::DEFAULT_DENOMINATIONS.last().unwrap()
                                    });
                                let mut blind_issuer =
                                    pwb_node::blind::BlindBalanceIssuer::with_defaults(&[0xB1; 32]);
                                match pwb_node::blind::withdraw_attested(
                                    &mut blind_issuer,
                                    &mut attestor_ledgers[0],
                                    identity.public_key(),
                                    epoch,
                                    denomination,
                                    1,
                                ) {
                                    Ok(receipt) => {
                                        for ledger in &mut attestor_ledgers[1..] {
                                            let _ = ledger.record_spent(
                                                identity.public_key(),
                                                epoch,
                                                receipt.total,
                                                receipt.evidence,
                                            );
                                        }
                                        let voucher_key =
                                            blind_issuer.public_key(denomination).unwrap();
                                        let binding = pwb_node::blind::IssuerBinding::sign(
                                            &attestor_keys[0],
                                            epoch,
                                            denomination,
                                            voucher_key,
                                        );
                                        let claim = pwb_node::blind::BlindClaim::build(
                                            &receipt.vouchers[0],
                                            &binding,
                                        )
                                        .expect("voucher ownership");
                                        let pseudonym =
                                            pwb_core::crypto::Keypair::generate().public;
                                        match registry.register_blind(
                                            "private.pwb",
                                            content_id,
                                            pseudonym,
                                            &claim,
                                            &neighborhood,
                                        ) {
                                            Ok(()) => println!(
                                                "[Blind] Withdrew {total} credits as an unlinkable voucher (evidence {evidence:02x?}...) and claimed 'private.pwb' under pseudonym {pseudonym} — unlinkable to the earner",
                                                total = receipt.total,
                                                evidence = &receipt.evidence[..4],
                                            ),
                                            Err(e) => println!("[Blind] Private claim refused: {e}"),
                                        }
                                    }
                                    Err(e) => println!("[Blind] Withdrawal refused: {e}"),
                                }
                            }
                        }
                        Err(e) => println!("[Domain] No attestor quorum: {e}"),
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
    if let Some(record) = registry.resolve("private.pwb") {
        println!(
            "[Domain Resolver] 'private.pwb' resolved to Content ID: {} (owner {})",
            record.target_content_id, record.owner
        );
    }

    let price_before = registry.claim_cost();
    registry.record_pricing_epoch(PricingEpochStats {
        epoch: 2,
        registrations: 32,
        network_work: 2_000,
        node_count: 4,
    });
    println!(
        "[Pricing] Claim price moved {price_before} -> {} credits after a hot epoch (32 claims, 2000 credits minted, 4 nodes)",
        registry.claim_cost()
    );

    let mut cluster_a = DomainRegistry::new();
    let mut cluster_b = DomainRegistry::new();
    let alice = PublicKey([11; 32]);
    let bob = PublicKey([22; 32]);
    let cid = ContentId([33; 32]);

    let _ = cluster_a.register("alpha.pwb", cid, alice);
    let _ = cluster_b.register("beta.pwb", cid, bob);
    let _ = cluster_a.register("contested.pwb", cid, alice);
    let _ = cluster_b.register("contested.pwb", cid, bob);

    cluster_a.record_pricing_epoch(PricingEpochStats {
        epoch: 1,
        registrations: 40,
        network_work: 1_000,
        node_count: 1,
    });
    cluster_b.record_pricing_epoch(PricingEpochStats {
        epoch: 1,
        registrations: 0,
        network_work: 100_000,
        node_count: 100,
    });

    let price_before = cluster_a.claim_cost();
    cluster_a.merge(&cluster_b);
    let contested_owner = cluster_a
        .resolve("contested.pwb")
        .map(|record| record.owner)
        .expect("contested.pwb exists after merge");
    println!(
        "[Gossip] merged clusters: {} domains; 'contested.pwb' won by {}",
        cluster_a.count(),
        contested_owner
    );
    println!(
        "[Gossip] merged base cost: {} credits (max of cluster prices {} and {})",
        cluster_a.claim_cost(),
        price_before,
        cluster_b.claim_cost()
    );

    // --- Network: connect to a peer via connection link -------------------
    //
    // Launching a connection registers the peer in the engine's peer table
    // and sends a `PeerDiscovery` announcement over UDP so the remote node
    // learns about us too. The discovery service (already running) will also
    // begin exchanging beacons with any peers on the LAN.
    if let Some(target) = connect_target {
        println!(
            "\n[Network] Launching connection to peer via link: {}",
            target
        );
        let result = engine.launch_connection(&target).or_else(|_| {
            // Scanned QR payloads may use the compact `<ip:port>/<hex>` form.
            match pwb_core::qr::parse_pairing_payload(&target) {
                Some((addr, key)) => engine.connect_to(addr, key),
                None => Err(pwb_core::net::NetworkError::InvalidConnectionLink(
                    target.clone(),
                )),
            }
        });
        match &result {
            Ok(peer) => {
                println!(
                    "[Network] Connected to peer {} at {} (reputation 0)",
                    peer.public_key, peer.address
                );
                // Announce ourselves to the new peer so the connection is
                // bidirectional: they learn our address and public key.
                let _ = engine.send_to(
                    &peer.public_key,
                    pwb_core::net::NetworkMessage::PeerDiscovery {
                        known_peers: engine
                            .peer_table()
                            .lock()
                            .unwrap()
                            .keys()
                            .copied()
                            .collect(),
                        sender_pubkey: identity.public_key(),
                    },
                );
            }
            Err(e) => eprintln!("[Network] Could not launch connection: {:?}", e),
        }
    }

    println!("\nNode remaining balance: {} credits.", identity.credits);
    println!("Node daemon active. Press Ctrl+C to stop.");

    // Keep the node alive so it can send/receive network traffic.
    // Discovery beacons continue announcing on the LAN; inbound messages are
    // dispatched to the handler loop by the receiver loop.
    tokio::signal::ctrl_c().await.expect("wait for Ctrl+C");
    engine.stop().await;
    println!("[Network] Node stopped.");
}
