use super::*;
use crate::domain::{DomainRegistry, PricingEpochStats};
use crate::ledger::AttestorLedger;
use pwb_core::crypto::{ContentId, Keypair, PublicKey, VoucherSecret, VoucherSpendLog};
use pwb_core::system::is_reserved_system_domain;

fn make_issuer() -> BlindBalanceIssuer {
    BlindBalanceIssuer::with_defaults(&[0x42; 32])
}

fn funded_ledger(subject: PublicKey, balance: u64) -> AttestorLedger {
    let attestor = Keypair::generate();
    let mut ledger = AttestorLedger::new(attestor.public);
    if balance > 0 {
        ledger
            .record_earned(subject, 1, balance, [7; 32])
            .expect("fund");
    }
    ledger
}

#[test]
fn test_denomination_keys_are_independent() {
    let mut issuer = make_issuer();
    let ten = issuer.public_key(10).expect("10 supported");
    let fifty = issuer.public_key(50).expect("50 supported");
    assert_ne!(ten.to_bytes(), fifty.to_bytes());
    assert!(issuer.public_key(7).is_none());
    assert_eq!(issuer.denominations(), vec![1, 5, 10, 25, 50]);
    assert!(issuer.commit_nonces(7, 1).is_empty());
}

#[test]
fn test_binding_sign_verify_and_tamper() {
    let attestor = Keypair::generate();
    let key = make_issuer().public_key(50).unwrap();
    let binding = IssuerBinding::sign(&attestor, 3, 50, key);
    assert!(binding.verify());

    let mut tampered = binding.clone();
    tampered.denomination = 500;
    assert!(!tampered.verify());

    let mut forged = binding.clone();
    forged.signature = Keypair::generate().sign(b"other");
    assert!(!forged.verify());
}

#[test]
fn test_withdrawal_converts_attested_balance_to_unlinkable_vouchers() {
    let subject = Keypair::generate().public;
    let mut issuer = make_issuer();
    let mut ledger = funded_ledger(subject, 50);

    let receipt = withdraw_attested(&mut issuer, &mut ledger, subject, 1, 10, 5).expect("withdraw");
    assert_eq!(receipt.total, 50);
    assert_eq!(receipt.vouchers.len(), 5);
    assert_eq!(ledger.balance(&subject), 0, "volume was spent down");

    let key = issuer.public_key(10).unwrap();
    for withdrawn in &receipt.vouchers {
        assert_eq!(withdrawn.voucher.amount, 10);
        assert!(withdrawn.voucher.verify(&key));
        assert!(withdrawn.voucher.verify_owner(&withdrawn.secret));
    }
}

#[test]
fn test_withdrawal_refusals() {
    let subject = Keypair::generate().public;
    let mut issuer = make_issuer();

    // No balance at all.
    let mut empty = funded_ledger(subject, 0);
    assert_eq!(
        withdraw_attested(&mut issuer, &mut empty, subject, 1, 10, 1),
        Err(BlindError::InsufficientBalance {
            balance: 0,
            needed: 10
        })
    );

    // Overdraw: 50 available, withdraw 60.
    let mut ledger = funded_ledger(subject, 50);
    assert_eq!(
        withdraw_attested(&mut issuer, &mut ledger, subject, 1, 10, 6),
        Err(BlindError::InsufficientBalance {
            balance: 50,
            needed: 60
        })
    );

    // Zero chunks and unknown denomination.
    assert_eq!(
        withdraw_attested(&mut issuer, &mut ledger, subject, 1, 10, 0),
        Err(BlindError::InvalidAmount)
    );
    assert_eq!(
        withdraw_attested(&mut issuer, &mut ledger, subject, 1, 7, 1),
        Err(BlindError::UnknownDenomination(7))
    );
    // A denominator too large to multiply safely.
    assert_eq!(
        withdraw_attested(&mut issuer, &mut ledger, subject, 1, u64::MAX, 2),
        Err(BlindError::InvalidAmount)
    );
}

