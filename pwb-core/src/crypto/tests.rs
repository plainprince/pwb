use super::*;

#[test]
fn content_id_produces_32_byte_blake3() {
    let id = ContentId::compute(b"hello world");
    assert_eq!(id.0.len(), 32);
}

#[test]
fn ed25519_sign_verify_roundtrip() {
    let keypair = Keypair::generate();
    let message = b"test message";
    let signature = keypair.sign(message);
    assert!(signature.verify(&keypair.public, message));
}

#[test]
fn x25519_diffie_hellman_agreement() {
    let alice = DhKeypair::generate();
    let bob = DhKeypair::generate();
    let shared_alice = alice.diffie_hellman(&bob.public);
    let shared_bob = bob.diffie_hellman(&alice.public);
    assert_eq!(shared_alice, shared_bob);
}

#[test]
fn x25519_shared_secret_is_not_default() {
    let alice = DhKeypair::generate();
    let bob = DhKeypair::generate();
    let shared = alice.diffie_hellman(&bob.public);
    assert_ne!(shared, [0u8; 32]);
}

/// Runs the full interactive issuance: nonce, blinded request, blind
/// signature, then unblinding into a verifiable voucher.
fn issue_voucher(
    issuer: &BlindVoucherIssuer,
    amount: u64,
    secret: &VoucherSecret,
    blinding: &VoucherBlinding,
) -> BlindVoucher {
    let public = issuer.public_key();
    let nonce = issuer.signing_nonce();
    let (state, request) = BlindVoucher::blind_request(amount, secret, blinding, &public, &nonce)
        .expect("valid issuer key");
    let partial = issuer
        .sign_blind(nonce, &request)
        .expect("issuer accepts the amount");
    BlindVoucher::unblind(state, &partial).expect("valid blinded signature")
}

#[test]
fn blind_voucher_issuance_and_redemption() {
    let issuer = BlindVoucherIssuer::generate();
    let public = issuer.public_key();
    let amount = 42;
    let secret = VoucherSecret::from_bytes([0x42; 32]);
    let blinding = VoucherBlinding::from_seed([0x13; 32]);

    let voucher = issue_voucher(&issuer, amount, &secret, &blinding);

    assert_eq!(voucher.amount, amount);
    assert!(voucher.verify(&public));
    assert!(voucher.verify_owner(&secret));
    assert!(voucher.token_point().is_some());

    let wrong_secret = VoucherSecret::from_bytes([0x43; 32]);
    assert!(!voucher.verify_owner(&wrong_secret));

    let other_issuer = BlindVoucherIssuer::generate();
    assert!(!voucher.verify(&other_issuer.public_key()));
}

#[test]
fn blind_voucher_binds_amount_and_token() {
    let issuer = BlindVoucherIssuer::generate();
    let public = issuer.public_key();
    let secret = VoucherSecret::random();
    let blinding = VoucherBlinding::random();
    let voucher = issue_voucher(&issuer, 10, &secret, &blinding);

    assert!(voucher.verify(&public));

    let mut tampered_amount = voucher.clone();
    tampered_amount.amount = 1_000_000;
    assert!(!tampered_amount.verify(&public));

    let mut tampered_token = voucher.clone();
    tampered_token.token = VoucherSecret::random().to_bytes();
    assert!(!tampered_token.verify(&public));

    let mut tampered_response = voucher.clone();
    tampered_response.response[0] ^= 0x01;
    assert!(!tampered_response.verify(&public));
}

#[test]
fn blind_voucher_issuances_are_unlinkable() {
    let issuer = BlindVoucherIssuer::generate();
    let public = issuer.public_key();
    let secret = VoucherSecret::random();

    let first = issue_voucher(&issuer, 5, &secret, &VoucherBlinding::random());
    let second = issue_voucher(&issuer, 5, &secret, &VoucherBlinding::random());

    // Both are valid, but the issuer-visible signature material differs,
    // so the issuer cannot link a redemption back to an issuance request.
    assert!(first.verify(&public));
    assert!(second.verify(&public));
    assert_ne!(first.challenge, second.challenge);
    assert_ne!(first.response, second.response);
}

#[test]
fn blind_voucher_serialization_roundtrip() {
    let issuer = BlindVoucherIssuer::generate();
    let public = issuer.public_key();
    let secret = VoucherSecret::random();
    let voucher = issue_voucher(&issuer, 7, &secret, &VoucherBlinding::random());

    let encoded = voucher.to_bytes();
    let decoded = BlindVoucher::from_bytes(&encoded);
    assert_eq!(voucher, decoded);
    assert!(decoded.verify(&public));
    assert!(decoded.verify_owner(&secret));
    assert_eq!(voucher.to_hex(), hex::encode(encoded));
}

#[test]
fn blind_voucher_partial_signature_roundtrip() {
    let issuer = BlindVoucherIssuer::generate();
    let public = issuer.public_key();
    let secret = VoucherSecret::random();
    let blinding = VoucherBlinding::random();

    let nonce = issuer.signing_nonce();
    let (state, request) =
        BlindVoucher::blind_request(11, &secret, &blinding, &public, &nonce).unwrap();
    let partial = issuer.sign_blind(nonce, &request).unwrap();
    let bytes = partial.to_bytes();
    let restored = BlindVoucherPartialSignature::from_bytes(bytes).unwrap();

    let voucher = BlindVoucher::unblind(state, &restored).unwrap();
    assert!(voucher.verify(&public));
    assert!(BlindVoucherPartialSignature::from_bytes([0xff; 32]).is_none());
}

