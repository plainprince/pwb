//! Blind withdrawal: converting attested credits into unlinkable vouchers.

use std::fmt;

use pwb_core::crypto::{
    BlindVoucher, BlindVoucherNonce, BlindVoucherPartialSignature, BlindVoucherPublicKey,
    ContentId, PublicKey, VoucherBlindRequest, VoucherBlindState, VoucherBlinding, VoucherSecret,
};

use super::error::BlindError;
use super::issuer::BlindBalanceIssuer;
use crate::blind::BLIND_PREFIX;
use crate::ledger::AttestorLedger;

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
    /// Attestor-signed binding of the denomination key, when the withdrawal
    /// ran over the network (needed to build a private claim).
    pub binding: Option<crate::blind::IssuerBinding>,
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
        binding: None,
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