#[test]
fn test_sign_batch_validates_before_consuming_nonces() {
    let mut issuer = make_issuer();
    let key = issuer.public_key(10).unwrap();
    let points = issuer.commit_nonces(10, 2);

    // Request count mismatch.
    let withdrawal = BlindWithdrawal::prepare(10, &key, &points).unwrap();
    assert_eq!(
        issuer.sign_batch(&withdrawal.requests()[..1], 100),
        Err(BlindError::NonceCountMismatch)
    );

    // Over the spendable bound.
    assert_eq!(
        issuer.sign_batch(&withdrawal.requests(), 15),
        Err(BlindError::InsufficientBalance {
            balance: 15,
            needed: 20
        })
    );

    // Amount outside the paired denomination key.
    let wrong = BlindWithdrawal::prepare(50, &issuer.public_key(50).unwrap(), &points).unwrap();
    assert_eq!(
        issuer.sign_batch(&wrong.requests(), 1000),
        Err(BlindError::AmountMismatch)
    );

    // The refused calls did not burn the nonces: the honest batch still signs.
    let requests = withdrawal.requests();
    let partials = issuer.sign_batch(&requests, 100).expect("sign");
    let vouchers = withdrawal.finish(&partials).expect("unblind");
    assert_eq!(vouchers.len(), 2);

    // The queue is drained now: re-submitting the same requests fails.
    assert_eq!(
        issuer.sign_batch(&requests, 100),
        Err(BlindError::NonceCountMismatch)
    );
}

#[test]
fn test_double_withdrawal_blocked_and_evidence_unique() {
    let subject = Keypair::generate().public;
    let mut issuer = make_issuer();
    let mut ledger = funded_ledger(subject, 50);

    let first = withdraw_attested(&mut issuer, &mut ledger, subject, 1, 50, 1).unwrap();
    // Balance is gone: a second withdrawal is refused.
    assert_eq!(
        withdraw_attested(&mut issuer, &mut ledger, subject, 1, 50, 1),
        Err(BlindError::InsufficientBalance {
            balance: 0,
            needed: 50
        })
    );

    // Replaying the same evidence while balance remains is a duplicate
    // (the balance check runs first, so this needs headroom).
    let mut replay_ledger = funded_ledger(subject, 100);
    replay_ledger
        .record_spent(subject, 1, 50, first.evidence)
        .expect("first spend");
    assert_eq!(
        replay_ledger.record_spent(subject, 2, 50, first.evidence),
        Err(crate::ledger::LedgerError::DuplicateEvent)
    );

    // A fresh withdrawal (fresh blinding) has a distinct evidence digest.
    let mut other_issuer = make_issuer();
    let mut other_ledger = funded_ledger(subject, 50);
    let second =
        withdraw_attested(&mut other_issuer, &mut other_ledger, subject, 1, 50, 1).unwrap();
    assert_ne!(first.evidence, second.evidence);
}

#[test]
fn test_cross_attestor_inflation_requires_gossip() {
    // The same 50-credit balance is attested by three peers. Each keeps
    // its own ledger view, so a withdrawal must be gossiped or every
    // view would independently back a fresh conversion.
    let subject = Keypair::generate().public;
    let mut ledgers = [
        funded_ledger(subject, 50),
        funded_ledger(subject, 50),
        funded_ledger(subject, 50),
    ];

    let mut issuer = make_issuer();
    let receipt =
        withdraw_attested(&mut issuer, &mut ledgers[0], subject, 1, 50, 1).expect("withdraw");
    assert_eq!(ledgers[0].balance(&subject), 0);

    // Without gossip, ledger 1 still shows 50 and would fund inflation.
    assert_eq!(ledgers[1].balance(&subject), 50);

    // Gossip the spend event (same evidence) to the sibling views.
    for ledger in &mut ledgers[1..] {
        ledger
            .record_spent(subject, 1, receipt.total, receipt.evidence)
            .expect("gossip spend");
    }
    assert!(ledgers.iter().all(|ledger| ledger.balance(&subject) == 0));

    // Now no attestor will back another withdrawal.
    let mut fresh = make_issuer();
    assert_eq!(
        withdraw_attested(&mut fresh, &mut ledgers[2], subject, 1, 50, 1),
        Err(BlindError::InsufficientBalance {
            balance: 0,
            needed: 50
        })
    );
}

