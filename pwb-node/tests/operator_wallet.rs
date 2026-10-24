//! Operator wallet flows over live loopback UDP: withdraw a blind voucher from
//! a peer attestor, redeem it for a private name claim, and claim another name
//! against a fresh attested balance certificate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pwb_core::crypto::{ContentId, Keypair};
use pwb_core::net::{NodeConfig, NodeEngine};
use pwb_node::attestation::{current_epoch, AttestationService};
use pwb_node::domain::{DomainRegistry, DomainService};
use pwb_node::wallet::{self, Wallet};

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

#[tokio::test]
async fn wallet_withdraws_spends_and_claims_against_attested_balance() {
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let a = engine(kp_a.clone());
    let b = engine(kp_b.clone());
    a.start().await.unwrap();
    b.start().await.unwrap();
    let addr_b = b.local_addr().unwrap().unwrap();
    a.connect_to(addr_b, kp_b.public).unwrap();

    let attestor = Arc::new(AttestationService::new(
        b.clone(),
        kp_b.clone(),
        &kp_b.secret.0,
    ));
    attestor
        .record_earned(kp_a.public, current_epoch(), 50, [7; 32])
        .expect("attestor records verified work");
    let attestor_handle = tokio::spawn(Arc::clone(&attestor).run());

    // Separate fresh registries keep each flow at the initial price: a claim
    // gossips a pricing epoch, so chaining them through one registry would
    // raise the second claim's price.
    let attested_domain = DomainService::new(
        a.clone(),
        Arc::new(Mutex::new(DomainRegistry::new())),
        kp_a.clone(),
        0,
        None,
    );
    let blind_domain = DomainService::new(
        a.clone(),
        Arc::new(Mutex::new(DomainRegistry::new())),
        kp_a.clone(),
        0,
        None,
    );

    let dir = std::env::temp_dir().join(format!(
        "pwb-wallet-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("vouchers.json");
    let mut wallet = Wallet::open(&path);
    assert!(wallet.vouchers().is_empty());

    // The attestor vouches for 50 credits; the attested path spends 10 of it.
    let record = wallet::attested_claim(
        &a,
        &attested_domain,
        "attested-wallet.pwb",
        ContentId([6; 32]),
        Duration::from_secs(2),
    )
    .await
    .expect("attested claim accepts the attested balance");
    assert_eq!(record.target_content_id, ContentId([6; 32]));
    assert_eq!(record.owner, kp_a.public);
    assert_eq!(attestor.attested_balance(&kp_a.public), 50);

    let (receipt, issuer) =
        wallet::withdraw_from_peers(&a, &blind_domain, 10, None, Duration::from_secs(2))
            .await
            .expect("peer attests enough balance for a voucher");
    assert_eq!(issuer, kp_b.public);
    assert_eq!(receipt.total, 10);
    assert_eq!(wallet.add_receipt(&receipt).unwrap(), 1);
    assert_eq!(wallet.total(), 10);
    assert_eq!(
        Wallet::open(&path).total(),
        10,
        "the voucher survives a wallet reload"
    );

    let record = wallet::blind_claim(&blind_domain, &mut wallet, "wallet.pwb", ContentId([5; 32]))
        .expect("blind voucher funds a private claim");
    assert_eq!(record.target_content_id, ContentId([5; 32]));
    assert_eq!(record.owner, kp_a.public);
    assert_eq!(wallet.total(), 0, "the spent voucher leaves the wallet");
    assert_eq!(
        Wallet::open(&path).vouchers().len(),
        0,
        "the spend is persisted"
    );
    // The issuer recorded the withdrawal volume, so its view of the subject
    // dropped by the voucher's face value.
    assert_eq!(attestor.attested_balance(&kp_a.public), 40);

    attestor_handle.abort();
    a.stop().await;
    b.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
