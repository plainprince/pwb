//! Blind balance & privacy preservation (§4.3).
//!
//! Attestors must be able to log credit *volume* without linking a node owner
//! to specific browsing actions or domain claims. This module layers the
//! Chaum-style blind signature vouchers of [`pwb_core::crypto`] over the §4.2
//! attested ledger:
//!
//! - **Withdrawal** — a node converts attested credits into unlinkable blind
//!   vouchers. The attestor-issuer ([`BlindBalanceIssuer`]) checks the
//!   subject's attested balance, signs *blinded* requests with
//!   denomination-bound keys (so amounts cannot be forged), and records a
//!   `Spent` volume event on its [`AttestorLedger`]. The issuer never learns
//!   the voucher tokens: only fresh blinding factors cross the wire.
//! - **Redemption** — the holder spends a voucher (e.g. on `register.plain`)
//!   by presenting it with a zero-knowledge
//!   [`VoucherOwnershipProof`](pwb_core::crypto::VoucherOwnershipProof). The
//!   registry sees only the bearer token: a spend is unlinkable to the
//!   withdrawal, to the earning identity, and to any other spend.
//!
//! # Cross-attestor coordination
//!
//! A subject's balance is attested by $N$ peers, so each attestor's ledger
//! shows the same earnings. Withdrawing from one attestor must therefore
//! *gossip* the spend event (same [`withdrawal_evidence`] digest) to the rest
//! of the neighborhood — otherwise each attestor would independently back a
//! fresh withdrawal and the $N$ views would inflate the spendable supply.
//! [`withdraw_attested`] records the spend on the ledger it is given; the
//! caller repeats [`AttestorLedger::record_spent`] with the returned
//! [`WithdrawalReceipt::evidence`] on the sibling ledgers.

use std::collections::{HashMap, VecDeque};
use std::fmt;

use pwb_core::crypto::{
    BlindVoucher, BlindVoucherIssuer, BlindVoucherNonce, BlindVoucherPartialSignature,
    BlindVoucherPublicKey, ContentId, Keypair, PublicKey, Signature, VoucherBlindRequest,
    VoucherBlindState, VoucherBlinding, VoucherOwnershipProof, VoucherSecret,
};

use crate::ledger::AttestorLedger;

/// Domain-separation prefix for issuer-binding signatures and withdrawal
/// evidence digests.
const BLIND_PREFIX: &[u8] = b"PWBBLIND1";

/// Default denominations a [`BlindBalanceIssuer`] supports (credits).
pub const DEFAULT_DENOMINATIONS: &[u64] = &[1, 5, 10, 25, 50];

/// Why a blind-balance operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlindError {
    /// The requested amount is not a whole multiple of the denomination.
    InvalidAmount,
    /// The issuer does not support the requested denomination.
    UnknownDenomination(u64),
    /// The subject's attested balance cannot cover the withdrawal.
    InsufficientBalance { balance: u64, needed: u64 },
    /// The prepared requests do not match the committed nonces.
    NonceCountMismatch,
    /// A request's amount falls outside its paired denomination key.
    AmountMismatch,
    /// The unblinded voucher does not verify against the issuer key.
    BadVoucher,
    /// The holder cannot prove ownership (wrong secret).
    NoOwnershipProof,
    /// The attestor's ledger refused the volume event.
    LedgerRejected(String),
}

impl fmt::Display for BlindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlindError::InvalidAmount => {
                write!(f, "amount is not a whole multiple of the denomination")
            }
            BlindError::UnknownDenomination(d) => write!(f, "unknown denomination {d}"),
            BlindError::InsufficientBalance { balance, needed } => {
                write!(
                    f,
                    "attested balance {balance} cannot cover withdrawal of {needed}"
                )
            }
            BlindError::NonceCountMismatch => {
                write!(f, "prepared requests do not match committed nonces")
            }
            BlindError::AmountMismatch => {
                write!(f, "request amount falls outside its denomination key")
            }
            BlindError::BadVoucher => write!(f, "unblinded voucher does not verify"),
            BlindError::NoOwnershipProof => write!(f, "cannot prove voucher ownership"),
            BlindError::LedgerRejected(reason) => {
                write!(f, "attestor ledger rejected withdrawal: {reason}")
            }
        }
    }
}

impl std::error::Error for BlindError {}