#[test]
fn test_spend_log_merge_blocks_double_redeem_after_partition_merge() {
    // Two isolated clusters each redeem a different voucher for the same
    // subject's attested balance. After the clusters gossip-merge their
    // registries, both tokens are spent everywhere.
    let subject = Keypair::generate().public;
    let attestor = Keypair::generate();
    let neighborhood = [attestor.public];
    let pseudonym = Keypair::generate().public;

    fn claim_voucher(
        subject: PublicKey,
        issuer: &mut BlindBalanceIssuer,
        ledger: &mut AttestorLedger,
        attestor: &Keypair,
        pseudonym: PublicKey,
        neighborhood: [PublicKey; 1],
    ) -> (DomainRegistry, BlindClaim) {
        let receipt = withdraw_attested(issuer, ledger, subject, 1, 50, 1).expect("withdraw");
        let voucher_key = issuer.public_key(50).unwrap();
        let binding = IssuerBinding::sign(attestor, 1, 50, voucher_key);
        let claim = BlindClaim::build(&receipt.vouchers[0], &binding).expect("claim");
        let mut registry = DomainRegistry::new();
        assert!(registry
            .register_blind(
                "site.pwb",
                ContentId([1; 32]),
                pseudonym,
                &claim,
                &neighborhood
            )
            .is_ok());
        (registry, claim)
    }

    let mut issuer_a = make_issuer();
    let mut ledger_a = funded_ledger(subject, 50);
    let (mut registry_a, claim_a) = claim_voucher(
        subject,
        &mut issuer_a,
        &mut ledger_a,
        &attestor,
        pseudonym,
        neighborhood,
    );

    let mut issuer_b = make_issuer();
    let mut ledger_b = funded_ledger(subject, 50);
    let (registry_b, claim_b) = claim_voucher(
        subject,
        &mut issuer_b,
        &mut ledger_b,
        &attestor,
        pseudonym,
        neighborhood,
    );

    // Gossip-merge cluster B into cluster A.
    registry_a.merge(&registry_b);

    // Neither voucher can fund a second claim in the merged registry.
    assert_eq!(
        registry_a.register_blind(
            "other-a.pwb",
            ContentId([2; 32]),
            pseudonym,
            &claim_a,
            &neighborhood,
        ),
        Err(BlindClaimError::AlreadySpent)
    );
    assert_eq!(
        registry_a.register_blind(
            "other-b.pwb",
            ContentId([3; 32]),
            pseudonym,
            &claim_b,
            &neighborhood,
        ),
        Err(BlindClaimError::AlreadySpent)
    );
}

#[test]
fn test_withdrawn_vouchers_are_unlinkable() {
    let subject = Keypair::generate().public;
    let mut issuer = make_issuer();
    let first = withdraw_attested(
        &mut issuer,
        &mut funded_ledger(subject, 50),
        subject,
        1,
        50,
        1,
    )
    .unwrap();
    let second = withdraw_attested(
        &mut issuer,
        &mut funded_ledger(subject, 50),
        subject,
        1,
        50,
        1,
    )
    .unwrap();

    let a = &first.vouchers[0].voucher;
    let b = &second.vouchers[0].voucher;
    assert!(a.verify(&issuer.public_key(50).unwrap()));
    assert!(b.verify(&issuer.public_key(50).unwrap()));
    // Same amount, same issuer — but fresh blinding hides the linkage.
    assert_ne!(a.token, b.token);
    assert_ne!(a.challenge, b.challenge);
    assert_ne!(a.response, b.response);
}

#[test]
fn test_prepare_rejects_invalid_points_and_finish_rejects_count_mismatch() {
    let mut issuer = make_issuer();
    let key = issuer.public_key(10).unwrap();
    assert_eq!(
        BlindWithdrawal::prepare(10, &key, &[[0xff; 32]]).unwrap_err(),
        BlindError::NonceCountMismatch
    );
    let points = issuer.commit_nonces(10, 2);
    let withdrawal = BlindWithdrawal::prepare(10, &key, &points).unwrap();
    assert_eq!(withdrawal.total(), 20);
    assert_eq!(
        withdrawal.finish(&[]).unwrap_err(),
        BlindError::NonceCountMismatch
    );
}

