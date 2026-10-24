//! End-to-end domain claiming over real loopback UDP: a signed `.pwb` claim
//! gossips to peers, late joiners converge via registry sync, unknown names
//! resolve by on-demand swarm query, and records survive a restart.

use pwb_core::crypto::{ContentId, Keypair};
use pwb_core::discovery::unix_time;
use pwb_core::net::{domain_claim_sign_message, NetworkMessage, NodeConfig, NodeEngine};
use pwb_node::domain::{
    ClaimError, DomainRecord, DomainRegistry, DomainService, INITIAL_CLAIM_PRICE,
};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn engine_config(keypair: Keypair) -> NodeConfig {
    NodeConfig {
        listen_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        keypair,
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    }
}

async fn start_engine(kp: Keypair) -> (NodeEngine, SocketAddr) {
    let engine = NodeEngine::new(engine_config(kp));
    engine.start().await.expect("engine start");
    let addr = engine.local_addr().expect("bound").expect("addr");
    (engine, addr)
}

fn registry() -> Arc<Mutex<DomainRegistry>> {
    Arc::new(Mutex::new(DomainRegistry::new()))
}

fn service(
    engine: NodeEngine,
    registry: Arc<Mutex<DomainRegistry>>,
    kp: &Keypair,
    credits: u64,
) -> Arc<DomainService> {
    Arc::new(DomainService::new(
        engine,
        registry,
        kp.clone(),
        credits,
        None,
    ))
}

/// Bilateral join: dial both directions and exchange peer-discovery so each
/// node's peer table knows the other.
fn connect(
    engine_a: &NodeEngine,
    addr_a: SocketAddr,
    kp_a: &Keypair,
    engine_b: &NodeEngine,
    addr_b: SocketAddr,
    kp_b: &Keypair,
) {
    let link_a = NodeEngine::build_connection_link(addr_a, kp_a.public);
    let link_b = NodeEngine::build_connection_link(addr_b, kp_b.public);
    engine_b
        .launch_connection(&link_a)
        .expect("B connects to A");
    engine_a
        .launch_connection(&link_b)
        .expect("A connects to B");
    engine_b
        .send_to(
            &kp_a.public,
            NetworkMessage::PeerDiscovery {
                known_peers: Vec::new(),
                sender_pubkey: kp_b.public,
            },
        )
        .expect("B announces");
    engine_a
        .send_to(
            &kp_b.public,
            NetworkMessage::PeerDiscovery {
                known_peers: Vec::new(),
                sender_pubkey: kp_a.public,
            },
        )
        .expect("A announces");
}

