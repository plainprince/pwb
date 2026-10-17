//! Deterministic shard keys, attestor selection, and chain folds.

use std::time::Duration;

use pwb_core::crypto::{ContentId, PublicKey};
use pwb_core::dht::NodeId;

use super::event::{ChainedEvent, CreditEvent, EventKind};
use crate::ledger::LEDGER_SIGN_PREFIX;

/// Per-epoch shard key $\text{BLAKE3}(\text{NodeID} \parallel \text{Epoch})$
/// with a domain-separation prefix; the deterministic attestation coordinate.
pub fn epoch_key(node_id: &NodeId, epoch: u64) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(LEDGER_SIGN_PREFIX.len() + 40);
    bytes.extend_from_slice(LEDGER_SIGN_PREFIX);
    bytes.extend_from_slice(b"shard");
    bytes.extend_from_slice(node_id.as_bytes());
    bytes.extend_from_slice(&epoch.to_le_bytes());
    ContentId::compute(&bytes).0
}

/// Deterministically select the $N$ attestors for `subject` at `epoch` from
/// `peers`: rank every candidate by XOR distance between its epoch shard key
/// and the subject's epoch shard key, take the $N$ closest. The subject is
/// never its own attestor; ties break on public-key bytes.
pub fn select_attestors(
    subject: &PublicKey,
    epoch: u64,
    peers: &[PublicKey],
    n: usize,
) -> Vec<PublicKey> {
    let target = epoch_key(&NodeId::from_public_key(subject), epoch);
    let mut ranked: Vec<([u8; 32], PublicKey)> = peers
        .iter()
        .filter(|peer| **peer != *subject)
        .map(|peer| {
            let key = epoch_key(&NodeId::from_public_key(peer), epoch);
            (xor_bytes(&key, &target), *peer)
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1 .0.cmp(&b.1 .0)));
    ranked.into_iter().take(n).map(|(_, peer)| peer).collect()
}

/// Genesis chain head for a subject's attestor history.
pub fn genesis_head(subject: &PublicKey) -> [u8; 32] {
    fold_head(&[0u8; 32], subject, 0, EventKind::Earned, 0, &[0u8; 32])
}

/// Recompute the chain head an event *should* produce after `prev_head`.
pub fn chain_head_after(prev_head: [u8; 32], event: &CreditEvent) -> [u8; 32] {
    fold_head(
        &prev_head,
        &event.subject,
        event.epoch,
        event.kind,
        event.amount,
        &event.evidence,
    )
}

/// Verify a subject's full chained history from genesis: every link's
/// `prev_head`, recomputed `head`, and the final running balance all agree.
pub fn verify_history(subject: &PublicKey, events: &[ChainedEvent]) -> Option<u64> {
    let mut prev_head = genesis_head(subject);
    let mut balance = 0u64;
    for chained in events {
        if chained.event.subject != *subject || chained.prev_head != prev_head {
            return None;
        }
        let expected = chain_head_after(prev_head, &chained.event);
        if chained.head != expected {
            return None;
        }
        match chained.event.kind {
            EventKind::Earned => balance = balance.checked_add(chained.event.amount)?,
            EventKind::Spent => balance = balance.checked_sub(chained.event.amount)?,
        }
        prev_head = expected;
    }
    Some(balance)
}

/// Epoch index for `now` given a timeline start and epoch length.
pub fn epoch_at(start: std::time::Instant, now: std::time::Instant, length: Duration) -> u64 {
    let secs = length.as_secs().max(1);
    now.saturating_duration_since(start).as_secs() / secs
}

fn fold_head(
    prev_head: &[u8; 32],
    subject: &PublicKey,
    epoch: u64,
    kind: EventKind,
    amount: u64,
    evidence: &[u8; 32],
) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(LEDGER_SIGN_PREFIX.len() + 80);
    bytes.extend_from_slice(LEDGER_SIGN_PREFIX);
    bytes.extend_from_slice(b"fold");
    bytes.extend_from_slice(prev_head);
    bytes.extend_from_slice(&subject.0);
    bytes.extend_from_slice(&epoch.to_le_bytes());
    bytes.push(kind.tag());
    bytes.extend_from_slice(&amount.to_le_bytes());
    bytes.extend_from_slice(evidence);
    ContentId::compute(&bytes).0
}

pub(crate) fn xor_bytes(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        out[i] = x ^ y;
    }
    out
}
