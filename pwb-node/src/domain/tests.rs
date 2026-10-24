use super::*;
use crate::ledger::{BalanceAttestation, LedgerError, QuorumCertificate};
use pwb_core::crypto::{ContentId, Keypair, PublicKey};
use pwb_core::net::{
    domain_claim_funding_sign_message, domain_claim_sign_message, DomainRecordWire, NodeConfig,
    NodeEngine, QuorumCertificateWire,
};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[test]
fn test_domain_registry_security_rules() {
    let mut reg = DomainRegistry::new();
    let cid = ContentId([1; 32]);
    let pk = PublicKey([2; 32]);

    // Attempting to claim .plain domain MUST fail!
    assert!(reg.register("search.plain", cid, pk).is_err());
    assert!(reg.register("malicious.plain", cid, pk).is_err());
    assert!(reg.register("hacked.plain", cid, pk).is_err());

    // Registering an unofficial user domain .pwb MUST succeed!
    assert!(reg.register("mysite.pwb", cid, pk).is_ok());
    assert!(reg.register("mysite.pwb", cid, pk).is_err()); // duplicate check

    let resolved = reg.resolve("MYSITE.PWB").unwrap();
    assert_eq!(resolved.target_content_id, cid);
}

fn attest_quorum(
    subject: PublicKey,
    epoch: u64,
    balance: u64,
    head: [u8; 32],
    keys: &[pwb_core::crypto::Keypair],
) -> (QuorumCertificate, Vec<PublicKey>) {
    let attestations = keys
        .iter()
        .map(|key| BalanceAttestation::sign(key, subject, epoch, balance, head))
        .collect();
    let neighborhood: Vec<PublicKey> = keys.iter().map(|k| k.public).collect();
    (
        QuorumCertificate::from_attestations(attestations).expect("quorum"),
        neighborhood,
    )
}

fn test_engine(keypair: &Keypair) -> NodeEngine {
    NodeEngine::new(NodeConfig {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        keypair: keypair.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 16,
        heartbeat_interval: Duration::from_secs(30),
    })
}

fn anchored_service(keypair: &Keypair, anchors: &[Keypair]) -> DomainService {
    DomainService::new_with_trust(
        test_engine(keypair),
        Arc::new(Mutex::new(DomainRegistry::new())),
        keypair.clone(),
        0,
        None,
        anchors.iter().map(|key| key.public).collect(),
    )
}

#[test]
fn test_pinned_trust_anchors_are_the_funding_authority() {
    let owner = Keypair::generate();
    let anchors: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
    let rogue: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
    let (valid, _) = attest_quorum(owner.public, 4, 100, [7; 32], &anchors);
    let (forged, _) = attest_quorum(owner.public, 4, 100, [7; 32], &rogue);

    // The historical behavior has no independent trust root: with an empty
    // peer table there is no way for anchor-signed funding to verify.
    let unanchored = DomainService::new(
        test_engine(&owner),
        Arc::new(Mutex::new(DomainRegistry::new())),
        owner.clone(),
        0,
        None,
    );
    assert_eq!(unanchored.trust_quorum(), None);
    assert!(unanchored.verify_funding_certificate(&valid).is_err());

    // Pinned anchors verify even on a node that has never seen a peer, and
    // still reject a quorum of unpinned keys that looks perfectly valid.
    let anchored = anchored_service(&owner, &anchors);
    assert_eq!(anchored.trust_quorum(), Some((3, 3)));
    assert!(anchored.verify_funding_certificate(&valid).is_ok());
    assert!(anchored.verify_funding_certificate(&forged).is_err());
}

