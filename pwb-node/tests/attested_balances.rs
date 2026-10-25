//! End-to-end tests for peer-attested balances, blind vouchers, and remote
//! proof verification over live loopback UDP engines.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pwb_core::crypto::{ContentId, Keypair, PublicKey};
use pwb_core::net::{
    index_proof_sign_message, IndexProofWire, NetworkMessage, NodeConfig, NodeEngine,
};
use pwb_node::attestation::{
    collect_balance_certificate, request_proof_verdict, withdraw_blind, AttestationService,
};
use pwb_node::blind::{blind_claim_wire, BlindClaim, IssuerBinding};
use pwb_node::domain::{DomainRegistry, DomainService};
use pwb_node::verifier::IndexProof;

fn engine(keypair: Keypair) -> NodeEngine {
    NodeEngine::new(NodeConfig {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        keypair,
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    })
}

fn bundle_bytes(title: &str, body: &str) -> Vec<u8> {
    use pwb_core::spec::{PlainNode, Tag};
    let mut root = PlainNode::new(0, Tag::Document);
    let mut text = PlainNode::new(1, Tag::Text);
    text.text_content = Some(body.to_string());
    root.children.push(text);
    pwb_web::bundle::create_bundle(
        root,
        title.to_string(),
        None,
        None,
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    )
    .serialize()
}

async fn connected_pair() -> (NodeEngine, NodeEngine, Keypair, Keypair) {
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let a = engine(kp_a.clone());
    let b = engine(kp_b.clone());
    a.start().await.unwrap();
    b.start().await.unwrap();
    let addr_b = b.local_addr().unwrap().unwrap();
    a.connect_to(addr_b, kp_b.public).unwrap();
    (a, b, kp_a, kp_b)
}

fn domain_service(engine: &NodeEngine, keypair: &Keypair) -> Arc<DomainService> {
    let registry = Arc::new(Mutex::new(DomainRegistry::new()));
    Arc::new(DomainService::new(
        engine.clone(),
        registry,
        keypair.clone(),
        0,
        None,
    ))
}