/// Why a blind domain claim was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlindClaimError {
    /// The `.plain` TLD is reserved for built-in system protocols.
    ReservedSystemDomain,
    /// The domain is already registered.
    DomainAlreadyRegistered,
    /// The issuer binding's signature does not verify.
    BadBinding,
    /// The binding's attestor is not a member of the claiming neighborhood.
    IssuerNotAttestor,
    /// The voucher does not verify against the bound denomination key.
    BadVoucher,
    /// The ownership proof is missing or invalid.
    NoOwnershipProof,
    /// The voucher's denomination is below the claim cost.
    Underfunded { voucher: u64, cost: u64 },
    /// The voucher's token was already redeemed.
    AlreadySpent,
}

impl fmt::Display for BlindClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlindClaimError::ReservedSystemDomain => {
                write!(f, "reserved system domain: .plain TLD is restricted")
            }
            BlindClaimError::DomainAlreadyRegistered => write!(f, "domain name already registered"),
            BlindClaimError::BadBinding => write!(f, "issuer binding signature is invalid"),
            BlindClaimError::IssuerNotAttestor => {
                write!(f, "binding attestor is not in the claiming neighborhood")
            }
            BlindClaimError::BadVoucher => {
                write!(f, "voucher does not verify against the bound key")
            }
            BlindClaimError::NoOwnershipProof => write!(f, "voucher ownership proof is invalid"),
            BlindClaimError::Underfunded { voucher, cost } => {
                write!(
                    f,
                    "voucher of {voucher} credits cannot cover claim cost {cost}"
                )
            }
            BlindClaimError::AlreadySpent => write!(f, "voucher token has already been spent"),
        }
    }
}

impl std::error::Error for BlindClaimError {}

/// One attestor's blind-signing capability: denomination-bound issuer keys
/// derived from a master seed, plus the single-use nonce queue pairing
/// committed nonces with incoming blinded requests.
///
/// The master seed is the issuer's *blind-signing* secret — distinct from the
/// attestor's Ed25519 identity key, which only signs [`IssuerBinding`]s.
pub struct BlindBalanceIssuer {
    issuers: HashMap<u64, BlindVoucherIssuer>,
    /// `(denomination, nonce)` in commit order; consumed by `sign_batch`.
    pending: VecDeque<(u64, BlindVoucherNonce)>,
}

impl BlindBalanceIssuer {
    /// Issuer supporting `denominations`, each key derived from `master_seed`
    /// via the domain-separated denomination derivation in `pwb-core`
    /// (distinct denominations yield independent, unlinkable keys).
    pub fn new(master_seed: &[u8; 32], denominations: &[u64]) -> Self {
        let issuers = denominations
            .iter()
            .map(|denomination| {
                (
                    *denomination,
                    BlindVoucherIssuer::from_denomination_seed(master_seed, *denomination),
                )
            })
            .collect();
        BlindBalanceIssuer {
            issuers,
            pending: VecDeque::new(),
        }
    }

    /// Issuer with [`DEFAULT_DENOMINATIONS`].
    pub fn with_defaults(master_seed: &[u8; 32]) -> Self {
        Self::new(master_seed, DEFAULT_DENOMINATIONS)
    }

    /// Verification key for a denomination, if supported.
    pub fn public_key(&self, denomination: u64) -> Option<BlindVoucherPublicKey> {
        self.issuers
            .get(&denomination)
            .map(|issuer| issuer.public_key())
    }

    /// Supported denominations, ascending.
    pub fn denominations(&self) -> Vec<u64> {
        let mut denoms: Vec<u64> = self.issuers.keys().copied().collect();
        denoms.sort_unstable();
        denoms
    }

    /// Commit to `count` fresh single-use nonces for `denomination` and
    /// return their commitment points for the requester. Requests must later
    /// be submitted in the same order via [`sign_batch`].
    pub fn commit_nonces(&mut self, denomination: u64, count: usize) -> Vec<[u8; 32]> {
        let Some(issuer) = self.issuers.get(&denomination) else {
            return Vec::new();
        };
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            let nonce = issuer.signing_nonce();
            points.push(nonce.to_bytes());
            self.pending.push_back((denomination, nonce));
        }
        points
    }

    /// Sign blinded withdrawal requests, paired positionally with the
    /// previously committed nonces. Refuses any batch whose total exceeds
    /// `max_spendable` (the subject's attested balance) or whose amounts
    /// fall outside their paired denomination key. Validation failures do
    /// not consume the pending nonces.
    pub fn sign_batch(
        &mut self,
        requests: &[VoucherBlindRequest],
        max_spendable: u64,
    ) -> Result<Vec<BlindVoucherPartialSignature>, BlindError> {
        if requests.len() != self.pending.len() {
            return Err(BlindError::NonceCountMismatch);
        }
        let total: u64 = requests.iter().map(|r| r.amount).sum();
        if total > max_spendable {
            return Err(BlindError::InsufficientBalance {
                balance: max_spendable,
                needed: total,
            });
        }
        for (request, (denomination, _)) in requests.iter().zip(&self.pending) {
            if request.amount != *denomination {
                return Err(BlindError::AmountMismatch);
            }
        }
        let mut partials = Vec::with_capacity(requests.len());
        for (request, (denomination, nonce)) in requests.iter().zip(self.pending.drain(..)) {
            let issuer = self
                .issuers
                .get(&denomination)
                .expect("pending nonces only come from supported denominations");
            let partial = issuer
                .sign_blind(nonce, request)
                .expect("amount already checked against the denomination key");
            partials.push(partial);
        }
        Ok(partials)
    }
}