async fn wait_for<F: Fn() -> bool>(cond: F, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

#[tokio::test]
async fn test_claim_gossips_to_a_connected_peer() {
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;

    let service_a = service(engine_a.clone(), registry(), &kp_a, 100);
    let service_b = service(engine_b.clone(), registry(), &kp_b, 100);
    tokio::spawn(Arc::clone(&service_a).run());
    tokio::spawn(Arc::clone(&service_b).run());
    connect(&engine_a, addr_a, &kp_a, &engine_b, addr_b, &kp_b);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let cid = ContentId([7; 32]);
    let record = service_a.claim("alice.pwb", cid).expect("claim succeeds");
    assert_eq!(record.domain, "alice.pwb");
    assert_eq!(
        service_a.available_credits(),
        100 - INITIAL_CLAIM_PRICE,
        "the claim cost was debited from the ledger"
    );

    // B learns the claim purely by gossip.
    let learned = wait_for(
        || service_b.resolve_local("alice.pwb").is_some(),
        Duration::from_secs(3),
    )
    .await;
    assert!(learned, "B should learn alice.pwb by gossip");
    let learned = service_b.resolve_local("alice.pwb").unwrap();
    assert_eq!(learned.target_content_id, cid);
    assert_eq!(learned.owner, kp_a.public);

    // A's claim also gossips a pricing observation; B's rolling price moves
    // off the initial value instead of staying static forever.
    let priced = wait_for(
        || service_b.claim_cost() > INITIAL_CLAIM_PRICE,
        Duration::from_secs(3),
    )
    .await;
    assert!(priced, "B should apply A's pricing epoch gossip");

    // The owner can re-point the name for free, and peers accept the update
    // even though its signed `credits_spent` is zero.
    let updated_cid = ContentId([8; 32]);
    service_a
        .claim("alice.pwb", updated_cid)
        .expect("owner re-points the name");
    assert_eq!(
        service_a.available_credits(),
        100 - INITIAL_CLAIM_PRICE,
        "re-pointing must not debit the ledger"
    );
    let re_pointed = wait_for(
        || {
            service_b
                .resolve_local("alice.pwb")
                .is_some_and(|record| record.target_content_id == updated_cid)
        },
        Duration::from_secs(3),
    )
    .await;
    assert!(re_pointed, "B should learn the free re-point by gossip");

    engine_a.stop().await;
    engine_b.stop().await;
}

#[tokio::test]
async fn test_late_joiner_syncs_the_registry_from_peers() {
    let kp_a = Keypair::generate();
    let kp_c = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let (engine_c, addr_c) = start_engine(kp_c.clone()).await;

    let service_a = service(engine_a.clone(), registry(), &kp_a, 100);
    tokio::spawn(Arc::clone(&service_a).run());

    // A claims a domain while completely alone.
    let cid = ContentId([9; 32]);
    service_a.claim("solo.pwb", cid).expect("claim succeeds");

    // C joins afterwards and syncs the registry on startup.
    let service_c = service(engine_c.clone(), registry(), &kp_c, 100);
    connect(&engine_a, addr_a, &kp_a, &engine_c, addr_c, &kp_c);
    tokio::time::sleep(Duration::from_millis(300)).await;
    tokio::spawn(Arc::clone(&service_c).run());

    let synced = wait_for(
        || service_c.resolve_local("solo.pwb").is_some(),
        Duration::from_secs(3),
    )
    .await;
    assert!(synced, "C should receive A's registry via sync");
    assert_eq!(
        service_c
            .resolve_local("solo.pwb")
            .unwrap()
            .target_content_id,
        cid
    );

    engine_a.stop().await;
    engine_c.stop().await;
}

#[tokio::test]
async fn test_unknown_name_resolves_from_the_swarm_on_demand() {
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;

    let reg_a = registry();
    let service_a = service(engine_a.clone(), Arc::clone(&reg_a), &kp_a, 100);
    let service_b = service(engine_b.clone(), registry(), &kp_b, 100);
    tokio::spawn(Arc::clone(&service_a).run());
    tokio::spawn(Arc::clone(&service_b).run());
    connect(&engine_a, addr_a, &kp_a, &engine_b, addr_b, &kp_b);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Seed A's registry directly, *after* B's startup sync, so the record is
    // never gossiped and only the on-demand query path can deliver it. The
    // record is owner-signed, as every servable record must be.
    let cid = ContentId([3; 32]);
    let epoch = unix_time();
    let signature = kp_a.sign(&domain_claim_sign_message(
        "remote.pwb",
        cid,
        kp_a.public,
        0,
        epoch,
    ));
    reg_a.lock().unwrap().apply_record(DomainRecord {
        domain: "remote.pwb".into(),
        target_content_id: cid,
        owner: kp_a.public,
        credits_spent: 0,
        registered_at_epoch: epoch,
        signature: Some(signature),
        funding: None,
    });
    assert!(service_b.resolve_local("remote.pwb").is_none());
    let resolved = service_b
        .resolve("remote.pwb")
        .await
        .expect("B resolves the name by asking the swarm");
    assert_eq!(resolved.target_content_id, ContentId([3; 32]));
    // The positive answer is cached locally.
    assert!(service_b.resolve_local("remote.pwb").is_some());

    // An unknown name times out to None rather than hanging.
    assert!(service_b.resolve("nowhere.pwb").await.is_none());

    engine_a.stop().await;
    engine_b.stop().await;
}

#[tokio::test]
async fn test_claims_persist_across_restart() {
    let kp = Keypair::generate();
    let (engine, _addr) = start_engine(kp.clone()).await;
    let dir = std::env::temp_dir().join(format!("pwb-domain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("domains.json");

    let cid = ContentId([5; 32]);
    {
        let reg = registry();
        let service = Arc::new(DomainService::new(
            engine.clone(),
            Arc::clone(&reg),
            kp.clone(),
            100,
            Some(path.clone()),
        ));
        service.claim("stored.pwb", cid).expect("claim succeeds");
    }

    // A fresh service over the same file reloads the record.
    let reg = registry();
    let _service = DomainService::new(
        engine.clone(),
        Arc::clone(&reg),
        kp.clone(),
        100,
        Some(path),
    );
    assert_eq!(
        reg.lock()
            .unwrap()
            .resolve("stored.pwb")
            .unwrap()
            .target_content_id,
        cid
    );

    let _ = std::fs::remove_dir_all(&dir);
    engine.stop().await;
}

#[tokio::test]
async fn test_local_claim_validation() {
    let kp = Keypair::generate();
    let (engine, _addr) = start_engine(kp.clone()).await;
    let reg = registry();
    let svc = service(engine.clone(), Arc::clone(&reg), &kp, 60);

    // Reserved official namespace.
    assert_eq!(
        svc.claim("search.plain", ContentId([1; 32])),
        Err(ClaimError::ReservedDomain)
    );
    // Empty name.
    assert_eq!(
        svc.claim("   ", ContentId([1; 32])),
        Err(ClaimError::EmptyDomain)
    );
    // First claim spends the initial price.
    svc.claim("mine.pwb", ContentId([2; 32]))
        .expect("first claim");
    let after_first = 60 - INITIAL_CLAIM_PRICE;
    assert_eq!(svc.available_credits(), after_first);

    // The owner may re-point a name at a new content id.
    let updated = svc
        .claim("mine.pwb", ContentId([4; 32]))
        .expect("owner can update the target");
    assert_eq!(updated.target_content_id, ContentId([4; 32]));
    assert_eq!(
        reg.lock()
            .unwrap()
            .resolve("mine.pwb")
            .unwrap()
            .target_content_id,
        ContentId([4; 32]),
        "the local registry must apply the re-point"
    );
    // Re-pointing an owned name is free.
    let after_update = after_first;
    assert_eq!(svc.available_credits(), after_update);

    // A different identity cannot overwrite the claim.
    let other = service(
        engine.clone(),
        Arc::clone(&reg),
        &Keypair::generate(),
        INITIAL_CLAIM_PRICE,
    );
    assert_eq!(
        other.claim("mine.pwb", ContentId([5; 32])),
        Err(ClaimError::AlreadyRegistered)
    );

    // Not enough credits for another claim.
    assert_eq!(
        svc.claim("second.pwb", ContentId([3; 32])),
        Err(ClaimError::InsufficientCredits {
            balance: after_update,
            cost: svc.claim_cost()
        })
    );

    engine.stop().await;
}

/// Registry sync is peer input: a record that is not signed by its claimed
/// owner is ignored, while an owner-signed record merges normally.
#[tokio::test]
async fn test_registry_sync_rejects_unsigned_records() {
    use pwb_core::net::{DomainRecordWire, SyncRecordWire};

    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;

    let service_b = service(engine_b.clone(), registry(), &kp_b, 100);
    tokio::spawn(Arc::clone(&service_b).run());
    connect(&engine_a, addr_a, &kp_a, &engine_b, addr_b, &kp_b);
    tokio::time::sleep(Duration::from_millis(150)).await;

    let victim = Keypair::generate();
    let attacker = Keypair::generate();
    let cid = ContentId([0x66; 32]);
    let epoch = unix_time();

    // Attacker signs, but claims the victim as owner.
    let forged_signature = attacker.sign(&domain_claim_sign_message(
        "forged.pwb",
        cid,
        victim.public,
        50,
        epoch,
    ));
    let forged = DomainRecordWire {
        domain: "forged.pwb".into(),
        target_content_id: cid,
        owner: victim.public,
        credits_spent: 50,
        registered_at_epoch: epoch,
        signature: Some(forged_signature),
        funding_head: None,
        funding_balance: None,
    };
    let unsigned = DomainRecordWire {
        domain: "unsigned.pwb".into(),
        target_content_id: cid,
        owner: victim.public,
        credits_spent: 50,
        registered_at_epoch: epoch,
        signature: None,
        funding_head: None,
        funding_balance: None,
    };
    engine_a
        .send_to(
            &kp_b.public,
            NetworkMessage::RegistrySyncResponse {
                records: vec![
                    SyncRecordWire {
                        record: forged,
                        certificate: None,
                    },
                    SyncRecordWire {
                        record: unsigned,
                        certificate: None,
                    },
                ],
            },
        )
        .expect("A sends a poisoned sync response");
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(service_b.resolve_local("forged.pwb").is_none());
    assert!(service_b.resolve_local("unsigned.pwb").is_none());

    // A genuinely owner-signed record is accepted.
    let owner = Keypair::generate();
    let signed = DomainRecordWire {
        domain: "legit.pwb".into(),
        target_content_id: cid,
        owner: owner.public,
        credits_spent: 50,
        registered_at_epoch: epoch,
        signature: Some(owner.sign(&domain_claim_sign_message(
            "legit.pwb",
            cid,
            owner.public,
            50,
            epoch,
        ))),
        funding_head: None,
        funding_balance: None,
    };
    engine_a
        .send_to(
            &kp_b.public,
            NetworkMessage::RegistrySyncResponse {
                records: vec![SyncRecordWire {
                    record: signed,
                    certificate: None,
                }],
            },
        )
        .expect("A sends a valid sync response");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(service_b.resolve_local("legit.pwb").is_some());

    engine_a.stop().await;
    engine_b.stop().await;
}

/// The P0 registry-sync gap: a peer used to be able to gossip a record that
/// cites an invented attestor balance, because the owner's funding-bound
/// signature only proves the owner chose the citation. Sync now requires the
/// quorum certificate for the exact `(head, balance)` and verifies it against
/// this node's own deterministic neighborhood, so an uncited or fabricated
/// certificate is ignored.
#[tokio::test]
async fn test_registry_sync_rejects_unverifiable_funding_citations() {
    use pwb_core::net::{
        domain_claim_funding_sign_message, DomainRecordWire, QuorumCertificateWire, SyncRecordWire,
    };

    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;

    let service_b = service(engine_b.clone(), registry(), &kp_b, 100);
    tokio::spawn(Arc::clone(&service_b).run());
    connect(&engine_a, addr_a, &kp_a, &engine_b, addr_b, &kp_b);
    tokio::time::sleep(Duration::from_millis(150)).await;

    let attacker = Keypair::generate();
    let cid = ContentId([0x77; 32]);
    let epoch = unix_time();
    let head = [0xAB; 32];
    let balance = 1_000_000u64;
    let signature = attacker.sign(&domain_claim_funding_sign_message(
        "free.pwb",
        cid,
        attacker.public,
        50,
        epoch,
        &head,
        balance,
    ));
    let funded = DomainRecordWire {
        domain: "free.pwb".into(),
        target_content_id: cid,
        owner: attacker.public,
        credits_spent: 50,
        registered_at_epoch: epoch,
        signature: Some(signature),
        funding_head: Some(head),
        funding_balance: Some(balance),
    };

    // No certificate: the citation is unverifiable, so the record is dropped.
    engine_a
        .send_to(
            &kp_b.public,
            NetworkMessage::RegistrySyncResponse {
                records: vec![SyncRecordWire {
                    record: funded.clone(),
                    certificate: None,
                }],
            },
        )
        .expect("A sends an uncited funded record");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(service_b.resolve_local("free.pwb").is_none());

    // A self-minted certificate with no attestations is equally unverifiable.
    let fake = QuorumCertificateWire {
        subject: attacker.public,
        epoch,
        balance,
        head,
        attestations: Vec::new(),
    };
    engine_a
        .send_to(
            &kp_b.public,
            NetworkMessage::RegistrySyncResponse {
                records: vec![SyncRecordWire {
                    record: funded,
                    certificate: Some(fake),
                }],
            },
        )
        .expect("A sends a fabricated certificate");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(service_b.resolve_local("free.pwb").is_none());

    engine_a.stop().await;
    engine_b.stop().await;
}

#[tokio::test]
async fn test_registry_load_rejects_forged_records_and_writes_atomically() {
    use pwb_core::net::DomainRecordWire;
    use std::time::{SystemTime, UNIX_EPOCH};

    let kp = Keypair::generate();
    let (engine, _addr) = start_engine(kp.clone()).await;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("pwb-domain-load-{}", nanos));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("domains.json");
    let cid = ContentId([7; 32]);
    let epoch = unix_time();

    let valid = DomainRecordWire {
        domain: "valid.pwb".into(),
        target_content_id: cid,
        owner: kp.public,
        credits_spent: 50,
        registered_at_epoch: epoch,
        signature: Some(kp.sign(&domain_claim_sign_message(
            "valid.pwb",
            cid,
            kp.public,
            50,
            epoch,
        ))),
        funding_head: None,
        funding_balance: None,
    };
    let attacker = Keypair::generate();
    let forged = DomainRecordWire {
        domain: "forged.pwb".into(),
        target_content_id: cid,
        owner: kp.public,
        credits_spent: 50,
        registered_at_epoch: epoch,
        signature: Some(attacker.sign(&domain_claim_sign_message(
            "forged.pwb",
            cid,
            kp.public,
            50,
            epoch,
        ))),
        funding_head: None,
        funding_balance: None,
    };
    let value = serde_json::json!({ "version": 2, "records": vec![valid, forged] });
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

    let reg = registry();
    let svc = Arc::new(DomainService::new(
        engine.clone(),
        Arc::clone(&reg),
        kp.clone(),
        100,
        Some(path.clone()),
    ));
    // Forged disk entries are skipped; valid ones load.
    assert!(reg.lock().unwrap().resolve("valid.pwb").is_some());
    assert!(reg.lock().unwrap().resolve("forged.pwb").is_none());

    // Persisting (via a real claim) leaves no temp files behind.
    svc.claim("another.pwb", cid).expect("claim persists");
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "atomic write leaves no tmp files");
    assert!(reg.lock().unwrap().resolve("another.pwb").is_some());

    let _ = std::fs::remove_dir_all(&dir);
    engine.stop().await;
}
