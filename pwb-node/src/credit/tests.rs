use std::collections::HashMap;
use std::time::{Duration, Instant};

use pwb_core::crypto::{ContentId, Keypair};
use pwb_core::spec::{PlainNode, Tag};
use pwb_core::wire::Bundle;

use super::*;

fn test_bundle(text: &str) -> Bundle {
    let mut root = PlainNode::new(0, Tag::View);
    let mut title = PlainNode::new(0, Tag::Title);
    title.text_content = Some("PlainWeb Demo".to_string());
    let mut body = PlainNode::new(0, Tag::Text);
    body.text_content = Some(text.to_string());
    root.children.push(title);
    root.children.push(body);
    Bundle {
        version: pwb_core::wire::CURRENT_VERSION,
        title: "PlainWeb Demo".to_string(),
        author_pubkey: None,
        author_signature: None,
        root_node: root,
        script_payload: None,
        components: HashMap::new(),
        modules: HashMap::new(),
    }
}

fn minter_with(owner: &Keypair, policy: MintPolicy) -> CreditMinter {
    CreditMinter::new(owner.public, policy)
}

fn no_cooldown_policy() -> MintPolicy {
    MintPolicy {
        per_service_cooldown: Duration::ZERO,
        ..MintPolicy::default()
    }
}

#[test]
fn test_query_mint_happy_path_and_replay_rejection() {
    let server = Keypair::generate();
    let mut minter = minter_with(&server, no_cooldown_policy());
    let t0 = Instant::now();

    let challenge = [7u8; 32];
    minter.record_challenge(challenge, t0);
    minter.record_traffic(8 * 1024);

    let results = vec![
        ContentId::compute(b"bundle-a"),
        ContentId::compute(b"bundle-b"),
    ];
    let receipt = QueryReceipt::sign(&server, "plainweb", results, challenge);
    assert_eq!(
        minter.mint_query(&receipt, t0),
        Ok(MintPolicy::default().query_credits)
    );
    assert_eq!(minter.total_earned(), 5);
    // Traffic was consumed at the policy's floor rate.
    let floor = minter.policy().traffic_per_credit;
    assert_eq!(minter.available_traffic(), 8 * 1024 - 5 * floor);

    // The challenge was claimed: the same receipt cannot mint twice.
    assert_eq!(
        minter.mint_query(&receipt, t0),
        Err(MintError::UnknownChallenge)
    );

    // A retried query re-sends the same signed challenge. Re-recording it
    // must not reopen the mint window: the responder may answer again, but it
    // cannot mint a second time from one requester round-trip.
    minter.record_challenge(challenge, t0 + Duration::from_secs(1));
    assert_eq!(
        minter.mint_query(&receipt, t0 + Duration::from_secs(1)),
        Err(MintError::UnknownChallenge)
    );
}

#[test]
fn test_query_receipt_rejects_bad_signature() {
    let server = Keypair::generate();
    let mut minter = minter_with(&server, no_cooldown_policy());
    let t0 = Instant::now();
    let challenge = [9u8; 32];
    minter.record_challenge(challenge, t0);
    minter.record_traffic(10 * 1024);

    let mut receipt = QueryReceipt::sign(&server, "plainweb", vec![], challenge);
    receipt.query = "plainweb2".to_string(); // tampered after signing
    assert_eq!(
        minter.mint_query(&receipt, t0),
        Err(MintError::BadSignature)
    );

    // A receipt signed by a different key cannot mint this minter's
    // credits — the owner identity is bound at construction.
    let impostor = Keypair::generate();
    let forged = QueryReceipt::sign(&impostor, "plainweb", vec![], challenge);
    assert_eq!(minter.mint_query(&forged, t0), Err(MintError::WrongSigner));
}

#[test]
fn test_challenge_unknown_and_expired() {
    let server = Keypair::generate();
    let mut minter = minter_with(
        &server,
        MintPolicy {
            challenge_ttl: Duration::from_secs(60),
            ..no_cooldown_policy()
        },
    );
    let t0 = Instant::now();
    minter.record_traffic(10 * 1024);

    // Never issued.
    let unknown = QueryReceipt::sign(&server, "q", vec![], [1u8; 32]);
    assert_eq!(
        minter.mint_query(&unknown, t0),
        Err(MintError::UnknownChallenge)
    );

    // Issued but expired.
    let challenge = [2u8; 32];
    minter.record_challenge(challenge, t0);
    let expired = QueryReceipt::sign(&server, "q", vec![], challenge);
    assert_eq!(
        minter.mint_query(&expired, t0 + Duration::from_secs(61)),
        Err(MintError::ChallengeExpired)
    );

    // Re-issuing refreshes the same nonce.
    minter.record_challenge(challenge, t0 + Duration::from_secs(100));
    assert_eq!(
        minter.mint_query(&expired, t0 + Duration::from_secs(101)),
        Ok(5)
    );
}

