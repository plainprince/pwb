//! Networked peer attestation: balance certificates, blind voucher issuance,
//! and remote index-proof verification.
//!
//! This is the wire-level counterpart of the library modules:
//!
//! - [`AttestationService`] runs on every node, answers [`AttestBalance`] with
//!   a signed statement from its [`AttestorLedger`], and implements the
//!   two-round blind withdrawal protocol with its [`BlindBalanceIssuer`].
//! - [`collect_balance_certificate`] gathers `k` attestations from the chosen
//!   neighbors into a [`QuorumCertificate`].
//! - [`withdraw_blind`] converts a subject's attested balance into unlinkable
//!   bearer vouchers without the issuer learning the tokens.
//! - [`request_proof_verdict`] asks a peer to re-verify index work against the
//!   actual bundle bytes.
//!
//! [`AttestBalance`]: pwb_core::net::NetworkMessage::AttestBalance

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pwb_core::crypto::{ContentId, Keypair, PublicKey};
use pwb_core::discovery::unix_time;
use pwb_core::net::{
    index_proof_sign_message, BlindClaimWire, IndexProofWire, NetworkMessage, NodeEngine,
    VoucherBlindRequestWire,
};
use pwb_core::wire::Bundle;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::blind::{
    binding_to_wire, claim_to_wire, request_from_wire, sign_binding, verified_binding,
    withdrawal_evidence, BlindBalanceIssuer, BlindError, BlindWithdrawal, IssuerBinding,
    WithdrawalReceipt, WithdrawnVoucher,
};
use crate::credit::{canonical_term_hash, MintPolicy};
use crate::ledger::{
    AttestorLedger, BalanceAttestation, ChainedEvent, LedgerError, QuorumCertificate,
};

/// Maximum vouchers one withdrawal may request.
pub const MAX_WITHDRAWAL_CHUNKS: usize = 64;

/// How long a client waits for an attestor reply.
pub const ATTEST_TIMEOUT: Duration = Duration::from_millis(700);

struct PendingWithdrawal {
    query_id: u64,
    subject: PublicKey,
    epoch: u64,
    nonce_points: Vec<[u8; 32]>,
}

/// A node's attestation endpoint: signed balances, blind issuance, and
/// verification work.
pub struct AttestationService {
    engine: NodeEngine,
    keypair: Keypair,
    ledger: Mutex<AttestorLedger>,
    issuer: Mutex<BlindBalanceIssuer>,
    pending: Mutex<Option<PendingWithdrawal>>,
    policy: MintPolicy,
    path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct PersistedHistory {
    subject: PublicKey,
    events: Vec<ChainedEvent>,
}

impl AttestationService {
    /// Create a service whose issuer keys derive from `master_seed`.
    pub fn new(engine: NodeEngine, keypair: Keypair, master_seed: &[u8; 32]) -> Self {
        Self::with_path(engine, keypair, master_seed, None)
    }

    /// Like [`Self::new`], but reload the attestor's verified histories from
    /// `path` and persist every accepted idea/earning/spend there.
    pub fn open(
        engine: NodeEngine,
        keypair: Keypair,
        master_seed: &[u8; 32],
        path: impl AsRef<Path>,
    ) -> Self {
        Self::with_path(
            engine,
            keypair,
            master_seed,
            Some(path.as_ref().to_path_buf()),
        )
    }

    fn with_path(
        engine: NodeEngine,
        keypair: Keypair,
        master_seed: &[u8; 32],
        path: Option<PathBuf>,
    ) -> Self {
        let ledger = match &path {
            Some(path) => load_attestor_ledger(path, keypair.public),
            None => AttestorLedger::new(keypair.public),
        };
        Self {
            engine,
            keypair,
            ledger: Mutex::new(ledger),
            issuer: Mutex::new(BlindBalanceIssuer::with_defaults(master_seed)),
            pending: Mutex::new(None),
            policy: MintPolicy::default(),
            path,
        }
    }

