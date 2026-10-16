//! Multi-node integration tests for §5.2 Gossip Partition & Network Merge.
//!
//! Simulates several independent nodes that earn attested credits, claim
//! domains, evolve in isolation (network partition), then gossip-merge their
//! registries and pricing engines. Verifies CRDT reconciliation: every domain
//! survives, conflicts resolve deterministically, double-spends are blocked,
//! and the base cost converges.

use pwb_core::crypto::{ContentId, Keypair, PublicKey};
use pwb_node::blind::{
    withdraw_attested, BlindBalanceIssuer, BlindClaim, BlindClaimError, IssuerBinding,
};
use pwb_node::domain::{DomainRegistry, PricingEpochStats, RollingPricing};
use pwb_node::ledger::{AttestorLedger, BalanceAttestation, QuorumCertificate};

/// A mini node: its identity keypair, the attestor ledger it runs (which
/// attests *other* nodes' balances — a ledger never attests itself), and the
/// domain registry it maintains.
struct Node {
    identity: Keypair,
    ledger: AttestorLedger,
    registry: DomainRegistry,
}

impl Node {
    fn new() -> Self {
        let identity = Keypair::generate();
        let ledger = AttestorLedger::new(identity.public);
        let registry = DomainRegistry::new();
        Self {
            identity,
            ledger,
            registry,
        }
    }
}

/// Build a $k$-of-$N$ quorum certificate attesting `subject`'s balance, signed
/// by every member of `signers` (none of which may equal `subject`).
fn attest_quorum(
    subject: PublicKey,
    epoch: u64,
    balance: u64,
    head: [u8; 32],
    signers: &[Keypair],
) -> QuorumCertificate {
    let attestations: Vec<_> = signers
        .iter()
        .map(|k| BalanceAttestation::sign(k, subject, epoch, balance, head))
        .collect();
    QuorumCertificate::from_attestations(attestations).unwrap()
}

