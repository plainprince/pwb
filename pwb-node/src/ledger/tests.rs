use super::chain::xor_bytes;
use super::*;
use crate::credit::{canonical_term_hash, index_sign_message, IndexReceipt, MintPolicy};
use pwb_core::crypto::{ContentId, Keypair, PublicKey};
use pwb_core::dht::NodeId;
use pwb_core::spec::{PlainNode, Tag};
use pwb_core::wire::Bundle;
use std::collections::HashMap as Map;
use std::time::{Duration, Instant};

fn peer(i: u8) -> PublicKey {
    PublicKey([i; 32])
}

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
        components: Map::new(),
        modules: Map::new(),
    }
}

#[test]
fn test_attestor_selection_is_deterministic_and_excludes_subject() {
    let subject = Keypair::generate().public;
    let peers: Vec<PublicKey> = (1..=12).map(peer).collect();

    let a = select_attestors(&subject, 7, &peers, 5);
    let b = select_attestors(&subject, 7, &peers, 5);
    assert_eq!(a, b, "same inputs must select the same neighborhood");
    assert_eq!(a.len(), 5);
    assert!(!a.contains(&subject), "subject is never its own attestor");
    for member in &a {
        assert!(peers.contains(member));
    }

    // Requesting more than the candidate pool yields the whole pool.
    let all = select_attestors(&subject, 7, &peers, 32);
    assert_eq!(all.len(), peers.len());
}

#[test]
fn test_attestor_selection_rotates_with_epoch_and_ranks_by_shard_distance() {
    let subject = Keypair::generate().public;
    let peers: Vec<PublicKey> = (1..=16).map(peer).collect();

    let epoch0 = select_attestors(&subject, 0, &peers, 8);
    let epoch1 = select_attestors(&subject, 1, &peers, 8);
    assert_ne!(epoch0, epoch1, "membership must rotate with the epoch");

    // Ranked order matches XOR distance to the subject's shard key.
    let target = epoch_key(&NodeId::from_public_key(&subject), 3);
    let ranked = select_attestors(&subject, 3, &peers, 8);
    let mut last = [0u8; 32];
    for (i, member) in ranked.iter().enumerate() {
        let key = epoch_key(&NodeId::from_public_key(member), 3);
        let dist = xor_bytes(&key, &target);
        if i > 0 {
            assert!(last <= dist, "ranking must be by ascending distance");
        }
        last = dist;
    }
}

#[test]
fn test_record_earned_advances_balance_and_chain() {
    let attestor = Keypair::generate();
    let mut ledger = AttestorLedger::new(attestor.public);
    let subject = Keypair::generate().public;

    assert_eq!(ledger.balance(&subject), 0);
    assert_eq!(ledger.head(&subject), genesis_head(&subject));

    assert_eq!(ledger.record_earned(subject, 3, 10, [1; 32]), Ok(10));
    assert_eq!(ledger.record_earned(subject, 3, 5, [2; 32]), Ok(15));
    assert_eq!(ledger.balance(&subject), 15);
    assert_eq!(ledger.subject_count(), 1);

    let history = ledger.history(&subject);
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].prev_head, genesis_head(&subject));
    assert_eq!(history[1].prev_head, history[0].head);
    assert_eq!(verify_history(&subject, history), Some(15));
}

#[test]
fn test_ledger_refuses_self_attestation_and_duplicate_evidence() {
    let attestor = Keypair::generate();
    let mut ledger = AttestorLedger::new(attestor.public);
    let subject = Keypair::generate().public;

    assert_eq!(
        ledger.record_earned(attestor.public, 1, 10, [9; 32]),
        Err(LedgerError::SelfAttestation)
    );
    ledger.record_earned(subject, 2, 10, [1; 32]).unwrap();
    assert_eq!(
        ledger.record_earned(subject, 2, 10, [1; 32]),
        Err(LedgerError::DuplicateEvent)
    );
    // Same evidence for a different subject is a separate history.
    let other = Keypair::generate().public;
    assert_eq!(ledger.record_earned(other, 2, 3, [1; 32]), Ok(3));
}