    fn persist(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let histories: Vec<PersistedHistory> = self
            .ledger
            .lock()
            .unwrap()
            .snapshot()
            .into_iter()
            .map(|(subject, events)| PersistedHistory { subject, events })
            .collect();
        let value = serde_json::json!({
            "version": 1,
            "attestor": self.keypair.public,
            "histories": histories,
        });
        let Ok(json) = serde_json::to_vec_pretty(&value) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// This node's attestor identity.
    pub fn public_key(&self) -> PublicKey {
        self.keypair.public
    }

    /// Record verified earnings for a subject (used by work-verification paths
    /// and tests). The same evidence can only be recorded once.
    pub fn record_earned(
        &self,
        subject: PublicKey,
        epoch: u64,
        amount: u64,
        evidence: [u8; 32],
    ) -> Result<u64, LedgerError> {
        let balance = self
            .ledger
            .lock()
            .unwrap()
            .record_earned(subject, epoch, amount, evidence)?;
        self.persist();
        Ok(balance)
    }

    /// Record a spend (e.g. an attested domain claim) on this attestor's view.
    pub fn record_spent(
        &self,
        subject: PublicKey,
        epoch: u64,
        amount: u64,
        evidence: [u8; 32],
    ) -> Result<u64, LedgerError> {
        let balance = self
            .ledger
            .lock()
            .unwrap()
            .record_spent(subject, epoch, amount, evidence)?;
        self.persist();
        Ok(balance)
    }

    /// The balance this attestor currently vouches for.
    pub fn attested_balance(&self, subject: &PublicKey) -> u64 {
        self.ledger.lock().unwrap().balance(subject)
    }

    /// Sign a local balance statement for `subject` (mainly for tests and
    /// diagnostics; over the wire use [`Self::run`]).
    pub fn attest(
        &self,
        subject: PublicKey,
        epoch: u64,
    ) -> Result<BalanceAttestation, LedgerError> {
        self.ledger
            .lock()
            .unwrap()
            .attest(subject, epoch, &self.keypair)
    }

    /// Run the inbound handler until the engine stops.
    pub async fn run(self: Arc<Self>) {
        let mut rx = self.engine.subscribe();
        while let Some((from, message)) = rx.recv().await {
            match message {
                NetworkMessage::AttestBalance {
                    subject,
                    epoch,
                    query_id,
                    ..
                } => {
                    if subject == self.keypair.public {
                        continue;
                    }
                    if let Ok(attestation) = self.attest(subject, epoch) {
                        let _ = self.engine.send_to_addr(
                            from,
                            NetworkMessage::BalanceAttest {
                                query_id,
                                attestation: (&attestation).into(),
                            },
                        );
                    }
                }

                NetworkMessage::WithdrawCommit {
                    subject,
                    epoch,
                    denomination,
                    count,
                    query_id,
                    ..
                } => self.on_withdraw_commit(
                    from,
                    subject,
                    epoch,
                    denomination,
                    count as usize,
                    query_id,
                ),

                NetworkMessage::WithdrawSign {
                    query_id,
                    subject,
                    epoch,
                    requests,
                    ..
                } => self.on_withdraw_sign(from, query_id, subject, epoch, requests),

                NetworkMessage::CheckIndexProof {
                    query_id,
                    proof,
                    bundle_bytes,
                } => {
                    let valid = verify_index_proof(&proof, &bundle_bytes);
                    if valid && proof.miner != self.keypair.public {
                        // Verified remote work is what peer attestors vouch
                        // for: every verifier records the same earning, so a
                        // subject's balance converges across its neighborhood.
                        let _ = self.record_earned(
                            proof.miner,
                            current_epoch(),
                            self.policy.index_credits,
                            proof.content_id.0,
                        );
                    }
                    let _ = self.engine.send_to_addr(
                        from,
                        NetworkMessage::ProofVerdict {
                            query_id,
                            valid,
                            verifier: self.keypair.public,
                        },
                    );
                }

                _ => {}
            }
        }
    }

    fn on_withdraw_commit(
        &self,
        from: SocketAddr,
        subject: PublicKey,
        epoch: u64,
        denomination: u64,
        count: usize,
        query_id: u64,
    ) {
        if subject == self.keypair.public
            || count == 0
            || count > MAX_WITHDRAWAL_CHUNKS
            || self.pending.lock().unwrap().is_some()
        {
            return;
        }
        let Some(voucher_key) = self.issuer.lock().unwrap().public_key(denomination) else {
            return;
        };
        let nonce_points = self
            .issuer
            .lock()
            .unwrap()
            .commit_nonces(denomination, count);
        if nonce_points.len() != count {
            return;
        }
        let binding = sign_binding(&self.keypair, epoch, denomination, voucher_key);
        *self.pending.lock().unwrap() = Some(PendingWithdrawal {
            query_id,
            subject,
            epoch,
            nonce_points: nonce_points.clone(),
        });
        let _ = self.engine.send_to_addr(
            from,
            NetworkMessage::WithdrawNonces {
                query_id,
                binding: binding_to_wire(&binding),
                nonce_points,
            },
        );
    }

    fn on_withdraw_sign(
        &self,
        from: SocketAddr,
        query_id: u64,
        subject: PublicKey,
        epoch: u64,
        requests: Vec<VoucherBlindRequestWire>,
    ) {
        let pending = self.pending.lock().unwrap().take();
        let Some(pending) = pending else {
            return;
        };
        if pending.query_id != query_id
            || pending.subject != subject
            || pending.epoch != epoch
            || requests.len() != pending.nonce_points.len()
        {
            // Put the pending withdrawal back only when the correlation fields
            // match; a mismatched request must not consume someone else's
            // committed nonces.
            *self.pending.lock().unwrap() = Some(pending);
            return;
        }

        let requests: Vec<pwb_core::crypto::VoucherBlindRequest> =
            requests.iter().map(request_from_wire).collect();
        let total: u64 = requests.iter().map(|request| request.amount).sum();
        let balance = self.ledger.lock().unwrap().balance(&subject);
        let Ok(partials) = self.issuer.lock().unwrap().sign_batch(&requests, balance) else {
            return;
        };

        // Record the volume spend before the requester can unblind anything.
        // If the ledger refuses, the signatures are not sent and the vouchers
        // never become spendable.
        let evidence = withdrawal_evidence(&requests);
        if self
            .ledger
            .lock()
            .unwrap()
            .record_spent(subject, epoch, total, evidence)
            .is_err()
        {
            return;
        }
        self.persist();

        let _ = self.engine.send_to_addr(
            from,
            NetworkMessage::WithdrawSignatures {
                query_id,
                partials: partials.iter().map(|partial| partial.to_bytes()).collect(),
            },
        );
    }
}

/// Load an attestor's verified histories from disk. A missing/corrupt file
/// yields an empty ledger; individual histories are re-verified on restore.
fn load_attestor_ledger(path: &Path, attestor: PublicKey) -> AttestorLedger {
    let Ok(bytes) = std::fs::read(path) else {
        return AttestorLedger::new(attestor);
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return AttestorLedger::new(attestor);
    };
    if let Some(file_attestor) = value.get("attestor") {
        if serde_json::from_value::<PublicKey>(file_attestor.clone()).ok() != Some(attestor) {
            return AttestorLedger::new(attestor);
        }
    }
    let histories = value
        .get("histories")
        .and_then(|raw| serde_json::from_value::<Vec<PersistedHistory>>(raw.clone()).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|entry| (entry.subject, entry.events));
    AttestorLedger::restore(attestor, histories)
}

/// Verify an index-work proof against the actual bundle bytes.
pub fn verify_index_proof(proof: &IndexProofWire, bundle_bytes: &[u8]) -> bool {
    if !proof.signature.verify(
        &proof.miner,
        &index_proof_sign_message(&proof.content_id, &proof.term_hash),
    ) {
        return false;
    }
    if ContentId::compute(bundle_bytes) != proof.content_id {
        return false;
    }
    let Ok(bundle) = Bundle::deserialize(bundle_bytes) else {
        return false;
    };
    canonical_term_hash(&bundle) == proof.term_hash
}

/// Gather attestations from `attestors` into a `k`-of-`N` certificate.
///
/// Only statements whose attestor is one of `attestors` and whose signature
/// verifies are accepted; the first statement defines the certificate and any
/// disagreement is rejected. Returns `None` when fewer than `need` attestors
/// answered in time.
pub async fn collect_balance_certificate(
    engine: &NodeEngine,
    subject: PublicKey,
    epoch: u64,
    attestors: &[PublicKey],
    need: usize,
    timeout: Duration,
) -> Option<QuorumCertificate> {
    if attestors.is_empty() || need == 0 {
        return None;
    }
    let query_id = rand::random::<u64>();
    let mut rx = engine.subscribe();
    for attestor in attestors {
        let _ = engine.send_to(
            attestor,
            NetworkMessage::AttestBalance {
                subject,
                epoch,
                query_id,
                sender: engine.public_key(),
            },
        );
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let mut statements: Vec<BalanceAttestation> = Vec::new();
    while statements.len() < need {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some((
                _,
                NetworkMessage::BalanceAttest {
                    query_id: answer_id,
                    attestation,
                },
            ))) if answer_id == query_id => {
                if !attestors.contains(&attestation.attestor) {
                    continue;
                }
                let statement = BalanceAttestation::from(&attestation);
                if !statement.verify()
                    || statements.iter().any(|s| s.attestor == statement.attestor)
                {
                    continue;
                }
                statements.push(statement);
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    QuorumCertificate::from_attestations(statements).ok()
}

/// Withdraw `chunks` vouchers of `denomination` credits from `issuer`'s
/// attested view of `subject`'s balance.
///
/// Runs both rounds of the blind-signature protocol over the network. The
/// issuer never sees the voucher tokens; it records only the blinded volume
/// evidence.
pub async fn withdraw_blind(
    engine: &NodeEngine,
    issuer: PublicKey,
    subject: PublicKey,
    epoch: u64,
    denomination: u64,
    chunks: usize,
    timeout: Duration,
) -> Result<WithdrawalReceipt, BlindError> {
    if chunks == 0 || chunks > MAX_WITHDRAWAL_CHUNKS {
        return Err(BlindError::InvalidAmount);
    }
    let query_id = rand::random::<u64>();
    let mut rx = engine.subscribe();
    engine
        .send_to(
            &issuer,
            NetworkMessage::WithdrawCommit {
                subject,
                epoch,
                denomination,
                count: chunks as u32,
                query_id,
                sender: engine.public_key(),
            },
        )
        .map_err(|err| BlindError::LedgerRejected(err.to_string()))?;

    let (binding, nonce_points) =
        wait_for_nonces(&mut rx, query_id, timeout)
            .await
            .ok_or(BlindError::LedgerRejected(
                "issuer did not commit nonces".to_string(),
            ))?;
    if binding.denomination != denomination {
        return Err(BlindError::UnknownDenomination(denomination));
    }
    let withdrawal = BlindWithdrawal::prepare(denomination, &binding.voucher_key, &nonce_points)?;
    let requests = withdrawal.requests();
    let total = withdrawal.total();
    let evidence = withdrawal_evidence(&requests);

    engine
        .send_to(
            &issuer,
            NetworkMessage::WithdrawSign {
                query_id,
                subject,
                epoch,
                requests: requests.iter().map(crate::blind::request_to_wire).collect(),
                sender: engine.public_key(),
            },
        )
        .map_err(|err| BlindError::LedgerRejected(err.to_string()))?;

    let partials = wait_for_signatures(&mut rx, query_id, timeout)
        .await
        .ok_or(BlindError::LedgerRejected(
            "issuer did not return blind signatures".to_string(),
        ))?;
    let vouchers = withdrawal.finish(&partials)?;
    Ok(WithdrawalReceipt {
        vouchers,
        evidence,
        total,
        denomination,
        binding: Some(binding),
    })
}

async fn wait_for_nonces(
    rx: &mut mpsc::Receiver<(SocketAddr, NetworkMessage)>,
    query_id: u64,
    timeout: Duration,
) -> Option<(IssuerBinding, Vec<[u8; 32]>)> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some((
                _,
                NetworkMessage::WithdrawNonces {
                    query_id: answer_id,
                    binding,
                    nonce_points,
                },
            ))) if answer_id == query_id => {
                let binding = verified_binding(&binding)?;
                return Some((binding, nonce_points));
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    }
}

async fn wait_for_signatures(
    rx: &mut mpsc::Receiver<(SocketAddr, NetworkMessage)>,
    query_id: u64,
    timeout: Duration,
) -> Option<Vec<pwb_core::crypto::BlindVoucherPartialSignature>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some((
                _,
                NetworkMessage::WithdrawSignatures {
                    query_id: answer_id,
                    partials,
                },
            ))) if answer_id == query_id => {
                return partials
                    .iter()
                    .map(|bytes| pwb_core::crypto::BlindVoucherPartialSignature::from_bytes(*bytes))
                    .collect();
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    }
}

