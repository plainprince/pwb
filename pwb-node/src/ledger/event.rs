//! Credit-chain event records.

use pwb_core::crypto::PublicKey;
use serde::{Deserialize, Serialize};

/// Whether a [`CreditEvent`] moves credits in or out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Credits earned for a verified network service.
    Earned,
    /// Credits spent (e.g. a domain claim on `register.plain`).
    Spent,
}

impl EventKind {
    pub(super) fn tag(self) -> u8 {
        match self {
            EventKind::Earned => 1,
            EventKind::Spent => 2,
        }
    }
}

/// One subject's credit-history entry as attestors store it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreditEvent {
    /// Whose balance this event moves.
    pub subject: PublicKey,
    /// Epoch the event was recorded in (monotone per subject).
    pub epoch: u64,
    /// Direction of the movement.
    pub kind: EventKind,
    /// Credits moved.
    pub amount: u64,
    /// Evidence digest binding the event to its supporting receipt/claim.
    pub evidence: [u8; 32],
}

/// A [`CreditEvent`] chained onto its subject's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainedEvent {
    /// The event itself.
    pub event: CreditEvent,
    /// Chain head before this event.
    pub prev_head: [u8; 32],
    /// Chain head after folding this event.
    pub head: [u8; 32],
}