#[test]
fn test_record_spent_spends_down_and_refuses_overdraw() {
    let attestor = Keypair::generate();
    let mut ledger = AttestorLedger::new(attestor.public);
    let subject = Keypair::generate().public;

    assert_eq!(
        ledger.record_spent(subject, 1, 10, [3; 32]),
        Err(LedgerError::InsufficientBalance {
            balance: 0,
            needed: 10
        })
    );
    ledger.record_earned(subject, 1, 30, [1; 32]).unwrap();
    assert_eq!(ledger.record_spent(subject, 2, 20, [2; 32]), Ok(10));
    assert_eq!(ledger.balance(&subject), 10);
    assert_eq!(
        ledger.record_spent(subject, 2, 11, [3; 32]),
        Err(LedgerError::InsufficientBalance {
            balance: 10,
            needed: 11
        })
    );
}

#[test]
fn test_epoch_regression_is_rejected() {
    let attestor = Keypair::generate();
    let mut ledger = AttestorLedger::new(attestor.public);
    let subject = Keypair::generate().public;
    ledger.record_earned(subject, 5, 1, [1; 32]).unwrap();
    assert_eq!(
        ledger.record_earned(subject, 4, 1, [2; 32]),
        Err(LedgerError::EpochRegression {
            previous: 5,
            incoming: 4
        })
    );
}

#[test]
fn test_record_index_earned_verifies_receipt_against_bundle() {
    let attestor = Keypair::generate();
    let mut ledger = AttestorLedger::new(attestor.public);
    let indexer = Keypair::generate();
    let policy = MintPolicy::default();

    let bytes = test_bundle("attested indexing credit").serialize();
    let receipt = IndexReceipt::sign(&indexer, &bytes).expect("receipt");

    assert_eq!(
        ledger.record_index_earned(&receipt, &bytes, 9, &policy),
        Ok(policy.index_credits)
    );
    assert_eq!(ledger.balance(&indexer.public), policy.index_credits);

    // Wrong bytes (content id mismatch) and garbage bytes are refused.
    let other = test_bundle("different content").serialize();
    let mut fresh = AttestorLedger::new(attestor.public);
    assert_eq!(
        fresh.record_index_earned(&receipt, &other, 9, &policy),
        Err(LedgerError::ReceiptInvalid)
    );
    assert_eq!(
        fresh.record_index_earned(&receipt, b"garbage", 9, &policy),
        Err(LedgerError::ReceiptInvalid)
    );

    // A receipt whose term hash was swapped fails the term re-check.
    let other_bundle = test_bundle("zz different terms entirely");
    let message = index_sign_message(
        indexer.public,
        receipt.content_id,
        canonical_term_hash(&other_bundle),
    );
    let forged = IndexReceipt {
        indexer: indexer.public,
        content_id: receipt.content_id,
        term_hash: canonical_term_hash(&other_bundle),
        signature: indexer.sign(&message),
    };
    assert_eq!(
        fresh.record_index_earned(&forged, &bytes, 9, &policy),
        Err(LedgerError::ReceiptInvalid)
    );

    // Undeserializable bundle with a consistent CID still fails.
    let garbage = b"not a bundle";
    let message = index_sign_message(
        indexer.public,
        ContentId::compute(garbage),
        ContentId::compute(b"t"),
    );
    let unreadable = IndexReceipt {
        indexer: indexer.public,
        content_id: ContentId::compute(garbage),
        term_hash: ContentId::compute(b"t"),
        signature: indexer.sign(&message),
    };
    assert_eq!(
        fresh.record_index_earned(&unreadable, garbage, 9, &policy),
        Err(LedgerError::UnreadableBundle)
    );
}