#[test]
fn test_pinned_anchors_gate_claims_and_require_the_full_small_quorum() {
    let owner = Keypair::generate();
    let anchors: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
    let rogue: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
    let (valid, _) = attest_quorum(owner.public, 4, 100, [7; 32], &anchors);
    let (forged, _) = attest_quorum(owner.public, 4, 100, [7; 32], &rogue);
    let service = anchored_service(&owner, &anchors);
    let cid = ContentId([1; 32]);

    assert!(matches!(
        service.claim_attested("evil.pwb", cid, &forged, &[]),
        Err(ClaimError::Attestation(_))
    ));

    // Three pinned anchors means 3-of-3 for a small deployment; a 2-of-3
    // certificate is not enough just because a majority signed it.
    let partial = QuorumCertificate::from_attestations(
        anchors[..2]
            .iter()
            .map(|key| BalanceAttestation::sign(key, owner.public, 4, 100, [7; 32]))
            .collect(),
    )
    .expect("partial certificate");
    assert!(service
        .claim_attested("partial.pwb", cid, &partial, &[])
        .is_err());

    let record = service
        .claim_attested("good.pwb", cid, &valid, &[])
        .expect("the pinned quorum funds the claim");
    assert_eq!(record.owner, owner.public);
    assert_eq!(
        record.funding,
        Some(ClaimFunding {
            head: [7; 32],
            balance: 100
        })
    );
}

#[test]
fn test_forged_certificate_does_not_poison_the_sync_cache() {
    let owner = Keypair::generate();
    let anchors: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
    let rogue: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
    let service = anchored_service(&owner, &anchors);
    let from: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let cid = ContentId([2; 32]);

    let (forged, _) = attest_quorum(owner.public, 1, 100, [3; 32], &rogue);
    let forged_signature = owner.sign(&domain_claim_funding_sign_message(
        "evil.pwb",
        cid,
        owner.public,
        INITIAL_CLAIM_PRICE,
        1,
        &[3; 32],
        100,
    ));
    service.on_attested_claim(
        from,
        "evil.pwb".to_string(),
        cid,
        owner.public,
        INITIAL_CLAIM_PRICE,
        1,
        QuorumCertificateWire::from(&forged),
        Vec::new(),
        [3; 32],
        100,
        forged_signature,
    );
    assert_eq!(
        service.cached_funding_certificate_count(),
        0,
        "a forged certificate must not be cached for registry sync"
    );
    assert!(service.resolve_local("evil.pwb").is_none());

    let (valid, _) = attest_quorum(owner.public, 1, 100, [4; 32], &anchors);
    let valid_signature = owner.sign(&domain_claim_funding_sign_message(
        "good.pwb",
        cid,
        owner.public,
        INITIAL_CLAIM_PRICE,
        1,
        &[4; 32],
        100,
    ));
    service.on_attested_claim(
        from,
        "good.pwb".to_string(),
        cid,
        owner.public,
        INITIAL_CLAIM_PRICE,
        1,
        QuorumCertificateWire::from(&valid),
        Vec::new(),
        [4; 32],
        100,
        valid_signature,
    );
    assert_eq!(service.cached_funding_certificate_count(), 1);
    assert!(service.resolve_local("good.pwb").is_some());
}