#[test]
fn test_index_mint_verifies_content_and_terms() {
    let indexer = Keypair::generate();
    let mut minter = minter_with(&indexer, no_cooldown_policy());
    let t0 = Instant::now();

    let bytes = test_bundle("serving search indexing relaying").serialize();
    let receipt = IndexReceipt::sign(&indexer, &bytes).expect("receipt");
    assert_eq!(receipt.content_id, ContentId::compute(&bytes));

    // Honest path: record the verified download exchange, then mint.
    minter.record_traffic(bytes.len() as u64 + 16 * 1024);
    assert_eq!(
        minter.mint_index(&receipt, &bytes, t0),
        Ok(MintPolicy::default().index_credits)
    );
    assert_eq!(minter.total_earned(), 10);
}

#[test]
fn test_index_rejects_content_mismatch() {
    let indexer = Keypair::generate();
    let mut minter = minter_with(&indexer, no_cooldown_policy());
    let t0 = Instant::now();
    minter.record_traffic(16 * 1024);

    let a = test_bundle("alpha").serialize();
    let b = test_bundle("beta").serialize();
    let receipt = IndexReceipt::sign(&indexer, &a).expect("receipt");
    assert_eq!(
        minter.mint_index(&receipt, &b, t0),
        Err(MintError::ContentMismatch)
    );
}

#[test]
fn test_index_rejects_term_mismatch() {
    let indexer = Keypair::generate();
    let mut minter = minter_with(&indexer, no_cooldown_policy());
    let t0 = Instant::now();
    minter.record_traffic(16 * 1024);

    let a = test_bundle("alpha terms here").serialize();
    let other = test_bundle("completely different words");
    // A well-formed signature over a content id of A but the term hash
    // of a different bundle: signature and CID pass, terms must not.
    let message = index_sign_message(
        indexer.public,
        ContentId::compute(&a),
        canonical_term_hash(&other),
    );
    let receipt = IndexReceipt {
        indexer: indexer.public,
        content_id: ContentId::compute(&a),
        term_hash: canonical_term_hash(&other),
        signature: indexer.sign(&message),
    };
    assert_eq!(
        minter.mint_index(&receipt, &a, t0),
        Err(MintError::TermMismatch)
    );
}

#[test]
fn test_index_rejects_unreadable_bundle() {
    let indexer = Keypair::generate();
    let mut minter = minter_with(&indexer, no_cooldown_policy());
    let t0 = Instant::now();
    minter.record_traffic(16 * 1024);

    let garbage = b"not a bundle at all";
    let message = index_sign_message(
        indexer.public,
        ContentId::compute(garbage),
        ContentId::compute(b"terms"),
    );
    let receipt = IndexReceipt {
        indexer: indexer.public,
        content_id: ContentId::compute(garbage),
        term_hash: ContentId::compute(b"terms"),
        signature: indexer.sign(&message),
    };
    assert_eq!(
        minter.mint_index(&receipt, garbage, t0),
        Err(MintError::BundleUnreadable)
    );
}

#[test]
fn test_relay_mint_happy_path_and_replay_rejection() {
    let relayer = Keypair::generate();
    let mut minter = minter_with(&relayer, no_cooldown_policy());
    let t0 = Instant::now();

    // Three 1000-byte cells: 3000 bytes → floor 2 KiB × 1 credit = 2.
    for i in 0..3u8 {
        minter.relay_cell(&[i; 1000]);
    }
    assert_eq!(minter.traffic().total(), 3000);

    let receipt = minter.seal_relay_window(&relayer).expect("sealed");
    assert_eq!(receipt.window, 0);
    assert_eq!(receipt.cells, 3);
    assert_eq!(receipt.bytes, 3000);

    assert_eq!(minter.mint_relay(&receipt, t0), Ok(2));
    assert_eq!(minter.total_earned(), 2);
    // Claim-once: the same receipt cannot mint again.
    assert_eq!(
        minter.mint_relay(&receipt, t0),
        Err(MintError::NoSealedRelayWork)
    );
}