#[test]
fn test_claim_build_and_registry_redemption() {
    let subject = Keypair::generate().public;
    let attestor = Keypair::generate();
    let mut issuer = make_issuer();
    let mut ledger = funded_ledger(subject, 50);
    let receipt = withdraw_attested(&mut issuer, &mut ledger, subject, 1, 50, 1).expect("withdraw");

    let binding = IssuerBinding::sign(&attestor, 1, 50, issuer.public_key(50).unwrap());
    let claim = BlindClaim::build(&receipt.vouchers[0], &binding).expect("claim");
    // A wrong secret cannot produce a claim at all.
    let wrong = WithdrawnVoucher {
        voucher: receipt.vouchers[0].voucher.clone(),
        secret: VoucherSecret::random(),
    };
    assert!(BlindClaim::build(&wrong, &binding).is_none());

    let mut registry = DomainRegistry::new();
    // The owner is a fresh pseudonym — not the earning identity.
    let pseudonym = Keypair::generate().public;
    let neighborhood = [attestor.public];
    assert!(is_reserved_system_domain("private.plain"));
    assert_eq!(
        registry.register_blind(
            "private.pwb",
            ContentId([1; 32]),
            pseudonym,
            &claim,
            &neighborhood,
        ),
        Ok(())
    );
    let record = registry.resolve("private.pwb").expect("registered");
    assert_eq!(record.owner, pseudonym);
    assert_ne!(record.owner, subject, "claim is unlinkable to the earner");
    assert_eq!(record.registered_at_epoch, 1);

    // Reserved and duplicate rules still hold on the private path.
    assert_eq!(
        registry.register_blind(
            "search.plain",
            ContentId([1; 32]),
            pseudonym,
            &claim,
            &neighborhood,
        ),
        Err(BlindClaimError::ReservedSystemDomain)
    );
    // So does the unofficial-namespace rule: names must carry a listed TLD.
    assert_eq!(
        registry.register_blind(
            "private.xyz",
            ContentId([1; 32]),
            pseudonym,
            &claim,
            &neighborhood,
        ),
        Err(BlindClaimError::UnknownTld)
    );
    assert_eq!(
        registry.register_blind(
            "private.pwb",
            ContentId([1; 32]),
            pseudonym,
            &claim,
            &neighborhood,
        ),
        Err(BlindClaimError::DomainAlreadyRegistered)
    );
}

