//! Seed credits: a node that admits newcomers mints against the rendezvous
//! work its engine actually reported, into its signed credit ledger.

use std::sync::Arc;
use std::time::Duration;

use pwb_core::crypto::Keypair;
use pwb_core::net::{BootstrapPeer, NodeConfig, NodeEngine};
use pwb_node::credit::{CreditLedger, MintPolicy};
use pwb_node::seed::SeedCreditService;

fn engine(keypair: Keypair, bootstrap_peers: Vec<BootstrapPeer>) -> NodeEngine {
    NodeEngine::new(NodeConfig {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        keypair,
        bootstrap_peers,
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    })
}

async fn wait_for(mut check: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
async fn a_seed_earns_for_the_newcomers_it_admits() {
    let kp_seed = Keypair::generate();
    let seed = engine(kp_seed.clone(), Vec::new());
    seed.start().await.unwrap();
    let seed_addr = seed.local_addr().unwrap().unwrap();

    let ledger = Arc::new(CreditLedger::new(kp_seed.clone(), None, 0));
    let service = Arc::new(SeedCreditService::new(
        seed.clone(),
        &kp_seed,
        Arc::clone(&ledger),
        MintPolicy {
            per_service_cooldown: Duration::ZERO,
            ..MintPolicy::default()
        },
    ));
    let handle = tokio::spawn(Arc::clone(&service).run(kp_seed.clone()));

    // Two strangers join through the seed and never speak to each other, so
    // the only work the seed can bill for is admitting them.
    let mut newcomers = Vec::new();
    for _ in 0..2 {
        let kp = Keypair::generate();
        let newcomer = engine(
            kp.clone(),
            vec![BootstrapPeer {
                address: seed_addr,
                public_key: kp_seed.public,
            }],
        );
        newcomer.start().await.unwrap();
        let joined = newcomer.connect_bootstrap_verified().await;
        assert_eq!(joined.len(), 1, "the newcomer joins through the seed");
        assert_eq!(joined[0].public_key, kp_seed.public);
        newcomers.push(newcomer);
    }

    let credited = wait_for(|| ledger.balance() >= 2).await;
    assert!(
        credited,
        "one credit per admitted newcomer, got balance {}",
        ledger.balance()
    );
    assert!(
        ledger
            .entries()
            .iter()
            .any(|entry| entry.kind == "rendezvous"),
        "the earning is a signed rendezvous ledger entry"
    );

    handle.abort();
    for newcomer in newcomers {
        newcomer.stop().await;
    }
    seed.stop().await;
}

/// A node nobody joins earns nothing: being reachable is not a service.
#[tokio::test]
async fn an_unused_seed_earns_nothing() {
    let kp_seed = Keypair::generate();
    let seed = engine(kp_seed.clone(), Vec::new());
    seed.start().await.unwrap();

    let ledger = Arc::new(CreditLedger::new(kp_seed.clone(), None, 0));
    let service = Arc::new(SeedCreditService::with_defaults(
        seed.clone(),
        &kp_seed,
        Arc::clone(&ledger),
    ));
    let handle = tokio::spawn(Arc::clone(&service).run(kp_seed.clone()));

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(ledger.balance(), 0);
    assert_eq!(service.credits_earned(), 0);

    handle.abort();
    seed.stop().await;
}