#[test]
fn test_relay_chain_continuity_across_windows() {
    let relayer = Keypair::generate();
    let mut minter = minter_with(&relayer, no_cooldown_policy());
    let t0 = Instant::now();

    minter.relay_cell(&[1u8; 2048]);
    let first = minter.seal_relay_window(&relayer).expect("first");
    assert_eq!(minter.mint_relay(&first, t0), Ok(2));

    // The live chain continues into the next window.
    minter.relay_cell(&[2u8; 2048]);
    let second = minter.seal_relay_window(&relayer).expect("second");
    assert_eq!(
        minter.mint_relay(&second, t0 + Duration::from_secs(1)),
        Ok(2)
    );

    assert_eq!(first.window, 0);
    assert_eq!(second.window, 1);
    assert_ne!(first.chain_head, second.chain_head, "chain advances");
}

#[test]
fn test_relay_rejects_tampered_receipt_then_accepts_original() {
    let relayer = Keypair::generate();
    let mut minter = minter_with(&relayer, no_cooldown_policy());
    let t0 = Instant::now();

    minter.relay_cell(&[9u8; 1024]);
    let receipt = minter.seal_relay_window(&relayer).expect("sealed");

    // Post-signature tampering breaks the signature itself.
    let mut tampered = receipt.clone();
    tampered.cells += 1;
    assert_eq!(
        minter.mint_relay(&tampered, t0),
        Err(MintError::BadSignature)
    );

    // A well-signed receipt for a window the minter never sealed is a
    // chain break — the sealed window survives to pay the real claim.
    let wrong_window = RelayReceipt::sign(
        &relayer,
        99,
        receipt.cells,
        receipt.bytes,
        receipt.chain_head,
    );
    assert_eq!(
        minter.mint_relay(&wrong_window, t0),
        Err(MintError::RelayChainBroken)
    );
    assert_eq!(minter.mint_relay(&receipt, t0), Ok(1));
}

#[test]
fn test_relay_seal_refuses_empty_and_double_seal() {
    let relayer = Keypair::generate();
    let mut minter = minter_with(&relayer, no_cooldown_policy());

    // No cells → nothing to seal.
    assert!(minter.seal_relay_window(&relayer).is_none());

    minter.relay_cell(&[1u8; 100]);
    assert!(minter.seal_relay_window(&relayer).is_some());
    // Previous seal is still unclaimed → refuse to overwrite it.
    minter.relay_cell(&[2u8; 100]);
    assert!(minter.seal_relay_window(&relayer).is_none());
}

#[test]
fn test_cooldown_spreads_mints_over_time() {
    let server = Keypair::generate();
    let mut minter = minter_with(
        &server,
        MintPolicy {
            per_service_cooldown: Duration::from_secs(5),
            ..MintPolicy::default()
        },
    );
    let t0 = Instant::now();
    minter.record_traffic(100 * 1024);

    let c1 = [1u8; 32];
    minter.record_challenge(c1, t0);
    let r1 = QueryReceipt::sign(&server, "q1", vec![], c1);
    assert_eq!(minter.mint_query(&r1, t0), Ok(5));

    // Same kind inside the cooldown → refused with the remaining wait.
    let c2 = [2u8; 32];
    minter.record_challenge(c2, t0);
    let r2 = QueryReceipt::sign(&server, "q2", vec![], c2);
    assert_eq!(
        minter.mint_query(&r2, t0 + Duration::from_secs(1)),
        Err(MintError::Cooldown {
            retry_after: Duration::from_secs(4)
        })
    );

    // Once the cooldown elapses the second query pays.
    assert_eq!(minter.mint_query(&r2, t0 + Duration::from_secs(5)), Ok(5));

    // A different service kind is not gated by the query cooldown.
    let bytes = test_bundle("cooldown isolation").serialize();
    let receipt = IndexReceipt::sign(&server, &bytes).expect("receipt");
    assert_eq!(
        minter.mint_index(&receipt, &bytes, t0 + Duration::from_secs(1)),
        Ok(10)
    );
}