#[test]
fn test_registry_rejects_bad_bindings_vouchers_and_replays() {
    let subject = Keypair::generate().public;
    let attestor = Keypair::generate();
    let mut issuer = make_issuer();
    let receipt = withdraw_attested(
        &mut issuer,
        &mut funded_ledger(subject, 50),
        subject,
        1,
        50,
        1,
    )
    .unwrap();
    let voucher = &receipt.vouchers[0];
    let binding = IssuerBinding::sign(&attestor, 1, 50, issuer.public_key(50).unwrap());
    let pseudonym = Keypair::generate().public;
    let neighborhood = [attestor.public];
    let cid = ContentId([9; 32]);

    let mut registry = DomainRegistry::new();

    // Tampered binding.
    let mut bad_binding = binding.clone();
    bad_binding.epoch = 9;
    let claim = BlindClaim {
        voucher: voucher.voucher.clone(),
        proof: voucher.voucher.prove_ownership(&voucher.secret).unwrap(),
        binding: bad_binding,
    };
    assert_eq!(
        registry.register_blind("a.pwb", cid, pseudonym, &claim, &neighborhood),
        Err(BlindClaimError::BadBinding)
    );

    // Binding attestor outside the neighborhood.
    let stranger_binding =
        IssuerBinding::sign(&Keypair::generate(), 1, 50, issuer.public_key(50).unwrap());
    let claim = BlindClaim {
        voucher: voucher.voucher.clone(),
        proof: voucher.voucher.prove_ownership(&voucher.secret).unwrap(),
        binding: stranger_binding,
    };
    assert_eq!(
        registry.register_blind("b.pwb", cid, pseudonym, &claim, &neighborhood),
        Err(BlindClaimError::IssuerNotAttestor)
    );

    // Voucher that does not verify against the bound key (bind the 10 key
    // to a 50 voucher).
    let wrong_key_binding = IssuerBinding::sign(&attestor, 1, 50, issuer.public_key(10).unwrap());
    let claim = BlindClaim {
        voucher: voucher.voucher.clone(),
        proof: voucher.voucher.prove_ownership(&voucher.secret).unwrap(),
        binding: wrong_key_binding,
    };
    assert_eq!(
        registry.register_blind("c.pwb", cid, pseudonym, &claim, &neighborhood),
        Err(BlindClaimError::BadVoucher)
    );

    // Valid voucher, but a hot market has pushed the rolling claim
    // price above the voucher's denomination.
    let claim = BlindClaim::build(voucher, &binding).unwrap();
    for epoch in 1..=4 {
        registry.record_pricing_epoch(PricingEpochStats {
            epoch,
            registrations: 40,
            network_work: 1_000,
            node_count: 1,
        });
    }
    let hot_price = registry.claim_cost();
    assert!(hot_price > 50, "price rose with demand");
    assert_eq!(
        registry.register_blind("d.pwb", cid, pseudonym, &claim, &neighborhood),
        Err(BlindClaimError::Underfunded {
            voucher: 50,
            cost: hot_price
        })
    );

    // Forged ownership proof (at the still-elevated price).
    let mut forged = claim.clone();
    forged.proof.response[0] ^= 0xff;
    assert_eq!(
        registry.register_blind("e.pwb", cid, pseudonym, &forged, &neighborhood),
        Err(BlindClaimError::NoOwnershipProof)
    );

    // The hot-market price now exceeds the voucher: even an honest claim
    // is underfunded until the market cools or a bigger voucher arrives.
    assert_eq!(
        registry.register_blind("f.pwb", cid, pseudonym, &claim, &neighborhood),
        Err(BlindClaimError::Underfunded {
            voucher: 50,
            cost: hot_price
        })
    );

    // Cool the market back below the voucher denomination, then the
    // same claim succeeds and the token cannot be replayed.
    for epoch in 5..=40 {
        registry.record_pricing_epoch(PricingEpochStats {
            epoch,
            registrations: 0,
            network_work: 100_000,
            node_count: 100,
        });
    }
    assert_eq!(registry.claim_cost(), crate::domain::MIN_CLAIM_PRICE);
    assert!(registry.claim_cost() <= 50);
    assert_eq!(
        registry.register_blind("f.pwb", cid, pseudonym, &claim, &neighborhood),
        Ok(())
    );
    assert_eq!(
        registry.register_blind("g.pwb", cid, pseudonym, &claim, &neighborhood),
        Err(BlindClaimError::AlreadySpent)
    );
}

#[test]
fn test_spend_log_rejects_replay_of_redeemed_voucher() {
    // Defense in depth: even outside the registry, a redeemed token is
    // marked spent.
    let subject = Keypair::generate().public;
    let mut issuer = make_issuer();
    let receipt = withdraw_attested(
        &mut issuer,
        &mut funded_ledger(subject, 50),
        subject,
        1,
        50,
        1,
    )
    .unwrap();
    let voucher = &receipt.vouchers[0].voucher;
    let key = issuer.public_key(50).unwrap();
    let mut log = VoucherSpendLog::new();
    assert_eq!(log.redeem(voucher, &key).unwrap(), 50);
    assert_eq!(
        log.redeem(voucher, &key),
        Err(pwb_core::crypto::VoucherSpendError::AlreadySpent)
    );
}

#[test]
fn test_blind_errors_implement_display_and_error() {
    let errors = [
        BlindError::InvalidAmount,
        BlindError::UnknownDenomination(3),
        BlindError::InsufficientBalance {
            balance: 1,
            needed: 2,
        },
        BlindError::NonceCountMismatch,
        BlindError::AmountMismatch,
        BlindError::BadVoucher,
        BlindError::NoOwnershipProof,
        BlindError::LedgerRejected("duplicate event".into()),
    ];
    for err in errors {
        assert!(!err.to_string().is_empty());
        let _dyn: &dyn std::error::Error = &err;
    }
    let claim_errors = [
        BlindClaimError::ReservedSystemDomain,
        BlindClaimError::DomainAlreadyRegistered,
        BlindClaimError::BadBinding,
        BlindClaimError::IssuerNotAttestor,
        BlindClaimError::BadVoucher,
        BlindClaimError::NoOwnershipProof,
        BlindClaimError::Underfunded {
            voucher: 1,
            cost: 2,
        },
        BlindClaimError::AlreadySpent,
    ];
    for err in claim_errors {
        assert!(!err.to_string().is_empty());
        let _dyn: &dyn std::error::Error = &err;
    }
}
