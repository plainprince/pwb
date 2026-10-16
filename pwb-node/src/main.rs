//! `pwb-node` CLI node launcher.

use pwb_core::crypto::{ContentId, PublicKey};
use pwb_core::wire::Bundle;
use pwb_node::domain::{DomainRegistry, PricingEpochStats};
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
                    // Mint credits for verified network services (TODO §4.1):
                    // indexing the bundle, serving a requester query, and
                    // relaying onion cells — each receipt is signed and
                    // re-checkable, and every credit is backed by recorded
                    // exchange traffic (anti-ASIC floor).
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

                    // Serve the demo query with a requester challenge.
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

                    // Relay a window of onion cells.
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

                    // Claim user domain under .pwb TLD via the k-of-N
                    // attested path (TODO §4.2): a simulated local
                    // neighborhood of attestors independently re-verifies
                    // each signed receipt, records the earnings on their
                    // hash-chained ledgers, and attests this node's balance.
                    // The registry accepts the claim only against the quorum
                    // — never a self-declared balance.
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

                            // Private claim path (TODO §4.3): withdraw the
                            // remaining attested credits as a blind voucher
                            // from attestor 0's ledger — the issuer signs
                            // blinded requests and never sees the tokens —
                            // gossip the volume event to the sibling views,
                            // then redeem the voucher under a fresh pseudonym
                            // owner for a second domain. The registry sees
                            // only the bearer token: the claim is unlinkable
                            // to the earning identity and to the withdrawal.
                            if !attestor_ledgers.is_empty() {
                                // Pick the smallest default denomination that
                                // covers the current claim price.
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
                                        // Cross-attestor inflation guard: the
                                        // same spend event (same evidence
                                        // digest) goes to every sibling view.
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
                                        // A fresh pseudonym key — not the
                                        // earning identity — owns the claim.
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

    // Live pricing demo (TODO §5): fold a hot epoch of network stats into
    // the rolling window and watch the claim price leave its initial value.
    let price_before = registry.claim_cost();
    registry.record_pricing_epoch(pwb_node::domain::PricingEpochStats {
        epoch: 2,
        registrations: 32,
        network_work: 2_000,
        node_count: 4,
    });
    println!(
        "[Pricing] Claim price moved {price_before} -> {} credits after a hot epoch (32 claims, 2000 credits minted, 4 nodes)",
        registry.claim_cost()
    );

    // Gossip partition merge demo (TODO §5.2): two isolated clusters evolve
    // independently, then reconcile their registries and pricing
    // deterministically (CRDT merge: last-writer-wins domains, max claimed
    // spend, unioned spend log, max base cost).
    let mut cluster_a = DomainRegistry::new();
    let mut cluster_b = DomainRegistry::new();
    let alice = PublicKey([11; 32]);
    let bob = PublicKey([22; 32]);
    let cid = ContentId([33; 32]);

    let _ = cluster_a.register("alpha.pwb", cid, alice);
    let _ = cluster_b.register("beta.pwb", cid, bob);
    // Both partitions claim the same domain at the same epoch; the merge
    // tie-breaks deterministically by owner public key bytes.
    let _ = cluster_a.register("contested.pwb", cid, alice);
    let _ = cluster_b.register("contested.pwb", cid, bob);

    // Each cluster observes a disjoint pricing epoch.
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