#[test]
fn blind_voucher_ownership_proof_is_zero_knowledge() {
    let issuer = BlindVoucherIssuer::generate();
    let public = issuer.public_key();
    let secret = VoucherSecret::random();
    let voucher = issue_voucher(&issuer, 3, &secret, &VoucherBlinding::random());

    let proof = voucher.prove_ownership(&secret).expect("holder can prove");
    assert!(voucher.verify_ownership(&proof));

    // A third party verifies with no access to the bearer secret.
    assert!(voucher.verify(&public));

    // Proofs must not be transferable to another voucher.
    let other = issue_voucher(
        &issuer,
        3,
        &VoucherSecret::random(),
        &VoucherBlinding::random(),
    );
    assert!(!other.verify_ownership(&proof));

    // A wrong secret cannot produce a proof at all.
    let wrong = VoucherSecret::random();
    assert!(voucher.prove_ownership(&wrong).is_none());

    // Tampered proof components are rejected.
    let mut tampered = proof;
    tampered.response[0] ^= 0x01;
    assert!(!voucher.verify_ownership(&tampered));

    let encoded = proof.to_bytes();
    assert_eq!(VoucherOwnershipProof::from_bytes(&encoded), proof);
    assert_eq!(proof.to_hex(), hex::encode(encoded));
}

#[test]
fn blind_voucher_denomination_keys_enforce_amount() {
    let master_seed = [0x5a; 32];
    let hundred = BlindVoucherIssuer::from_denomination_seed(&master_seed, 100);
    let thousand = BlindVoucherIssuer::from_denomination_seed(&master_seed, 1000);
    let public = hundred.public_key();

    assert_eq!(hundred.denomination(), Some(100));
    assert!(hundred.accepts_amount(100));
    assert!(!hundred.accepts_amount(1000));
    assert_ne!(
        hundred.public_key().to_bytes(),
        thousand.public_key().to_bytes()
    );

    // The 100 key issues a 100 voucher.
    let voucher = issue_voucher(
        &hundred,
        100,
        &VoucherSecret::random(),
        &VoucherBlinding::random(),
    );
    assert!(voucher.verify(&public));

    // Asking the 100 key to authorize a different amount is refused.
    let secret = VoucherSecret::random();
    let blinding = VoucherBlinding::random();
    let nonce = hundred.signing_nonce();
    let (_state, request) =
        BlindVoucher::blind_request(1000, &secret, &blinding, &public, &nonce).unwrap();
    assert!(hundred.sign_blind(nonce, &request).is_none());

    // A 1000 voucher cannot verify against the 100 public key.
    let thousand_secret = VoucherSecret::random();
    let thousand_voucher = issue_voucher(
        &thousand,
        1000,
        &thousand_secret,
        &VoucherBlinding::random(),
    );
    assert!(!thousand_voucher.verify(&public));
}

#[test]
fn blind_voucher_nonce_point_roundtrip_is_requester_side() {
    let issuer = BlindVoucherIssuer::generate();
    let nonce = issuer.signing_nonce();
    let point = nonce.to_bytes();

    // The requester reconstructs the commitment from the wire bytes and
    // can complete blind_request with it.
    let restored = BlindVoucherNonce::from_point(point).expect("valid point");
    assert_eq!(restored.to_bytes(), point);

    let secret = VoucherSecret::random();
    let blinding = VoucherBlinding::random();
    let public = issuer.public_key();
    let (state, request) = BlindVoucher::blind_request(5, &secret, &blinding, &public, &restored)
        .expect("blinded request");
    let partial = issuer.sign_blind(nonce, &request).expect("sign");
    let voucher = BlindVoucher::unblind(state, &partial).expect("unblind");
    assert!(voucher.verify(&public));

    // Garbage bytes do not decompress.
    assert!(BlindVoucherNonce::from_point([0xff; 32]).is_none());
}

#[test]
fn blind_voucher_spend_log_rejects_replays() {
    let issuer = BlindVoucherIssuer::generate();
    let public = issuer.public_key();
    let secret = VoucherSecret::random();
    let voucher = issue_voucher(&issuer, 25, &secret, &VoucherBlinding::random());
    let proof = voucher.prove_ownership(&secret).unwrap();

    let mut log = VoucherSpendLog::new();
    assert!(log.is_empty());
    assert!(!log.is_spent(&voucher));

    assert_eq!(log.redeem(&voucher, &public).unwrap(), 25);
    assert_eq!(log.len(), 1);
    assert!(log.is_spent(&voucher));
    assert_eq!(
        log.redeem(&voucher, &public),
        Err(VoucherSpendError::AlreadySpent)
    );

    // An unrelated voucher still redeems, and its proof checks out.
    let second = issue_voucher(
        &issuer,
        25,
        &VoucherSecret::random(),
        &VoucherBlinding::random(),
    );
    assert_eq!(
        log.redeem_with_proof(&second, &public, &proof),
        Err(VoucherSpendError::InvalidVoucher)
    );
    let second_secret = VoucherSecret::random();
    let third = issue_voucher(&issuer, 25, &second_secret, &VoucherBlinding::random());
    let third_proof = third.prove_ownership(&second_secret).unwrap();
    assert_eq!(
        log.redeem_with_proof(&third, &public, &third_proof)
            .unwrap(),
        25
    );
    assert_eq!(log.len(), 2);

    // A forged voucher is rejected before any state changes.
    let mut forged = voucher.clone();
    forged.amount = 1_000_000;
    assert_eq!(
        log.redeem(&forged, &public),
        Err(VoucherSpendError::InvalidVoucher)
    );
}