#[test]
fn test_multi_node_domain_claim_and_partition_merge() {
    let mut nodes: Vec<Node> = (0..3).map(|_| Node::new()).collect();
    let k = 2;

    // Phase 1: every node earns 200 attested credits, attested by the other
    // two nodes, and claims one domain.
    for i in 0..3 {
        let subject = nodes[i].identity.public;
        let others: Vec<&mut AttestorLedger> = nodes
            .iter_mut()
            .enumerate()
            .filter_map(|(j, n)| if j != i { Some(&mut n.ledger) } else { None })
            .collect();
        for ledger in others {
            ledger
                .record_earned(subject, 1, 200, [i as u8; 32])
                .unwrap();
        }

        let signers: Vec<Keypair> = nodes
            .iter()
            .enumerate()
            .filter_map(|(j, n)| {
                if j != i {
                    Some(n.identity.clone())
                } else {
                    None
                }
            })
            .collect();
        let cert = attest_quorum(subject, 1, 200, [7; 32], &signers);
        let cid = ContentId([i as u8 + 1; 32]);
        let signer_keys: Vec<PublicKey> = signers.iter().map(|k| k.public).collect();
        let domain = format!("node{i}.pwb");
        assert!(
            nodes[i]
                .registry
                .register_attested(&domain, cid, &cert, k, &signer_keys)
                .is_ok(),
            "node {i} claims its first domain"
        );
        assert_eq!(nodes[i].registry.claimed_by(&subject), 50);
    }

    // Phase 2: network partition. Left pair {0, 1}, right singleton {2}.
    // Each node reuses its still-valid certificate to claim a second domain
    // (balance 200 - 50 = 150 still covers the 50-credit price).
    // Precompute the other-two-node signing sets before splitting borrows.
    let others_signers: Vec<Vec<Keypair>> = (0..3)
        .map(|i| {
            nodes
                .iter()
                .enumerate()
                .filter_map(|(j, n)| {
                    if j != i {
                        Some(n.identity.clone())
                    } else {
                        None
                    }
                })
                .collect()
        })
        .collect();
    let others_keys: Vec<Vec<PublicKey>> = others_signers
        .iter()
        .map(|s| s.iter().map(|k| k.public).collect())
        .collect();

    let (left, right) = nodes.split_at_mut(2);
    for (i, node) in left.iter_mut().enumerate() {
        let subject = node.identity.public;
        let cert = attest_quorum(subject, 2, 200, [8; 32], &others_signers[i]);
        let domain = format!("left-{i}.pwb");
        assert!(node
            .registry
            .register_attested(
                &domain,
                ContentId([10 + i as u8; 32]),
                &cert,
                k,
                &others_keys[i]
            )
            .is_ok());
    }
    let subject = right[0].identity.public;
    let cert = attest_quorum(subject, 2, 200, [9; 32], &others_signers[2]);
    assert!(right[0]
        .registry
        .register_attested(
            "right-0.pwb",
            ContentId([20; 32]),
            &cert,
            k,
            &others_keys[2]
        )
        .is_ok());

    // Each side observes disjoint pricing epochs while partitioned.
    let mut left_reg = left[0].registry.clone();
    left_reg.merge(&left[1].registry);
    left_reg.record_pricing_epoch(PricingEpochStats {
        epoch: 1,
        registrations: 12,
        network_work: 1_000,
        node_count: 1,
    }); // hot → price rises
    let mut right_reg = right[0].registry.clone();
    for epoch in 1..=40 {
        right_reg.record_pricing_epoch(PricingEpochStats {
            epoch,
            registrations: 0,
            network_work: 100_000,
            node_count: 100,
        }); // cold → price falls to floor
    }
    let left_price = left_reg.claim_cost();
    let right_price = right_reg.claim_cost();
    assert!(left_price > 50, "hot partition price: {left_price}");
    assert_eq!(right_price, 10, "cold partition price: {right_price}");

    // Phase 3: partition heals — gossip-merge the two registries.
    left_reg.merge(&right_reg);

    // Every domain claimed on either side survives the merge (6 total).
    for domain in [
        "node0.pwb",
        "node1.pwb",
        "node2.pwb",
        "left-0.pwb",
        "left-1.pwb",
        "right-0.pwb",
    ] {
        assert!(
            left_reg.resolve(domain).is_some(),
            "domain {domain} survives partition merge"
        );
    }
    assert_eq!(left_reg.count(), 6);

    // Claimed spend per subject is the max observed on either side (two
    // claims of 50 each).
    for node in &nodes {
        assert_eq!(left_reg.claimed_by(&node.identity.public), 100);
    }

    // Merged base cost takes the maximum observed on either partition.
    assert_eq!(
        left_reg.claim_cost(),
        left_price.max(right_price),
        "merged base cost is the max of both partitions"
    );

    // Commutative: merging in the opposite order yields identical state.
    let mut right_first = right_reg.clone();
    right_first.merge(&left_reg);
    assert_eq!(left_reg, right_first);

    // Idempotent: re-merging the same side is a no-op.
    let snapshot = left_reg.clone();
    left_reg.merge(&right_reg);
    assert_eq!(left_reg, snapshot);
}

#[test]
fn test_multi_node_contested_domain_last_writer_wins() {
    let mut left = DomainRegistry::new();
    let mut right = DomainRegistry::new();
    let cid = ContentId([9; 32]);
    let alice = PublicKey([10; 32]);
    let bob = PublicKey([11; 32]);

    // Both partitions claim the same domain at the same epoch; the merge
    // tie-breaks deterministically by owner public-key bytes.
    assert!(left.register("contested.pwb", cid, alice).is_ok());
    assert!(right.register("contested.pwb", cid, bob).is_ok());

    left.merge(&right);
    let record = left.resolve("contested.pwb").expect("merged record");
    assert_eq!(record.owner, alice, "smaller owner bytes win the tie");

    // Symmetric merge order → same winner (commutative).
    let mut right2 = right.clone();
    let left2 = left.clone();
    right2.merge(&left2);
    assert_eq!(right2.resolve("contested.pwb").unwrap().owner, alice);
}