/// Ask `verifier` to check an index proof against `bundle_bytes`.
pub async fn request_proof_verdict(
    engine: &NodeEngine,
    verifier: PublicKey,
    proof: IndexProofWire,
    bundle_bytes: Vec<u8>,
    timeout: Duration,
) -> Option<bool> {
    let query_id = rand::random::<u64>();
    let mut rx = engine.subscribe();
    engine
        .send_to(
            &verifier,
            NetworkMessage::CheckIndexProof {
                query_id,
                proof,
                bundle_bytes,
            },
        )
        .ok()?;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some((
                _,
                NetworkMessage::ProofVerdict {
                    query_id: answer_id,
                    valid,
                    verifier: answerer,
                },
            ))) if answer_id == query_id && answerer == verifier => return Some(valid),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    }
}

/// Current hour epoch used for issuer bindings and claims.
pub fn current_epoch() -> u64 {
    unix_time() / 3600
}

/// Private claim submission payload used by the domain service.
pub struct BlindClaimSubmission {
    pub wire: BlindClaimWire,
    pub secret: pwb_core::crypto::VoucherSecret,
}

/// Sign and broadcast a blind claim after building its wire representation.
pub fn build_blind_claim(
    withdrawn: &WithdrawnVoucher,
    binding: &IssuerBinding,
) -> Option<BlindClaimSubmission> {
    let claim = crate::blind::BlindClaim::build(withdrawn, binding)?;
    Some(BlindClaimSubmission {
        wire: claim_to_wire(&claim),
        secret: withdrawn.secret.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(kp: &Keypair) -> NodeEngine {
        NodeEngine::new(pwb_core::net::NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 10,
            heartbeat_interval: Duration::from_secs(30),
        })
    }

    #[test]
    fn attestor_histories_persist_and_reload_only_valid_chains() {
        let dir = std::env::temp_dir().join(format!("pwb-attest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("attestations.json");
        let attestor = Keypair::generate();
        let subject = Keypair::generate();

        {
            let service =
                AttestationService::open(engine(&attestor), attestor.clone(), &[1u8; 32], &path);
            service
                .record_earned(subject.public, 1, 25, [4u8; 32])
                .unwrap();
            assert_eq!(service.attested_balance(&subject.public), 25);
        }
        {
            let service =
                AttestationService::open(engine(&attestor), attestor.clone(), &[1u8; 32], &path);
            assert_eq!(service.attested_balance(&subject.public), 25);
        }

        // Editing a recorded amount breaks the chain, so it is not restored.
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["histories"][0]["events"][0]["event"]["amount"] = serde_json::json!(1_000_000);
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let service = AttestationService::open(engine(&attestor), attestor, &[1u8; 32], &path);
        assert_eq!(service.attested_balance(&subject.public), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