#[test]
fn test_traffic_floor_rate_limits_mints() {
    let server = Keypair::generate();
    let mut minter = minter_with(
        &server,
        MintPolicy {
            traffic_per_credit: 100,
            ..no_cooldown_policy()
        },
    );
    let t0 = Instant::now();

    // 300 bytes < 5 credits × 100 bytes needed.
    minter.record_traffic(300);
    let c1 = [1u8; 32];
    minter.record_challenge(c1, t0);
    let r1 = QueryReceipt::sign(&server, "q1", vec![], c1);
    assert_eq!(
        minter.mint_query(&r1, t0),
        Err(MintError::RateLimited {
            needed_bytes: 500,
            available_bytes: 300
        })
    );

    // With enough traffic the same receipt mints; the floor is consumed.
    minter.record_traffic(300); // total 600
    assert_eq!(minter.mint_query(&r1, t0), Ok(5));
    assert_eq!(minter.available_traffic(), 100);

    // The next mint needs fresh traffic: issuance tracks network flow.
    let c2 = [2u8; 32];
    minter.record_challenge(c2, t0);
    let r2 = QueryReceipt::sign(&server, "q2", vec![], c2);
    assert_eq!(
        minter.mint_query(&r2, t0),
        Err(MintError::RateLimited {
            needed_bytes: 500,
            available_bytes: 100
        })
    );
}

#[test]
fn test_epoch_cap_and_rollover() {
    let server = Keypair::generate();
    let mut minter = minter_with(
        &server,
        MintPolicy {
            epoch_cap: 8,
            epoch: Duration::from_secs(60),
            ..no_cooldown_policy()
        },
    );
    let t0 = Instant::now();
    minter.record_traffic(1024 * 1024);

    let c1 = [1u8; 32];
    minter.record_challenge(c1, t0);
    let r1 = QueryReceipt::sign(&server, "q1", vec![], c1);
    assert_eq!(minter.mint_query(&r1, t0), Ok(5)); // 5/8 used

    let c2 = [2u8; 32];
    minter.record_challenge(c2, t0);
    let r2 = QueryReceipt::sign(&server, "q2", vec![], c2);
    assert_eq!(
        minter.mint_query(&r2, t0),
        Err(MintError::EpochCapReached) // 5+5 > 8
    );

    // Rollover opens a fresh epoch window.
    let t1 = t0 + Duration::from_secs(61);
    minter.record_challenge(c2, t1);
    assert_eq!(minter.mint_query(&r2, t1), Ok(5));
}

#[test]
fn test_anti_asic_traffic_invariant_across_services() {
    // After any sequence of mints, credits × traffic floor never exceeds
    // the verified traffic actually recorded — the core §4.1 property.
    let node = Keypair::generate();
    let mut minter = minter_with(
        &node,
        MintPolicy {
            traffic_per_credit: 64,
            ..no_cooldown_policy()
        },
    );
    let t0 = Instant::now();

    minter.record_traffic(4096);
    let c1 = [1u8; 32];
    minter.record_challenge(c1, t0);
    let query = QueryReceipt::sign(&node, "q", vec![], c1);
    minter.mint_query(&query, t0).expect("query pays");

    let bytes = test_bundle("invariant check").serialize();
    let index = IndexReceipt::sign(&node, &bytes).expect("receipt");
    minter.mint_index(&index, &bytes, t0).expect("index pays");

    minter.relay_cell(&[3u8; 512]);
    let relay = minter.seal_relay_window(&node).expect("sealed");
    minter.mint_relay(&relay, t0).expect("relay pays");

    let floor = minter.policy().traffic_per_credit;
    assert!(
        minter.total_earned() * floor <= minter.traffic().total(),
        "earned {} credits against {} bytes (floor {floor}/credit)",
        minter.total_earned(),
        minter.traffic().total()
    );
}

#[test]
fn test_receipt_verify_and_evidence_digest() {
    let server = Keypair::generate();
    let query = QueryReceipt::sign(&server, "plainweb", vec![], [3u8; 32]);
    assert!(query.verify());
    assert_ne!(query.evidence_digest(), [0u8; 32]);
    let mut tampered = query.clone();
    tampered.query = "plainweb2".into();
    assert!(!tampered.verify());
    assert_ne!(tampered.evidence_digest(), query.evidence_digest());

    let relayer = Keypair::generate();
    let relay = RelayReceipt::sign(&relayer, 0, 3, 3000, [9u8; 32]);
    assert!(relay.verify());
    let mut tampered = relay;
    tampered.bytes += 1;
    assert!(!tampered.verify());
}