#[test]
fn test_blind_voucher_double_spend_prevention_across_partition_merge() {
    // Two isolated registries, each backed by its own attestor-issuer. The
    // same subject S earns 100 credits on both sides and withdraws a
    // 50-credit voucher from each. After the clusters gossip-merge, neither
    // voucher can be redeemed a second time.
    let subject = Keypair::generate().public;
    let pseudonym_a = Keypair::generate().public;
    let pseudonym_b = Keypair::generate().public;

    let attestor_a = Keypair::generate();
    let attestor_b = Keypair::generate();

    #[allow(clippy::too_many_arguments)]
    fn withdraw_and_claim(
        issuer_seed: [u8; 32],
        ledger: &mut AttestorLedger,
        registry: &mut DomainRegistry,
        attestor: &Keypair,
        subject: PublicKey,
        pseudonym: PublicKey,
        domain: &str,
        cid: ContentId,
        neighborhood: &[PublicKey],
    ) -> BlindClaim {
        ledger.record_earned(subject, 1, 100, [1; 32]).unwrap();
        let mut issuer = BlindBalanceIssuer::with_defaults(&issuer_seed);
        let receipt =
            withdraw_attested(&mut issuer, ledger, subject, 1, 50, 1).expect("withdraw 50 credits");
        let voucher_key = issuer.public_key(50).expect("50 supported");
        let binding = IssuerBinding::sign(attestor, 1, 50, voucher_key);
        let claim = BlindClaim::build(&receipt.vouchers[0], &binding).expect("claim");
        assert!(registry
            .register_blind(domain, cid, pseudonym, &claim, neighborhood)
            .is_ok());
        claim
    }

    let mut ledger_a = AttestorLedger::new(attestor_a.public);
    let mut registry_a = DomainRegistry::new();
    let neighborhood_a = [attestor_a.public];
    let claim_a = withdraw_and_claim(
        [0xa1; 32],
        &mut ledger_a,
        &mut registry_a,
        &attestor_a,
        subject,
        pseudonym_a,
        "site-a.pwb",
        ContentId([1; 32]),
        &neighborhood_a,
    );

    let mut ledger_b = AttestorLedger::new(attestor_b.public);
    let mut registry_b = DomainRegistry::new();
    let neighborhood_b = [attestor_b.public];
    let claim_b = withdraw_and_claim(
        [0xb2; 32],
        &mut ledger_b,
        &mut registry_b,
        &attestor_b,
        subject,
        pseudonym_b,
        "site-b.pwb",
        ContentId([2; 32]),
        &neighborhood_b,
    );

    // Gossip-merge cluster B into cluster A.
    registry_a.merge(&registry_b);

    // Both tokens are now in the unioned spend log: neither can fund a second claim.
    assert_eq!(
        registry_a.register_blind(
            "other-a.pwb",
            ContentId([3; 32]),
            pseudonym_a,
            &claim_a,
            &neighborhood_a,
        ),
        Err(BlindClaimError::AlreadySpent)
    );
    assert_eq!(
        registry_a.register_blind(
            "other-b.pwb",
            ContentId([4; 32]),
            pseudonym_b,
            &claim_b,
            &neighborhood_b,
        ),
        Err(BlindClaimError::AlreadySpent)
    );
}

#[test]
fn test_pricing_converges_after_network_merge() {
    let mut hot = RollingPricing::new();
    for epoch in 1..=6 {
        hot.record_epoch(PricingEpochStats {
            epoch,
            registrations: 12,
            network_work: 1_000,
            node_count: 1,
        });
    }
    let hot_price = hot.current_price();
    assert!(hot_price > 50);

    let mut cold = RollingPricing::new();
    for epoch in 1..=6 {
        cold.record_epoch(PricingEpochStats {
            epoch,
            registrations: 0,
            network_work: 100_000,
            node_count: 100,
        });
    }
    assert_eq!(cold.current_price(), 10);

    // Merge: the merged base cost never drops below either partition's.
    hot.merge(&cold);
    assert_eq!(hot.current_price(), hot_price);

    // As balanced epochs keep arriving, the price drifts back toward
    // equilibrium from the merged maximum.
    let mut price = hot.current_price();
    for epoch in 7..=40 {
        hot.record_epoch(PricingEpochStats {
            epoch,
            registrations: 4,
            network_work: 10_000,
            node_count: 10,
        });
        let next = hot.current_price();
        assert!(
            next <= price || next == hot_price,
            "price should not creep above merge max"
        );
        price = next;
    }
    assert!(
        price < hot_price,
        "balanced traffic tempers the merged max price"
    );
    assert!(price >= 10, "price stays within bounds");
}