#[test]
fn test_attest_signs_and_verifies_statement() {
    let attestor = Keypair::generate();
    let mut ledger = AttestorLedger::new(attestor.public);
    let subject = Keypair::generate().public;
    ledger.record_earned(subject, 4, 42, [1; 32]).unwrap();

    let attestation = ledger.attest(subject, 4, &attestor).expect("attest");
    assert_eq!(attestation.balance, 42);
    assert_eq!(attestation.head, ledger.head(&subject));
    assert!(attestation.verify());

    // Wrong key, or attesting yourself, is refused.
    let other = Keypair::generate();
    assert_eq!(
        ledger.attest(subject, 4, &other),
        Err(LedgerError::SelfAttestation)
    );
    assert_eq!(
        ledger.attest(attestor.public, 4, &attestor),
        Err(LedgerError::SelfAttestation)
    );

    // A tampered statement no longer verifies.
    let mut tampered = attestation.clone();
    tampered.balance += 1;
    assert!(!tampered.verify());
}

#[test]
fn test_quorum_certificate_verifies_k_of_n() {
    let subject = Keypair::generate().public;
    let attestor_keys: Vec<Keypair> = (1..=5).map(|_| Keypair::generate()).collect();
    let neighborhood: Vec<PublicKey> = attestor_keys.iter().map(|k| k.public).collect();

    let mut ledger = AttestorLedger::new(attestor_keys[0].public);
    ledger.record_earned(subject, 11, 80, [7; 32]).unwrap();
    let head = ledger.head(&subject);

    // Two independent attestors agree on the statement.
    let ledger2 = AttestorLedger::new(attestor_keys[1].public);
    let a0 = BalanceAttestation::sign(&attestor_keys[0], subject, 11, 80, head);
    let a1 = BalanceAttestation::sign(&attestor_keys[1], subject, 11, 80, head);
    let _ = ledger2;
    let cert = QuorumCertificate::from_attestations(vec![a0, a1]).expect("certificate");
    assert_eq!(cert.verify(2, &neighborhood), Ok(()));
    // Also satisfies the lower threshold.
    assert_eq!(cert.verify(1, &neighborhood), Ok(()));
    // But not a higher one.
    assert_eq!(
        cert.verify(3, &neighborhood),
        Err(LedgerError::QuorumTooSmall { have: 2, need: 3 })
    );
}

#[test]
fn test_quorum_rejects_strangers_duplicates_and_tampering() {
    let subject = Keypair::generate().public;
    let keys: Vec<Keypair> = (1..=3).map(|_| Keypair::generate()).collect();
    let neighborhood: Vec<PublicKey> = keys.iter().map(|k| k.public).collect();
    let head = [9u8; 32];

    let a0 = BalanceAttestation::sign(&keys[0], subject, 1, 50, head);
    let a1 = BalanceAttestation::sign(&keys[1], subject, 1, 50, head);

    // A signer outside the neighborhood is rejected.
    let stranger = Keypair::generate();
    let a_stranger = BalanceAttestation::sign(&stranger, subject, 1, 50, head);
    let cert = QuorumCertificate::from_attestations(vec![a0.clone(), a_stranger]).unwrap();
    assert_eq!(
        cert.verify(2, &neighborhood),
        Err(LedgerError::SignerNotInNeighborhood)
    );

    // The same attestor cannot count twice.
    let dup = QuorumCertificate::from_attestations(vec![a0.clone(), a0.clone()]).unwrap();
    assert_eq!(
        dup.verify(2, &neighborhood),
        Err(LedgerError::DuplicateSigner)
    );

    // The subject cannot vouch for itself.
    let self_sig = BalanceAttestation::sign(&keys[0], subject, 1, 50, head);
    let mut self_cert_body = self_sig.clone();
    self_cert_body.attestor = subject;
    let self_cert = QuorumCertificate {
        subject,
        epoch: 1,
        balance: 50,
        head,
        attestations: vec![self_cert_body],
    };
    assert_eq!(
        self_cert.verify(1, &neighborhood),
        Err(LedgerError::SelfAttestation)
    );

    // A forged signature inside an otherwise valid quorum fails.
    let mut forged = a1.clone();
    forged.signature = keys[2].sign(b"wrong message");
    let cert = QuorumCertificate::from_attestations(vec![a0.clone(), forged]).unwrap();
    assert_eq!(
        cert.verify(2, &neighborhood),
        Err(LedgerError::BadAttestationSignature)
    );

    // Mixed statements cannot assemble into one certificate.
    let a_other = BalanceAttestation::sign(&keys[1], subject, 1, 51, head);
    assert_eq!(
        QuorumCertificate::from_attestations(vec![a0, a_other]),
        Err(LedgerError::AttestationMismatch)
    );
    assert_eq!(
        QuorumCertificate::from_attestations(vec![]),
        Err(LedgerError::QuorumTooSmall { have: 0, need: 1 })
    );
}