#[test]
fn test_loading_with_anchors_drops_persisted_records_that_do_not_verify() {
    let owner = Keypair::generate();
    let anchors: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
    let rogue: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
    let cid = ContentId([6; 32]);

    let (valid_cert, _) = attest_quorum(owner.public, 1, 100, [5; 32], &anchors);
    let (forged_cert, _) = attest_quorum(owner.public, 1, 100, [6; 32], &rogue);
    let make_record = |domain: &str, head: [u8; 32]| DomainRecordWire {
        domain: domain.to_string(),
        target_content_id: cid,
        owner: owner.public,
        credits_spent: INITIAL_CLAIM_PRICE,
        registered_at_epoch: 1,
        signature: Some(owner.sign(&domain_claim_funding_sign_message(
            domain,
            cid,
            owner.public,
            INITIAL_CLAIM_PRICE,
            1,
            &head,
            100,
        ))),
        funding_head: Some(head),
        funding_balance: Some(100),
    };

    let dir = std::env::temp_dir().join(format!("pwb-anchor-load-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("domains.json");
    let value = serde_json::json!({
        "version": 3,
        "records": [
            make_record("kept.pwb", [5; 32]),
            make_record("forged.pwb", [6; 32]),
        ],
        "certificates": [
            QuorumCertificateWire::from(&valid_cert),
            QuorumCertificateWire::from(&forged_cert),
        ],
    });
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

    let service = DomainService::new_with_trust(
        test_engine(&owner),
        Arc::new(Mutex::new(DomainRegistry::new())),
        owner.clone(),
        0,
        Some(path),
        anchors.iter().map(|key| key.public).collect(),
    );
    assert!(service.resolve_local("kept.pwb").is_some());
    assert!(
        service.resolve_local("forged.pwb").is_none(),
        "a persisted record whose certificate is not anchor-signed must be dropped"
    );
    assert_eq!(service.cached_funding_certificate_count(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_attested_claim_requires_valid_quorum_and_spends_balance() {
    let mut reg = DomainRegistry::new();
    let cid = ContentId([1; 32]);
    let subject = pwb_core::crypto::Keypair::generate().public;
    let keys: Vec<pwb_core::crypto::Keypair> = (0..3)
        .map(|_| pwb_core::crypto::Keypair::generate())
        .collect();
    let (cert, neighborhood) = attest_quorum(subject, 4, 100, [7; 32], &keys);

    // Reserved and duplicate checks apply on the attested path too.
    assert_eq!(
        reg.register_attested("search.plain", cid, &cert, 2, &neighborhood),
        Err(LedgerError::ReservedSystemDomain)
    );

    // k=2 of the 3-member neighborhood claims and records the spend at
    // the registry's current (initial) rolling price.
    assert_eq!(
        reg.register_attested("mysite.pwb", cid, &cert, 2, &neighborhood),
        Ok(())
    );
    assert_eq!(reg.claimed_by(&subject), INITIAL_CLAIM_PRICE);
    let record = reg.resolve("mysite.pwb").expect("registered");
    assert_eq!(record.owner, subject);
    assert_eq!(record.registered_at_epoch, 4);

    assert_eq!(
        reg.register_attested("mysite.pwb", cid, &cert, 2, &neighborhood),
        Err(LedgerError::DomainAlreadyRegistered)
    );

    // Replaying the same certificate cannot overdraw: spend the remaining
    // attested balance one claim at a time, then the next claim is refused.
    assert_eq!(
        reg.register_attested("second.pwb", cid, &cert, 2, &neighborhood),
        Ok(())
    );
    for i in 0..8 {
        reg.register_attested(&format!("fill{i}.pwb"), cid, &cert, 2, &neighborhood)
            .expect("claim within the attested balance");
    }
    assert_eq!(reg.claimed_by(&subject), 100);
    assert_eq!(
        reg.register_attested("third.pwb", cid, &cert, 2, &neighborhood),
        Err(LedgerError::InsufficientAttestedBalance {
            attested: 100,
            already_claimed: 100,
            needed: INITIAL_CLAIM_PRICE
        })
    );

    // A threshold above the certificate's quorum is refused.
    assert_eq!(
        reg.register_attested("fourth.pwb", cid, &cert, 4, &neighborhood),
        Err(LedgerError::QuorumTooSmall { have: 3, need: 4 })
    );
}

#[test]
fn test_attested_claim_rejects_forged_and_stranger_quorums() {
    let mut reg = DomainRegistry::new();
    let cid = ContentId([2; 32]);
    let subject = pwb_core::crypto::Keypair::generate().public;
    let keys: Vec<pwb_core::crypto::Keypair> = (0..2)
        .map(|_| pwb_core::crypto::Keypair::generate())
        .collect();
    let (mut cert, neighborhood) = attest_quorum(subject, 1, 60, [3; 32], &keys);

    // Inflating the envelope balance breaks attestation agreement.
    cert.balance = 6000;
    assert_eq!(
        reg.register_attested("evil.pwb", cid, &cert, 2, &neighborhood),
        Err(LedgerError::AttestationMismatch)
    );

    // A stranger's signature does not count toward the quorum.
    let (cert, neighborhood) = attest_quorum(subject, 1, 60, [3; 32], &keys);
    let stranger = pwb_core::crypto::Keypair::generate();
    let mut with_stranger = cert.clone();
    with_stranger
        .attestations
        .push(crate::ledger::BalanceAttestation::sign(
            &stranger, subject, 1, 60, [3; 32],
        ));
    assert_eq!(
        reg.register_attested("evil.pwb", cid, &with_stranger, 3, &neighborhood),
        Err(LedgerError::SignerNotInNeighborhood)
    );
}

fn stats(epoch: u64, registrations: u64, network_work: u64, node_count: u64) -> PricingEpochStats {
    PricingEpochStats {
        epoch,
        registrations,
        network_work,
        node_count,
    }
}

/// Balanced conditions: 4 claims/epoch, 10 nodes × 1_000 credits of work.
fn equilibrium(epoch: u64) -> PricingEpochStats {
    stats(epoch, 4, 10_000, 10)
}

#[test]
fn test_rolling_pricing_starts_at_initial_and_stays_at_equilibrium() {
    let mut pricing = RollingPricing::new();
    assert_eq!(pricing.current_price(), INITIAL_CLAIM_PRICE);
    for epoch in 1..=12 {
        pricing.record_epoch(equilibrium(epoch));
    }
    assert_eq!(
        pricing.current_price(),
        INITIAL_CLAIM_PRICE,
        "balanced demand and supply keep the price stable"
    );
    assert_eq!(pricing.history_len(), PRICING_WINDOW);
}

#[test]
fn test_rolling_pricing_rises_with_demand_and_falls_with_glut() {
    let mut pricing = RollingPricing::new();

    // Hot market: registration flow far above the target, thin work
    // capacity. The price climbs every epoch until it hits the ceiling.
    let mut last = pricing.current_price();
    for epoch in 1..=6 {
        pricing.record_epoch(stats(epoch, 12, 1_000, 1));
        let price = pricing.current_price();
        assert!(
            price > last || price == MAX_CLAIM_PRICE,
            "epoch {epoch}: {price} should exceed {last} or be at the ceiling"
        );
        last = price;
    }
    assert!(last > INITIAL_CLAIM_PRICE * 2);

    // Cold market: zero claims, abundant minting. The price decays to
    // the floor.
    for epoch in 7..=40 {
        pricing.record_epoch(stats(epoch, 0, 100_000, 100));
    }
    assert_eq!(pricing.current_price(), MIN_CLAIM_PRICE);
}

#[test]
fn test_rolling_pricing_clamps_to_bounds() {
    let mut pricing = RollingPricing::new();
    for epoch in 1..=40 {
        pricing.record_epoch(stats(epoch, u64::MAX / 4, 1, 1));
    }
    assert_eq!(pricing.current_price(), MAX_CLAIM_PRICE);

    let mut pricing = RollingPricing::new();
    for epoch in 1..=40 {
        pricing.record_epoch(stats(epoch, 0, u64::MAX / 4, u64::MAX / 4));
    }
    assert_eq!(pricing.current_price(), MIN_CLAIM_PRICE);
}

#[test]
fn test_rolling_pricing_ignores_stale_or_duplicate_epochs() {
    let mut pricing = RollingPricing::new();
    pricing.record_epoch(equilibrium(5));
    let price = pricing.current_price();
    // Same or older epochs are dropped without moving the price or
    // growing the window.
    pricing.record_epoch(equilibrium(5));
    pricing.record_epoch(equilibrium(3));
    assert_eq!(pricing.current_price(), price);
    assert_eq!(pricing.history_len(), 1);
    // A newer epoch is accepted.
    pricing.record_epoch(stats(6, 40, 0, 1));
    assert_eq!(pricing.history_len(), 2);
    assert!(pricing.current_price() > price);
}

#[test]
fn test_registry_claim_price_is_dynamic_not_a_constant() {
    let mut reg = DomainRegistry::new();
    let cid = ContentId([1; 32]);
    let subject = pwb_core::crypto::Keypair::generate().public;
    let keys: Vec<pwb_core::crypto::Keypair> = (0..3)
        .map(|_| pwb_core::crypto::Keypair::generate())
        .collect();

    assert_eq!(reg.claim_cost(), INITIAL_CLAIM_PRICE);

    // A hot epoch pushes the price far above the initial constant.
    for epoch in 1..=4 {
        reg.record_pricing_epoch(stats(epoch, 40, 1_000, 1));
    }
    let hot = reg.claim_cost();
    assert!(hot > INITIAL_CLAIM_PRICE);

    // The elevated price now gates the attested claim path: a 60-credit
    // balance that covered two claims at the initial price covers none.
    let (cert, neighborhood) = attest_quorum(subject, 1, 60, [3; 32], &keys);
    assert_eq!(
        reg.register_attested("expensive.pwb", cid, &cert, 2, &neighborhood),
        Err(LedgerError::InsufficientAttestedBalance {
            attested: 60,
            already_claimed: 0,
            needed: hot
        })
    );

    // A balance above the hot price still clears.
    let (rich, neighborhood) = attest_quorum(subject, 2, hot, [9; 32], &keys);
    assert_eq!(
        reg.register_attested("expensive.pwb", cid, &rich, 2, &neighborhood),
        Ok(())
    );
    assert_eq!(reg.claimed_by(&subject), hot);
}

#[test]
fn test_apply_record_inserts_updates_and_ignores_stale_or_duplicate() {
    let cid = ContentId([9; 32]);
    let low = PublicKey([1; 32]);
    let high = PublicKey([2; 32]);
    let mut reg = DomainRegistry::new();

    fn record(domain: &str, cid: ContentId, owner: PublicKey, epoch: u64) -> DomainRecord {
        DomainRecord {
            domain: domain.into(),
            target_content_id: cid,
            owner,
            credits_spent: 0,
            registered_at_epoch: epoch,
            signature: None,
            funding: None,
        }
    }

    // A fresh record inserts and reports a change.
    assert!(reg.apply_record(record("site.pwb", cid, high, 3)));
    assert!(reg.contains("SITE.PWB"));
    assert!(!reg.contains("other.pwb"));

    // Re-applying the identical record is a no-op.
    assert!(!reg.apply_record(record("site.pwb", cid, high, 3)));

    // A lower epoch is stale and ignored.
    assert!(!reg.apply_record(record("site.pwb", cid, low, 2)));
    assert_eq!(reg.resolve("site.pwb").unwrap().owner, high);

    // A higher epoch supersedes.
    assert!(reg.apply_record(record("site.pwb", cid, low, 5)));
    assert_eq!(reg.resolve("site.pwb").unwrap().owner, low);
    assert_eq!(reg.resolve("site.pwb").unwrap().registered_at_epoch, 5);

    // Same epoch tiebreaks on the lexicographically smaller owner key.
    assert!(reg.apply_record(record("site.pwb", cid, PublicKey([0; 32]), 5)));
    assert_eq!(reg.resolve("site.pwb").unwrap().owner, PublicKey([0; 32]));
    assert!(!reg.apply_record(record("site.pwb", cid, high, 5)));

    assert_eq!(reg.records_snapshot().len(), 1);
}

#[test]
fn test_domain_registry_merge_keeps_domains_unique_to_each_side() {
    let mut a = DomainRegistry::new();
    let mut b = DomainRegistry::new();
    let cid_a = ContentId([1; 32]);
    let cid_b = ContentId([2; 32]);
    let owner_a = PublicKey([3; 32]);
    let owner_b = PublicKey([4; 32]);
    assert!(a.register("alpha.pwb", cid_a, owner_a).is_ok());
    assert!(b.register("beta.pwb", cid_b, owner_b).is_ok());

    a.merge(&b);

    assert!(a.resolve("alpha.pwb").is_some());
    assert!(a.resolve("beta.pwb").is_some());
    assert_eq!(a.count(), 2);
}

#[test]
fn test_domain_registry_merge_conflict_higher_epoch_wins() {
    let cid = ContentId([9; 32]);
    let alice = PublicKey([10; 32]);
    let bob = PublicKey([11; 32]);
    let mut a = DomainRegistry::new();
    let mut b = DomainRegistry::new();
    a.records.insert(
        "contested.pwb".into(),
        DomainRecord {
            domain: "contested.pwb".into(),
            target_content_id: cid,
            owner: alice,
            credits_spent: 0,
            registered_at_epoch: 3,
            signature: None,
            funding: None,
        },
    );
    b.records.insert(
        "contested.pwb".into(),
        DomainRecord {
            domain: "contested.pwb".into(),
            target_content_id: cid,
            owner: bob,
            credits_spent: 0,
            registered_at_epoch: 7,
            signature: None,
            funding: None,
        },
    );
    a.merge(&b);
    let record = a.resolve("contested.pwb").expect("merged record exists");
    assert_eq!(record.owner, bob);
    assert_eq!(record.registered_at_epoch, 7);
    assert_eq!(record.target_content_id, cid);
}

#[test]
fn test_domain_registry_merge_same_epoch_tiebreaks_by_owner() {
    let cid = ContentId([9; 32]);
    let low = PublicKey([1; 32]);
    let high = PublicKey([255; 32]);

    // a holds the high-owner claim, b the low-owner claim, same epoch.
    let mut a = DomainRegistry::new();
    let mut b = DomainRegistry::new();
    a.records.insert(
        "tie.pwb".into(),
        DomainRecord {
            domain: "tie.pwb".into(),
            target_content_id: cid,
            owner: high,
            credits_spent: 0,
            registered_at_epoch: 5,
            signature: None,
            funding: None,
        },
    );
    b.records.insert(
        "tie.pwb".into(),
        DomainRecord {
            domain: "tie.pwb".into(),
            target_content_id: cid,
            owner: low,
            credits_spent: 0,
            registered_at_epoch: 5,
            signature: None,
            funding: None,
        },
    );
    a.merge(&b);
    assert_eq!(a.resolve("tie.pwb").unwrap().owner, low);

    // Symmetric: merging in the opposite order yields the same winner.
    let mut c = DomainRegistry::new();
    let mut d = DomainRegistry::new();
    c.records.insert(
        "tie.pwb".into(),
        DomainRecord {
            domain: "tie.pwb".into(),
            target_content_id: cid,
            owner: low,
            credits_spent: 0,
            registered_at_epoch: 5,
            signature: None,
            funding: None,
        },
    );
    d.records.insert(
        "tie.pwb".into(),
        DomainRecord {
            domain: "tie.pwb".into(),
            target_content_id: cid,
            owner: high,
            credits_spent: 0,
            registered_at_epoch: 5,
            signature: None,
            funding: None,
        },
    );
    d.merge(&c);
    assert_eq!(d.resolve("tie.pwb").unwrap().owner, low);
}

/// A funded record builder for merge/repair tests: `spent` credits cited
/// against attested `(head, balance)`.
fn funded_record(
    domain: &str,
    cid: ContentId,
    owner: PublicKey,
    spent: u64,
    epoch: u64,
    head: [u8; 32],
    balance: u64,
) -> DomainRecord {
    DomainRecord {
        domain: domain.into(),
        target_content_id: cid,
        owner,
        credits_spent: spent,
        registered_at_epoch: epoch,
        signature: None,
        funding: Some(ClaimFunding { head, balance }),
    }
}

#[test]
fn test_domain_registry_merge_spend_derived_from_records_and_idempotent() {
    let owner = PublicKey([7; 32]);
    let cid = ContentId([8; 32]);
    let mut a = DomainRegistry::new();
    let mut b = DomainRegistry::new();
    // The same claim seen on both sides counts once, not twice.
    let record = funded_record("site.pwb", cid, owner, 50, 3, [1; 32], 100);
    a.records.insert("site.pwb".into(), record.clone());
    b.records.insert("site.pwb".into(), record);
    a.merge(&b);
    assert_eq!(a.claimed_by(&owner), 50);

    // A distinct funded name on the other side adds its spend.
    b.records.insert(
        "other.pwb".into(),
        funded_record("other.pwb", cid, owner, 30, 4, [1; 32], 100),
    );
    a.merge(&b);
    assert_eq!(a.claimed_by(&owner), 80);

    // Re-merging the same side is a no-op.
    let snapshot = a.clone();
    a.merge(&b);
    assert_eq!(a, snapshot);
}

#[test]
fn test_domain_registry_merge_repairs_same_certificate_double_spend() {
    // Two partitions each accept a different claim against the same
    // certificate (head [1; 32], balance 10, price 10): the documented
    // attested double-spend. On rejoin only the first-spent claim
    // survives, and the accounting matches the survivor.
    let owner = PublicKey([7; 32]);
    let cid = ContentId([8; 32]);
    let mut a = DomainRegistry::new();
    let mut b = DomainRegistry::new();
    a.records.insert(
        "first.pwb".into(),
        funded_record("first.pwb", cid, owner, 10, 3, [1; 32], 10),
    );
    b.records.insert(
        "second.pwb".into(),
        funded_record("second.pwb", cid, owner, 10, 4, [1; 32], 10),
    );
    a.merge(&b);
    assert!(
        a.resolve("first.pwb").is_some(),
        "first-spent claim keeps its funding"
    );
    assert!(
        a.resolve("second.pwb").is_none(),
        "the overspending claim is evicted on merge"
    );
    assert_eq!(a.claimed_by(&owner), 10);

    // Repair is stable: merging again changes nothing.
    let snapshot = a.clone();
    a.merge(&b);
    assert_eq!(a, snapshot);
}

#[test]
fn test_funded_record_wire_verify_binds_funding_to_signature() {
    let owner = Keypair::generate();
    let cid = ContentId([8; 32]);
    let epoch = 7u64;
    let cost = INITIAL_CLAIM_PRICE;
    let head = [1u8; 32];
    let balance = 100u64;

    let funded = DomainRecordWire {
        domain: "funded.pwb".into(),
        target_content_id: cid,
        owner: owner.public,
        credits_spent: cost,
        registered_at_epoch: epoch,
        signature: Some(owner.sign(&domain_claim_funding_sign_message(
            "funded.pwb",
            cid,
            owner.public,
            cost,
            epoch,
            &head,
            balance,
        ))),
        funding_head: Some(head),
        funding_balance: Some(balance),
    };
    assert!(funded.verify());
    // The funded record converts with its citation intact.
    let record = DomainRecord::from(funded.clone());
    assert_eq!(record.funding, Some(ClaimFunding { head, balance }));
    assert_eq!(DomainRecordWire::from(&record), funded);

    // Swapping the cited head breaks the owner signature.
    let mut swapped = funded.clone();
    swapped.funding_head = Some([2u8; 32]);
    assert!(!swapped.verify());

    // Inflating the cited balance breaks it too.
    let mut inflated = funded.clone();
    inflated.funding_balance = Some(balance * 10);
    assert!(!inflated.verify());

    // Stripping the citation while keeping a funding-bound signature fails:
    // the legacy statement is a different message.
    let mut stripped = funded.clone();
    stripped.funding_head = None;
    stripped.funding_balance = None;
    assert!(!stripped.verify());

    // A half-present citation never verifies.
    let mut half = funded.clone();
    half.funding_balance = None;
    assert!(!half.verify());

    // Legacy unfunded records with legacy signatures still verify.
    let legacy = DomainRecordWire {
        funding_head: None,
        funding_balance: None,
        signature: Some(owner.sign(&domain_claim_sign_message(
            "funded.pwb",
            cid,
            owner.public,
            cost,
            epoch,
        ))),
        ..funded.clone()
    };
    assert!(legacy.verify());
    assert_eq!(DomainRecord::from(legacy).funding, None);
}

#[test]
fn test_domain_registry_merge_keeps_fitting_same_head_claims() {
    // Two claims against one head whose combined spend fits the balance
    // both survive: repair only evicts genuine overspends.
    let owner = PublicKey([7; 32]);
    let cid = ContentId([8; 32]);
    let mut a = DomainRegistry::new();
    let mut b = DomainRegistry::new();
    a.records.insert(
        "first.pwb".into(),
        funded_record("first.pwb", cid, owner, 10, 3, [1; 32], 25),
    );
    b.records.insert(
        "second.pwb".into(),
        funded_record("second.pwb", cid, owner, 10, 4, [1; 32], 25),
    );
    a.merge(&b);
    assert!(a.resolve("first.pwb").is_some());
    assert!(a.resolve("second.pwb").is_some());
    assert_eq!(a.claimed_by(&owner), 20);
}

#[test]
fn test_rolling_pricing_merge_takes_max_price() {
    let mut hot = RollingPricing::new();
    for epoch in 1..=6 {
        hot.record_epoch(stats(epoch, 12, 1_000, 1));
    }
    let hot_price = hot.current_price();
    assert!(hot_price > INITIAL_CLAIM_PRICE);

    let mut cold = RollingPricing::new();
    for epoch in 1..=6 {
        cold.record_epoch(stats(epoch, 0, 100_000, 100));
    }
    let cold_price = cold.current_price();
    assert_eq!(cold_price, MIN_CLAIM_PRICE);

    hot.merge(&cold);
    assert_eq!(hot.current_price(), hot_price.max(cold_price));

    // The merged window contains both partitions' epoch-1 observations,
    // so a subsequent balanced epoch is priced against the combined
    // demand/supply and tempers the hot price.
    hot.record_epoch(stats(7, 4, 10_000, 10));
    let after = hot.current_price();
    assert!(after <= hot_price, "{after} should not exceed {hot_price}");
}

#[test]
fn test_rolling_pricing_merge_unions_epoch_history() {
    let mut a = RollingPricing::new();
    a.record_epoch(stats(1, 4, 10_000, 10));
    a.record_epoch(stats(2, 8, 20_000, 10));
    let mut b = RollingPricing::new();
    b.record_epoch(stats(2, 2, 5_000, 5)); // shared epoch 2: merged, not duplicated
    b.record_epoch(stats(3, 0, 100_000, 100));
    let a_price = a.current_price();
    let b_price = b.current_price();

    a.merge(&b);
    assert_eq!(a.history_len(), 3); // epochs 1, 2, 3 across both partitions
    assert_eq!(a.current_price(), a_price.max(b_price));
}

#[test]
fn test_domain_registry_merge_is_commutative_and_idempotent() {
    fn make_pair() -> (DomainRegistry, DomainRegistry) {
        let cid = ContentId([9; 32]);
        let alice = PublicKey([10; 32]);
        let bob = PublicKey([11; 32]);
        let mut a = DomainRegistry::new();
        let mut b = DomainRegistry::new();
        let _ = a.register("alpha.pwb", cid, alice);
        let _ = b.register("beta.pwb", cid, bob);
        a.records.insert(
            "contested.pwb".into(),
            DomainRecord {
                domain: "contested.pwb".into(),
                target_content_id: cid,
                owner: alice,
                credits_spent: 0,
                registered_at_epoch: 3,
                signature: None,
                funding: None,
            },
        );
        b.records.insert(
            "contested.pwb".into(),
            DomainRecord {
                domain: "contested.pwb".into(),
                target_content_id: cid,
                owner: bob,
                credits_spent: 0,
                registered_at_epoch: 7,
                signature: None,
                funding: None,
            },
        );
        a.records.insert(
            "alice.pwb".into(),
            funded_record("alice.pwb", cid, alice, 50, 3, [1; 32], 100),
        );
        b.records.insert(
            "bob.pwb".into(),
            funded_record("bob.pwb", cid, bob, 80, 3, [2; 32], 100),
        );
        a.record_pricing_epoch(stats(1, 40, 1_000, 1));
        b.record_pricing_epoch(stats(1, 0, 100_000, 100));
        (a, b)
    }

    let (a, b) = make_pair();

    // Commutative: A merged with B yields the same state as B with A.
    let mut a1 = a.clone();
    let b1 = b.clone();
    a1.merge(&b1);
    let a2 = a.clone();
    let mut b2 = b.clone();
    b2.merge(&a2);
    assert_eq!(a1, b2);

    // Idempotent: merging with oneself is a no-op.
    let mut a3 = a1.clone();
    a3.merge(&a1);
    assert_eq!(a3, a1);
}

#[test]
fn test_claim_dedup_bounds_memory_and_drops_duplicates() {
    use super::service::{ClaimDedup, MAX_SEEN_CLAIMS};

    let mut dedup = ClaimDedup::new(4);
    let digests: Vec<ContentId> = (0..4u8).map(|i| ContentId([i; 32])).collect();
    for digest in &digests {
        assert!(dedup.insert(*digest));
    }
    // Duplicate is reported without growing the set.
    assert!(!dedup.insert(digests[0]));
    assert_eq!(dedup.len(), 4);
    // Over-capacity evicts the oldest digest, but recent ones still dedup.
    assert!(dedup.insert(ContentId([9; 32])));
    assert_eq!(dedup.len(), 4);
    assert!(!dedup.contains(&digests[0]));
    assert!(dedup.contains(&digests[3]));
    assert!(!dedup.insert(digests[3]));
    const { assert!(MAX_SEEN_CLAIMS >= 1_000) };
}
