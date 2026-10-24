//! Publishing work must land in the node's signed ledger (and therefore be
//! spendable / attestable later), not just in memory.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use pwb_core::crypto::Keypair;
use pwb_core::net::{NodeConfig, NodeEngine};
use pwb_core::spec::{PlainNode, Tag};
use pwb_node::credit::CreditLedger;
use pwb_node::federation::DistributedSearch;
use pwb_node::host::ArtifactHost;
use pwb_node::search::SearchEngine;

fn bundle_bytes_with(body: &str, title: &str) -> Vec<u8> {
    let mut root = PlainNode::new(0, Tag::Document);
    let mut text = PlainNode::new(1, Tag::Text);
    text.text_content = Some(body.to_string());
    root.children.push(text);
    let bundle = pwb_web::bundle::create_bundle(
        root,
        title.to_string(),
        None,
        None,
        HashMap::new(),
        HashMap::new(),
    );
    pwb_web::compress::compress_bundle(&bundle)
}

fn bundle_bytes() -> Vec<u8> {
    // Large enough to clear the 10-credit / 64-bytes-per-credit index floor,
    // but with fewer terms than the receiver's per-peer burst bucket (256):
    // each term is replicated as its own `StorePosting`, and a fixture that
    // exceeds the burst would legitimately lose the trailing proof message.
    bundle_bytes_with(
        &(0..150u64)
            .map(|index| {
                format!(
                    "w{index}-{:016x} ",
                    index.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                )
            })
            .collect::<String>(),
        "Ledger Publish",
    )
}

fn engine(keypair: Keypair) -> NodeEngine {
    NodeEngine::new(NodeConfig {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        keypair,
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 10,
        heartbeat_interval: Duration::from_secs(30),
    })
}

#[tokio::test]
async fn publishing_persists_index_credits_in_the_shared_ledger() {
    let dir = std::env::temp_dir().join(format!("pwb-publish-ledger-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("credits.json");

    let keypair = Keypair::generate();
    let engine = engine(keypair.clone());
    let ledger = Arc::new(CreditLedger::new(keypair.clone(), Some(path.clone()), 0));
    let service = DistributedSearch::new(
        engine,
        Arc::new(SearchEngine::new()),
        Arc::new(ArtifactHost::new()),
        keypair.clone(),
    )
    .with_ledger(Arc::clone(&ledger));

    service
        .index_and_publish(&bundle_bytes())
        .expect("bundle publishes");

    assert!(service.credits_earned() >= 10, "index work is credited");
    assert!(path.exists(), "the shared ledger is written to disk");
    let reloaded = CreditLedger::new(keypair.clone(), Some(path.clone()), 0);
    assert!(reloaded.balance() >= 10, "the earning reloads from disk");

    let other = CreditLedger::new(Keypair::generate(), Some(path), 0);
    assert_eq!(
        other.entries().len(),
        0,
        "a ledger written for another identity is not loaded"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Peers are only asked to attest work the local anti-ASIC floor accepted: a
/// tiny (highly compressible) bundle must not produce a `CheckIndexProof`
/// broadcast, or verifiers would credit supply the local policy rejected.
#[tokio::test]
async fn rate_limited_index_work_is_not_broadcast_for_attestation() {
    use pwb_core::net::NetworkMessage;

    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let a = engine(kp_a.clone());
    let b = engine(kp_b.clone());
    a.start().await.unwrap();
    b.start().await.unwrap();
    let addr_b = b.local_addr().unwrap().unwrap();
    a.connect_to(addr_b, kp_b.public).unwrap();
    let mut inbound = b.subscribe();

    let service = DistributedSearch::new(
        a.clone(),
        Arc::new(SearchEngine::new()),
        Arc::new(ArtifactHost::new()),
        kp_a,
    );
    service
        .index_and_publish(&bundle_bytes_with("tiny", "Tiny"))
        .expect("tiny bundle indexes locally");

    let mut saw_proof = false;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(400);
    while let Ok(Some((_, message))) = tokio::time::timeout_at(deadline, inbound.recv()).await {
        if matches!(message, NetworkMessage::CheckIndexProof { .. }) {
            saw_proof = true;
        }
    }
    assert!(
        !saw_proof,
        "rate-limited work is not submitted for attestation"
    );

    service
        .index_and_publish(&bundle_bytes())
        .expect("large bundle indexes");
    let mut saw_proof = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while let Ok(Some((_, message))) = tokio::time::timeout_at(deadline, inbound.recv()).await {
        if matches!(message, NetworkMessage::CheckIndexProof { .. }) {
            saw_proof = true;
            break;
        }
    }
    assert!(
        saw_proof,
        "accepted index work is submitted for attestation"
    );

    a.stop().await;
    b.stop().await;
}
