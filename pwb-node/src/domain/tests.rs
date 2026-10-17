use super::*;
use crate::ledger::LedgerError;
use pwb_core::crypto::{ContentId, PublicKey};

#[test]
fn test_domain_registry_security_rules() {
    let mut reg = DomainRegistry::new();
    let cid = ContentId([1; 32]);
    let pk = PublicKey([2; 32]);

    // Attempting to claim .plain domain MUST fail!
    assert!(reg.register("search.plain", cid, pk).is_err());
    assert!(reg.register("malicious.plain", cid, pk).is_err());
    assert!(reg.register("hacked.plain", cid, pk).is_err());

    // Registering user domain .pwb MUST succeed!
    assert!(reg.register("welcome.pwb", cid, pk).is_ok());
    assert!(reg.register("welcome.pwb", cid, pk).is_err()); // duplicate check

    let resolved = reg.resolve("WELCOME.PWB").unwrap();
    assert_eq!(resolved.target_content_id, cid);
}

fn attest_quorum(
    subject: PublicKey,
    epoch: u64,
    balance: u64,
    head: [u8; 32],
    keys: &[pwb_core::crypto::Keypair],
) -> (crate::ledger::QuorumCertificate, Vec<PublicKey>) {
    let attestations = keys
        .iter()
        .map(|key| crate::ledger::BalanceAttestation::sign(key, subject, epoch, balance, head))
        .collect();
    let neighborhood: Vec<PublicKey> = keys.iter().map(|k| k.public).collect();
    (
        crate::ledger::QuorumCertificate::from_attestations(attestations).expect("quorum"),
        neighborhood,
    )
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
        reg.register_attested("welcome.pwb", cid, &cert, 2, &neighborhood),
        Ok(())
    );
    assert_eq!(reg.claimed_by(&subject), INITIAL_CLAIM_PRICE);
    let record = reg.resolve("welcome.pwb").expect("registered");
    assert_eq!(record.owner, subject);
    assert_eq!(record.registered_at_epoch, 4);

    assert_eq!(
        reg.register_attested("welcome.pwb", cid, &cert, 2, &neighborhood),
        Err(LedgerError::DomainAlreadyRegistered)
    );

    // Replaying the same certificate cannot overdraw: 100 - 50 spent
    // covers a second 50-credit claim, but not a third.
    assert_eq!(
        reg.register_attested("second.pwb", cid, &cert, 2, &neighborhood),
        Ok(())
    );
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
            registered_at_epoch: 3,
        },
    );
    b.records.insert(
        "contested.pwb".into(),
        DomainRecord {
            domain: "contested.pwb".into(),
            target_content_id: cid,
            owner: bob,
            registered_at_epoch: 7,
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
            registered_at_epoch: 5,
        },
    );
    b.records.insert(
        "tie.pwb".into(),
        DomainRecord {
            domain: "tie.pwb".into(),
            target_content_id: cid,
            owner: low,
            registered_at_epoch: 5,
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
            registered_at_epoch: 5,
        },
    );
    d.records.insert(
        "tie.pwb".into(),
        DomainRecord {
            domain: "tie.pwb".into(),
            target_content_id: cid,
            owner: high,
            registered_at_epoch: 5,
        },
    );
    d.merge(&c);
    assert_eq!(d.resolve("tie.pwb").unwrap().owner, low);
}

#[test]
fn test_domain_registry_merge_claimed_takes_max_and_is_idempotent() {
    let owner = PublicKey([7; 32]);
    let mut a = DomainRegistry::new();
    let mut b = DomainRegistry::new();
    a.claimed.insert(owner, 50);
    b.claimed.insert(owner, 80);
    a.merge(&b);
    assert_eq!(a.claimed_by(&owner), 80);

    // Re-merging the same side is a no-op.
    let snapshot = a.clone();
    a.merge(&b);
    assert_eq!(a, snapshot);
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
                registered_at_epoch: 3,
            },
        );
        b.records.insert(
            "contested.pwb".into(),
            DomainRecord {
                domain: "contested.pwb".into(),
                target_content_id: cid,
                owner: bob,
                registered_at_epoch: 7,
            },
        );
        a.claimed.insert(alice, 50);
        b.claimed.insert(bob, 80);
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