impl fmt::Debug for BlindBalanceIssuer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlindBalanceIssuer")
            .field("denominations", &self.denominations())
            .field("pending_nonces", &self.pending.len())
            .finish()
    }
}

/// An attestor's Ed25519-signed statement binding one of its blind-signing
/// denomination keys to its identity for an epoch. The registry trusts a
/// voucher only through a binding whose attestor is a neighborhood member —
/// so only *attested* credits are privately spendable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuerBinding {
    /// Attestor identity vouching for the key.
    pub attestor: PublicKey,
    /// Epoch the binding is valid for (keys rotate per epoch).
    pub epoch: u64,
    /// Denomination the key issues.
    pub denomination: u64,
    /// The blind-signing public key.
    pub voucher_key: BlindVoucherPublicKey,
    /// Attestor signature over the canonical binding encoding.
    pub signature: Signature,
}

impl IssuerBinding {
    /// Sign a binding with the attestor's identity keypair.
    pub fn sign(
        attestor: &Keypair,
        epoch: u64,
        denomination: u64,
        voucher_key: BlindVoucherPublicKey,
    ) -> Self {
        let message = binding_sign_message(attestor.public, epoch, denomination, voucher_key);
        IssuerBinding {
            attestor: attestor.public,
            epoch,
            denomination,
            voucher_key,
            signature: attestor.sign(&message),
        }
    }

    /// Verify the attestor signature over the canonical binding encoding.
    pub fn verify(&self) -> bool {
        let message = binding_sign_message(
            self.attestor,
            self.epoch,
            self.denomination,
            self.voucher_key,
        );
        self.signature.verify(&self.attestor, &message)
    }
}

/// One voucher the holder withdrew, kept with the bearer secret needed to
/// prove ownership at redemption.
#[derive(Clone, PartialEq, Eq)]
pub struct WithdrawnVoucher {
    /// The unlinkable blind voucher (bearer instrument).
    pub voucher: BlindVoucher,
    /// The secret opening the voucher's token — never leaves the holder.
    pub secret: VoucherSecret,
}

impl fmt::Debug for WithdrawnVoucher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WithdrawnVoucher")
            .field("amount", &self.voucher.amount)
            .field("spend_id", &self.voucher.spend_id())
            .finish()
    }
}

/// Result of a successful withdrawal: the vouchers, the volume-evidence
/// digest the attestor recorded (gossip this to sibling ledgers), and the
/// total converted.
#[derive(Debug, PartialEq, Eq)]
pub struct WithdrawalReceipt {
    /// Vouchers the holder now owns.
    pub vouchers: Vec<WithdrawnVoucher>,
    /// Evidence digest of the recorded `Spent` event — identical across
    /// attestors that gossip the same withdrawal.
    pub evidence: [u8; 32],
    /// Total credits converted into vouchers.
    pub total: u64,
    /// Denomination of each voucher.
    pub denomination: u64,
}

/// Requester side of a withdrawal: fresh secrets and blinding factors bound
/// to the issuer's committed nonce points, ready to be signed blind.
pub struct BlindWithdrawal {
    denomination: u64,
    entries: Vec<WithdrawEntry>,
}

struct WithdrawEntry {
    secret: VoucherSecret,
    state: VoucherBlindState,
    request: VoucherBlindRequest,
}

impl fmt::Debug for BlindWithdrawal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlindWithdrawal")
            .field("denomination", &self.denomination)
            .field("requests", &self.entries.len())
            .finish()
    }
}