#[tokio::test]
async fn certificate_claim_and_blind_voucher_are_verified_over_the_wire() {
    let (a, b, kp_a, kp_b) = connected_pair().await;
    let epoch = pwb_node::attestation::current_epoch();
    let service_b = Arc::new(AttestationService::new(
        b.clone(),
        kp_b.clone(),
        &kp_b.secret.0,
    ));
    service_b
        .record_earned(kp_a.public, epoch, 50, [7; 32])
        .expect("attestor records verified work");
    let handle_b = tokio::spawn(Arc::clone(&service_b).run());
    let domain_a = domain_service(&a, &kp_a);
    let handle_a = tokio::spawn(Arc::clone(&domain_a).run());
    // The attestor also runs the claim service so the gossiped blind claim is
    // verified against its own deterministic neighborhood and spend log.
    let registry_b = Arc::new(Mutex::new(DomainRegistry::new()));
    let domain_b = Arc::new(DomainService::new(
        b.clone(),
        Arc::clone(&registry_b),
        kp_b.clone(),
        0,
        None,
    ));
    let handle_domain_b = tokio::spawn(Arc::clone(&domain_b).run());

    let certificate = collect_balance_certificate(
        &a,
        kp_a.public,
        epoch,
        &[kp_b.public],
        1,
        Duration::from_secs(1),
    )
    .await
    .expect("one attestor answers");
    assert_eq!(certificate.balance, 50);
    assert_eq!(certificate.attestations.len(), 1);
    certificate.verify(1, &[kp_b.public]).unwrap();

    // The blind path pays the initial rolling price before any claim has
    // moved it, so withdraw and redeem first.
    let receipt = withdraw_blind(
        &a,
        kp_b.public,
        kp_a.public,
        epoch,
        10,
        1,
        Duration::from_secs(1),
    )
    .await
    .expect("blind withdrawal succeeds");
    assert_eq!(receipt.total, 10);
    assert_eq!(receipt.vouchers.len(), 1);
    let binding: &IssuerBinding = receipt.binding.as_ref().expect("issuer binding");
    assert!(binding.verify());

    let claim = BlindClaim::build(&receipt.vouchers[0], binding).expect("claim built");
    let blind_record = domain_a
        .claim_blind(
            "private.pwb",
            ContentId([4; 32]),
            &blind_claim_wire(&claim),
            &[kp_b.public],
        )
        .expect("blind claim accepted");
    assert_eq!(blind_record.target_content_id, ContentId([4; 32]));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        registry_b
            .lock()
            .unwrap()
            .resolve("private.pwb")
            .map(|record| record.target_content_id),
        Some(ContentId([4; 32])),
        "blind claim gossips and redeems on another node"
    );
    assert_eq!(service_b.attested_balance(&kp_a.public), 40);

    // A fresh registry keeps the initial price for the attested path; the
    // certificate collected before the withdrawal still vouches for 50.
    let domain_attested = domain_service(&a, &kp_a);
    let handle_attested = tokio::spawn(Arc::clone(&domain_attested).run());
    let cid = ContentId([3; 32]);
    let record = domain_attested
        .claim_attested("attested.pwb", cid, &certificate, &[kp_b.public])
        .expect("attested claim accepted");
    assert_eq!(record.target_content_id, cid);

    handle_domain_b.abort();
    handle_attested.abort();
    handle_a.abort();
    handle_b.abort();
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn exhausted_certificate_cannot_fund_a_second_claim_over_the_wire() {
    // The attested double-spend, live: one certificate (balance exactly the
    // claim price) funds "one.pwb"; a second claim against the same
    // certificate — even from a fresh local registry that has not seen the
    // first spend — must be refused by the receiving peer.
    let (a, b, kp_a, kp_b) = connected_pair().await;
    let epoch = pwb_node::attestation::current_epoch();
    let service_b = Arc::new(AttestationService::new(
        b.clone(),
        kp_b.clone(),
        &kp_b.secret.0,
    ));
    let price = DomainRegistry::new().claim_cost();
    service_b
        .record_earned(kp_a.public, epoch, price, [7; 32])
        .expect("attestor records verified work");
    let handle_b = tokio::spawn(Arc::clone(&service_b).run());
    let domain_a = domain_service(&a, &kp_a);
    let handle_a = tokio::spawn(Arc::clone(&domain_a).run());
    let registry_b = Arc::new(Mutex::new(DomainRegistry::new()));
    let domain_b = Arc::new(DomainService::new(
        b.clone(),
        Arc::clone(&registry_b),
        kp_b.clone(),
        0,
        None,
    ));
    let handle_domain_b = tokio::spawn(Arc::clone(&domain_b).run());

    let certificate = collect_balance_certificate(
        &a,
        kp_a.public,
        epoch,
        &[kp_b.public],
        1,
        Duration::from_secs(1),
    )
    .await
    .expect("one attestor answers");
    assert_eq!(certificate.balance, price);

    let first = domain_a
        .claim_attested("one.pwb", ContentId([3; 32]), &certificate, &[kp_b.public])
        .expect("first claim fits the certificate");
    assert!(first.funding.is_some(), "the record cites its funding");
    assert_eq!(first.funding.map(|funding| funding.balance), Some(price));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        registry_b.lock().unwrap().resolve("one.pwb").is_some(),
        "the funded claim gossips and applies on the peer"
    );

    // A fresh local registry has no memory of the spend, so the second
    // claim passes the originator check and reaches the wire — the peer
    // must refuse it from its own funded-record accounting.
    let domain_a2 = domain_service(&a, &kp_a);
    domain_a2
        .claim_attested("two.pwb", ContentId([4; 32]), &certificate, &[kp_b.public])
        .expect("originator without spend history broadcasts");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        registry_b.lock().unwrap().resolve("two.pwb").is_none(),
        "the peer refuses the double-spend of one certificate"
    );
    assert_eq!(
        registry_b.lock().unwrap().claimed_by(&kp_a.public),
        price,
        "peer accounting matches the single surviving claim"
    );

    handle_domain_b.abort();
    handle_a.abort();
    handle_b.abort();
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn remote_peer_verifies_index_proof_against_real_bytes() {
    let (a, b, kp_a, kp_b) = connected_pair().await;
    let service_b = Arc::new(AttestationService::new(
        b.clone(),
        kp_b.clone(),
        &kp_b.secret.0,
    ));
    let handle_b = tokio::spawn(Arc::clone(&service_b).run());

    let bytes = bundle_bytes("Proof Page", "verifiable indexing work");
    let proof = IndexProof::sign(&kp_a, ContentId::compute(&bytes), ContentId([9; 32]));
    let wire = IndexProofWire {
        miner: proof.miner,
        content_id: proof.content_id,
        term_hash: proof.term_hash,
        signature: kp_a.sign(&index_proof_sign_message(
            &proof.content_id,
            &proof.term_hash,
        )),
    };
    let verdict =
        request_proof_verdict(&a, kp_b.public, wire, bytes.clone(), Duration::from_secs(1)).await;
    assert_eq!(verdict, Some(false), "a fabricated term hash is rejected");

    let bundle = pwb_core::wire::Bundle::deserialize(&bytes).unwrap();
    let real_terms = pwb_node::credit::canonical_term_hash(&bundle);
    let real_wire = IndexProofWire {
        miner: kp_a.public,
        content_id: ContentId::compute(&bytes),
        term_hash: real_terms,
        signature: kp_a.sign(&index_proof_sign_message(
            &ContentId::compute(&bytes),
            &real_terms,
        )),
    };
    let verdict =
        request_proof_verdict(&a, kp_b.public, real_wire, bytes, Duration::from_secs(1)).await;
    assert_eq!(verdict, Some(true), "recomputed terms verify remotely");

    handle_b.abort();
    a.stop().await;
    b.stop().await;
}