#[test]
fn test_quorum_verify_detects_envelope_tampering() {
    let subject = Keypair::generate().public;
    let keys: Vec<Keypair> = (1..=2).map(|_| Keypair::generate()).collect();
    let neighborhood: Vec<PublicKey> = keys.iter().map(|k| k.public).collect();
    let a0 = BalanceAttestation::sign(&keys[0], subject, 1, 50, [1; 32]);
    let a1 = BalanceAttestation::sign(&keys[1], subject, 1, 50, [1; 32]);
    let mut cert = QuorumCertificate::from_attestations(vec![a0, a1]).unwrap();

    cert.balance = 500;
    assert_eq!(
        cert.verify(2, &neighborhood),
        Err(LedgerError::AttestationMismatch)
    );
}

#[test]
fn test_epoch_at_and_verify_history_detects_tampering() {
    let start = Instant::now();
    let length = Duration::from_secs(60);
    assert_eq!(epoch_at(start, start, length), 0);
    assert_eq!(epoch_at(start, start + Duration::from_secs(59), length), 0);
    assert_eq!(epoch_at(start, start + Duration::from_secs(60), length), 1);
    // Degenerate epoch length never divides by zero.
    assert_eq!(
        epoch_at(start, start + Duration::from_secs(2), Duration::ZERO),
        2
    );

    // History verification catches a flipped head byte.
    let attestor = Keypair::generate();
    let mut ledger = AttestorLedger::new(attestor.public);
    let subject = Keypair::generate().public;
    ledger.record_earned(subject, 1, 10, [1; 32]).unwrap();
    let mut history = ledger.history(&subject).to_vec();
    assert_eq!(verify_history(&subject, &history), Some(10));
    history[0].head[0] ^= 0xff;
    assert_eq!(verify_history(&subject, &history), None);
    // ...and an event re-pointed at another subject.
    let mut other = ledger.history(&subject).to_vec();
    other[0].event.subject = Keypair::generate().public;
    assert_eq!(verify_history(&subject, &other), None);
    // Spends are subtracted in the recomputed balance.
    ledger.record_spent(subject, 2, 4, [2; 32]).unwrap();
    assert_eq!(verify_history(&subject, ledger.history(&subject)), Some(6));
}

#[test]
fn test_ledger_errors_implement_display_and_error() {
    let all = [
        LedgerError::SelfAttestation,
        LedgerError::EpochRegression {
            previous: 2,
            incoming: 1,
        },
        LedgerError::DuplicateEvent,
        LedgerError::InsufficientBalance {
            balance: 1,
            needed: 2,
        },
        LedgerError::ReceiptInvalid,
        LedgerError::UnreadableBundle,
        LedgerError::QuorumTooSmall { have: 1, need: 3 },
        LedgerError::SignerNotInNeighborhood,
        LedgerError::DuplicateSigner,
        LedgerError::AttestationMismatch,
        LedgerError::BadAttestationSignature,
        LedgerError::InsufficientAttestedBalance {
            attested: 40,
            already_claimed: 5,
            needed: 50,
        },
        LedgerError::ReservedSystemDomain,
        LedgerError::DomainAlreadyRegistered,
    ];
    for err in all {
        assert!(!err.to_string().is_empty());
        let _dyn: &dyn std::error::Error = &err;
    }
}