impl BlindWithdrawal {
    /// Prepare one blinded request per committed nonce point. Every voucher
    /// is for `denomination` credits; the nonce points come from
    /// [`BlindBalanceIssuer::commit_nonces`] and must be paired in order.
    pub fn prepare(
        denomination: u64,
        issuer_public: &BlindVoucherPublicKey,
        nonce_points: &[[u8; 32]],
    ) -> Result<Self, BlindError> {
        let mut entries = Vec::with_capacity(nonce_points.len());
        for point in nonce_points {
            let nonce =
                BlindVoucherNonce::from_point(*point).ok_or(BlindError::NonceCountMismatch)?;
            let secret = VoucherSecret::random();
            let blinding = VoucherBlinding::random();
            let (state, request) = BlindVoucher::blind_request(
                denomination,
                &secret,
                &blinding,
                issuer_public,
                &nonce,
            )
            .ok_or(BlindError::NonceCountMismatch)?;
            entries.push(WithdrawEntry {
                secret,
                state,
                request,
            });
        }
        Ok(BlindWithdrawal {
            denomination,
            entries,
        })
    }

    /// Total credits this withdrawal converts.
    pub fn total(&self) -> u64 {
        self.denomination * self.entries.len() as u64
    }

    /// The blinded requests to submit to the issuer.
    pub fn requests(&self) -> Vec<VoucherBlindRequest> {
        self.entries.iter().map(|entry| entry.request).collect()
    }

    /// Unblind the issuer's partial signatures into spendable vouchers,
    /// keeping each bearer secret alongside its voucher.
    pub fn finish(
        self,
        partials: &[BlindVoucherPartialSignature],
    ) -> Result<Vec<WithdrawnVoucher>, BlindError> {
        if partials.len() != self.entries.len() {
            return Err(BlindError::NonceCountMismatch);
        }
        let mut vouchers = Vec::with_capacity(self.entries.len());
        for (entry, partial) in self.entries.into_iter().zip(partials) {
            let voucher =
                BlindVoucher::unblind(entry.state, partial).ok_or(BlindError::BadVoucher)?;
            vouchers.push(WithdrawnVoucher {
                voucher,
                secret: entry.secret,
            });
        }
        Ok(vouchers)
    }
}

/// A complete private domain claim: the bearer voucher, its zero-knowledge
/// ownership proof, and the issuer binding tying the denomination key to a
/// neighborhood attestor.
#[derive(Debug, Clone)]
pub struct BlindClaim {
    /// The voucher being redeemed.
    pub voucher: BlindVoucher,
    /// Zero-knowledge proof that the redeemer holds the voucher's secret.
    pub proof: VoucherOwnershipProof,
    /// The attestor-signed denomination-key binding.
    pub binding: IssuerBinding,
}

impl BlindClaim {
    /// Build a claim from a withdrawn voucher and its binding. Returns `None`
    /// when the supplied secret does not open the voucher's token.
    pub fn build(withdrawn: &WithdrawnVoucher, binding: &IssuerBinding) -> Option<Self> {
        let proof = withdrawn.voucher.prove_ownership(&withdrawn.secret)?;
        Some(BlindClaim {
            voucher: withdrawn.voucher.clone(),
            proof,
            binding: binding.clone(),
        })
    }
}

/// Convert `chunks` vouchers of `denomination` credits out of `subject`'s
/// attested balance on one attestor's ledger (the in-process composition of
/// the withdrawal protocol; over the wire these are the same messages).
///
/// Records a `Spent` event on `ledger` with [`withdrawal_evidence`]; the
/// caller must gossip the same evidence to the subject's sibling attestor
/// ledgers so the $N$ views do not inflate the spendable supply.
pub fn withdraw_attested(
    issuer: &mut BlindBalanceIssuer,
    ledger: &mut AttestorLedger,
    subject: PublicKey,
    epoch: u64,
    denomination: u64,
    chunks: usize,
) -> Result<WithdrawalReceipt, BlindError> {
    let total = denomination
        .checked_mul(chunks as u64)
        .ok_or(BlindError::InvalidAmount)?;
    if chunks == 0 || total == 0 {
        return Err(BlindError::InvalidAmount);
    }
    let balance = ledger.balance(&subject);
    if total > balance {
        return Err(BlindError::InsufficientBalance {
            balance,
            needed: total,
        });
    }
    let issuer_public = issuer
        .public_key(denomination)
        .ok_or(BlindError::UnknownDenomination(denomination))?;

    // Issuer commits nonces; requester blinds against them.
    let nonce_points = issuer.commit_nonces(denomination, chunks);
    let withdrawal = BlindWithdrawal::prepare(denomination, &issuer_public, &nonce_points)?;

    // Issuer signs blind, bounded by the attested balance.
    let requests = withdrawal.requests();
    let partials = issuer.sign_batch(&requests, balance)?;

    // Requester unblinds into spendable vouchers.
    let vouchers = withdrawal.finish(&partials)?;

    // Volume event: the attestor logs the conversion (not the tokens).
    let evidence = withdrawal_evidence(&requests);
    ledger
        .record_spent(subject, epoch, total, evidence)
        .map_err(|err| BlindError::LedgerRejected(err.to_string()))?;

    Ok(WithdrawalReceipt {
        vouchers,
        evidence,
        total,
        denomination,
    })
}