#[test]
fn test_rendezvous_mint_pays_joins_and_peer_table_bytes() {
    let seed = Keypair::generate();
    let mut minter = minter_with(&seed, no_cooldown_policy());
    let t0 = Instant::now();

    // One newcomer and 2 KiB of peer table handed out, counted once per copy.
    minter.rendezvous_join(600);
    minter.record_peer_table_served(4, 2048);
    assert_eq!(minter.pending_rendezvous_joins(), 1);
    assert_eq!(minter.pending_rendezvous_bytes(), 2048);

    let receipt = minter.seal_rendezvous_window(&seed).expect("sealed");
    assert_eq!(receipt.seed, seed.public);
    assert!(receipt.verify());
    assert_eq!(
        minter.mint_rendezvous(&receipt, t0),
        Ok(MintPolicy::default().rendezvous_credits_per_join + 2),
        "one credit per newcomer plus one per whole KiB served"
    );
    assert_eq!(minter.total_earned(), 3);

    // A sealed window is claim-once, and the work is consumed with it.
    assert_eq!(
        minter.mint_rendezvous(&receipt, t0),
        Err(MintError::NoSealedRendezvousWork)
    );
    assert_eq!(minter.pending_rendezvous_joins(), 0);
    assert_eq!(minter.pending_rendezvous_bytes(), 0);
}

#[test]
fn test_rendezvous_rejects_a_receipt_that_does_not_match_the_window() {
    let seed = Keypair::generate();
    let mut minter = minter_with(&seed, no_cooldown_policy());
    let t0 = Instant::now();
    minter.rendezvous_join(600);

    // A rewritten window fails the seed signature before it can fail the chain.
    let mut receipt = minter.seal_rendezvous_window(&seed).expect("sealed");
    receipt.joins += 1;
    assert_eq!(
        minter.mint_rendezvous(&receipt, t0),
        Err(MintError::BadSignature)
    );

    // A does-not-match receipt is still refused.
    let other = RendezvousReceipt::sign(&seed, 99, 5, 5, [1u8; 32]);
    assert_eq!(
        minter.mint_rendezvous(&other, t0),
        Err(MintError::RendezvousChainBroken)
    );

    // Another identity cannot claim this node's window either.
    let receipt = minter
        .seal_rendezvous_window(&seed)
        .unwrap_or_else(|| RendezvousReceipt::sign(&seed, 0, 1, 600, [0u8; 32]));
    let stranger = Keypair::generate();
    let foreign = RendezvousReceipt::sign(&stranger, 0, 1, 600, receipt.chain_head);
    assert_eq!(
        minter.mint_rendezvous(&foreign, t0),
        Err(MintError::WrongSigner)
    );
}

#[test]
fn test_rendezvous_window_holds_until_a_whole_kib_is_served() {
    let seed = Keypair::generate();
    let mut minter = minter_with(&seed, no_cooldown_policy());

    // Nothing served: nothing to seal.
    assert!(minter.seal_rendezvous_window(&seed).is_none());

    // A few hundred bytes of table would round to zero credits, so the window
    // stays open instead of being sealed away.
    minter.record_peer_table_served(2, 512);
    assert!(minter.seal_rendezvous_window(&seed).is_none());
    minter.record_peer_table_served(2, 512);
    let receipt = minter
        .seal_rendezvous_window(&seed)
        .expect("a whole KiB is worth sealing");
    assert_eq!(receipt.peer_bytes, 1024);
    assert_eq!(minter.mint_rendezvous(&receipt, Instant::now()), Ok(1));
}

#[test]
fn test_mint_error_display_and_error_trait() {
    let all: [MintError; 12] = [
        MintError::BadSignature,
        MintError::UnknownChallenge,
        MintError::ChallengeExpired,
        MintError::ContentMismatch,
        MintError::BundleUnreadable,
        MintError::TermMismatch,
        MintError::WrongSigner,
        MintError::RelayChainBroken,
        MintError::NoSealedRelayWork,
        MintError::RendezvousChainBroken,
        MintError::NoSealedRendezvousWork,
        MintError::Cooldown {
            retry_after: Duration::from_secs(2),
        },
    ];
    for err in all {
        let text = err.to_string();
        assert!(!text.is_empty());
        let _dyn: &dyn std::error::Error = &err;
    }
    let limited = MintError::RateLimited {
        needed_bytes: 512,
        available_bytes: 256,
    };
    assert!(limited.to_string().contains("512"));
    assert!(MintError::EpochCapReached.to_string().contains("cap"));
}