/// A stranger on a fresh bootstrap: its only direct peer is a non-anchor
/// seed, and its pinned anchors are never in its peer table. The collect side
/// must discover the anchors through the seed's routing table (DHT walk),
/// dial them, collect their attestations, and produce a certificate the
/// anchored verifier accepts — proving the collect and verify sides agree
/// even when the anchors were never direct peers.
#[tokio::test]
async fn anchored_stranger_discovers_and_collects_from_pinned_anchors_via_dht() {
    let stranger = Keypair::generate();
    let seed = Keypair::generate();
    let anchors: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();

    // Five loopback engines: stranger, non-anchor seed, three anchors.
    let engine_stranger = engine(stranger.clone());
    let engine_seed = engine(seed.clone());
    let engine_anchors: Vec<NodeEngine> = anchors.iter().map(|kp| engine(kp.clone())).collect();
    engine_stranger.start().await.unwrap();
    engine_seed.start().await.unwrap();
    for anchor_engine in &engine_anchors {
        anchor_engine.start().await.unwrap();
    }

    // Each anchor connects to the seed (so the seed's routing table knows
    // all three), and the stranger connects only to the seed.
    let addr_seed = engine_seed.local_addr().unwrap().unwrap();
    for (anchor_engine, anchor_kp) in engine_anchors.iter().zip(anchors.iter()) {
        let addr = anchor_engine.local_addr().unwrap().unwrap();
        engine_seed.connect_to(addr, anchor_kp.public).unwrap();
        anchor_engine.connect_to(addr_seed, seed.public).unwrap();
    }
    engine_stranger.connect_to(addr_seed, seed.public).unwrap();

    // Every anchor has verified the stranger's work and will vouch for its
    // balance when asked.
    let epoch = pwb_node::attestation::current_epoch();
    let balance = 100u64;
    let mut anchor_handles = Vec::new();
    for (anchor_engine, anchor_kp) in engine_anchors.iter().zip(anchors.iter()) {
        let service = Arc::new(AttestationService::new(
            anchor_engine.clone(),
            anchor_kp.clone(),
            &anchor_kp.secret.0,
        ));
        service
            .record_earned(stranger.public, epoch, balance, [7; 32])
            .expect("attestor records verified work");
        anchor_handles.push(tokio::spawn(Arc::clone(&service).run()));
    }

    // The stranger's domain pins the three anchors as its funding authority.
    let registry = Arc::new(Mutex::new(DomainRegistry::new()));
    let domain = Arc::new(DomainService::new_with_trust(
        engine_stranger.clone(),
        registry,
        stranger.clone(),
        0,
        None,
        anchors.iter().map(|a| a.public).collect(),
    ));
    let domain_handle = tokio::spawn(Arc::clone(&domain).run());

    // Collect + claim through the normal wallet path. The stranger never
    // connects directly to the anchors; `ensure_attestors_reachable` must
    // discover them through the seed's routing table and dial them.
    let record = pwb_node::wallet::attested_claim(
        &engine_stranger,
        &domain,
        "stranger.pwb",
        ContentId([9; 32]),
        Duration::from_secs(4),
    )
    .await
    .expect("anchored stranger claims a name against DHT-discovered anchors");
    assert_eq!(record.target_content_id, ContentId([9; 32]));

    domain_handle.abort();
    for handle in anchor_handles {
        handle.abort();
    }
    engine_stranger.stop().await;
    engine_seed.stop().await;
    for anchor_engine in &engine_anchors {
        anchor_engine.stop().await;
    }
}

#[allow(dead_code)]
fn _assert_send_sync(_: PublicKey, _: SocketAddr, _: fn(NetworkMessage)) {}