/// Evidence digest of a withdrawal, derived only from what the issuer saw
/// (amounts and blinded challenges) — never from the voucher tokens, so the
/// recorded volume event stays unlinkable to the eventual spends. Fresh
/// blinding makes every withdrawal's digest unique.
pub fn withdrawal_evidence(requests: &[VoucherBlindRequest]) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(BLIND_PREFIX.len() + requests.len() * 40);
    bytes.extend_from_slice(BLIND_PREFIX);
    bytes.extend_from_slice(b"withdraw");
    for request in requests {
        bytes.extend_from_slice(&request.amount.to_le_bytes());
        bytes.extend_from_slice(&request.blinded_challenge);
    }
    ContentId::compute(&bytes).0
}

fn binding_sign_message(
    attestor: PublicKey,
    epoch: u64,
    denomination: u64,
    voucher_key: BlindVoucherPublicKey,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(BLIND_PREFIX.len() + 48);
    msg.extend_from_slice(BLIND_PREFIX);
    msg.extend_from_slice(b"binding");
    msg.extend_from_slice(&attestor.0);
    msg.extend_from_slice(&epoch.to_le_bytes());
    msg.extend_from_slice(&denomination.to_le_bytes());
    msg.extend_from_slice(&voucher_key.to_bytes());
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DomainRegistry, DOMAIN_CLAIM_COST};
    use pwb_core::crypto::VoucherSpendLog;
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

        let receipt =
            withdraw_attested(&mut issuer, &mut ledger, subject, 1, 10, 5).expect("withdraw");
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
        let receipt =
            withdraw_attested(&mut issuer, &mut ledger, subject, 1, 50, 1).expect("withdraw");

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
                DOMAIN_CLAIM_COST,
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
                DOMAIN_CLAIM_COST,
                &neighborhood,
            ),
            Err(BlindClaimError::ReservedSystemDomain)
        );
        assert_eq!(
            registry.register_blind(
                "private.pwb",
                ContentId([1; 32]),
                pseudonym,
                &claim,
                DOMAIN_CLAIM_COST,
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
            registry.register_blind("a.pwb", cid, pseudonym, &claim, 10, &neighborhood),
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
            registry.register_blind("b.pwb", cid, pseudonym, &claim, 10, &neighborhood),
            Err(BlindClaimError::IssuerNotAttestor)
        );

        // Voucher that does not verify against the bound key (bind the 10 key
        // to a 50 voucher).
        let wrong_key_binding =
            IssuerBinding::sign(&attestor, 1, 50, issuer.public_key(10).unwrap());
        let claim = BlindClaim {
            voucher: voucher.voucher.clone(),
            proof: voucher.voucher.prove_ownership(&voucher.secret).unwrap(),
            binding: wrong_key_binding,
        };
        assert_eq!(
            registry.register_blind("c.pwb", cid, pseudonym, &claim, 10, &neighborhood),
            Err(BlindClaimError::BadVoucher)
        );

        // Valid voucher but underfunded for the cost.
        let claim = BlindClaim::build(voucher, &binding).unwrap();
        assert_eq!(
            registry.register_blind("d.pwb", cid, pseudonym, &claim, 500, &neighborhood),
            Err(BlindClaimError::Underfunded {
                voucher: 50,
                cost: 500
            })
        );

        // Forged ownership proof.
        let mut forged = claim.clone();
        forged.proof.response[0] ^= 0xff;
        assert_eq!(
            registry.register_blind("e.pwb", cid, pseudonym, &forged, 10, &neighborhood),
            Err(BlindClaimError::NoOwnershipProof)
        );

        // A real claim succeeds, then the same voucher cannot be replayed.
        assert_eq!(
            registry.register_blind("f.pwb", cid, pseudonym, &claim, 10, &neighborhood),
            Ok(())
        );
        assert_eq!(
            registry.register_blind("g.pwb", cid, pseudonym, &claim, 10, &neighborhood),
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
}
