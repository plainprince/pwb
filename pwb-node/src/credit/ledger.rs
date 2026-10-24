//! Signed, hash-chained credit ledger (`pwb-node::credit::ledger`).
//!
//! A node's balance is not a number in a file: it is the running sum of
//! entries that were appended by this node's identity and chained together by
//! hash. Every entry signs `seq ‖ prev ‖ kind ‖ credit ‖ detail`, where `prev`
//! is the digest of the previous entry's signed message and signature. Loading
//! replays the chain and stops at the first entry whose sequence, hash link,
//! signature, or affordability check fails, so hand-editing the JSON cannot
//! mint spendable credits and cannot resurrect a spent balance.
//!
//! The ledger is the local *spend gate*: it records credits earned from
//! verified services (query receipts, bundle indexing, relayed cells) and
//! debits domain claims. It is deliberately not an attestation: peers enforce
//! prices on the signed claim, not on this file.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use pwb_core::crypto::{ContentId, Keypair, PublicKey, Signature};
use serde::{Deserialize, Serialize};

/// Domain-separation prefix for signed credit entries.
pub const CREDIT_ENTRY_PREFIX: &[u8] = b"PWBCREDIT1";

/// One signed movement in the local ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreditEntry {
    /// Strictly increasing position in the chain (0-based).
    pub seq: u64,
    /// Digest of the previous entry (`[0; 32]` for the first entry).
    pub prev: [u8; 32],
    /// Service that produced the credit (`query`, `index`, `relay`, `seed`).
    pub kind: String,
    /// Credits moved: earned entries are positive, spends are recorded in the
    /// `append` direction flag and always stored as unsigned amounts.
    pub amount: u64,
    /// `true` when the entry debits the balance.
    pub spent: bool,
    /// Human-readable audit detail (domain name, content id, …).
    pub detail: String,
    /// Owner signature over all of the fields above.
    pub signature: Signature,
}

impl CreditEntry {
    /// Canonical bytes the owner signs for this entry.
    pub fn sign_message(
        seq: u64,
        prev: &[u8; 32],
        kind: &str,
        amount: u64,
        spent: bool,
        detail: &str,
    ) -> Vec<u8> {
        let mut msg =
            Vec::with_capacity(CREDIT_ENTRY_PREFIX.len() + 32 + 32 + 8 + 8 + 1 + detail.len() + 8);
        msg.extend_from_slice(CREDIT_ENTRY_PREFIX);
        msg.extend_from_slice(&seq.to_le_bytes());
        msg.extend_from_slice(prev);
        msg.extend_from_slice(&(kind.len() as u32).to_le_bytes());
        msg.extend_from_slice(kind.as_bytes());
        msg.extend_from_slice(&amount.to_le_bytes());
        msg.push(u8::from(spent));
        msg.extend_from_slice(&(detail.len() as u32).to_le_bytes());
        msg.extend_from_slice(detail.as_bytes());
        msg
    }

    /// Digest chaining this entry into the next one: hash of the signed
    /// message plus the signature (so a re-signed entry is still a new link).
    fn link_digest(&self) -> [u8; 32] {
        let mut bytes = Self::sign_message(
            self.seq,
            &self.prev,
            &self.kind,
            self.amount,
            self.spent,
            &self.detail,
        );
        bytes.extend_from_slice(&self.signature.0);
        ContentId::compute(&bytes).0
    }

    /// Whether this entry's signature verifies against `owner`.
    pub fn verify(&self, owner: &PublicKey) -> bool {
        self.signature.verify(
            owner,
            &Self::sign_message(
                self.seq,
                &self.prev,
                &self.kind,
                self.amount,
                self.spent,
                &self.detail,
            ),
        )
    }
}

/// Why a ledger append was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    /// A spend exceeds the earned balance.
    InsufficientCredits {
        /// Current balance.
        balance: u64,
        /// Credits the spend requires.
        needed: u64,
    },
    /// The on-disk ledger could not be written.
    Persist(String),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::InsufficientCredits { balance, needed } => {
                write!(f, "need {needed} credits, have {balance}")
            }
            LedgerError::Persist(e) => write!(f, "cannot persist credit ledger: {e}"),
        }
    }
}

impl std::error::Error for LedgerError {}

#[derive(Debug, Default)]
struct State {
    entries: Vec<CreditEntry>,
    balance: u64,
}

/// A node's signed credit history and derived spendable balance.
pub struct CreditLedger {
    keypair: Keypair,
    path: Option<PathBuf>,
    state: Mutex<State>,
}

impl CreditLedger {
    /// Open (or create) the ledger for `keypair`.
    ///
    /// When `path` is set and holds a valid chain, the balance is derived from
    /// it. A `seed` is only ever applied to a ledger with no valid entries and
    /// is recorded as a signed `seed` entry, never as a bare number.
    pub fn new(keypair: Keypair, path: Option<PathBuf>, seed: u64) -> Self {
        let state = match &path {
            Some(p) => Self::load(&keypair.public, p),
            None => State::default(),
        };
        let ledger = Self {
            keypair,
            path,
            state: Mutex::new(state),
        };
        if seed > 0 && ledger.entries().is_empty() {
            ledger.append("seed", seed, false, "initial balance");
        }
        ledger
    }

    /// The ledger file (if this ledger is persistent).
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Spendable balance derived from the signed chain.
    pub fn balance(&self) -> u64 {
        self.state.lock().unwrap().balance
    }

    /// Credits earned over the ledger's lifetime (never decreases).
    pub fn earned(&self) -> u64 {
        self.state
            .lock()
            .unwrap()
            .entries
            .iter()
            .filter(|entry| !entry.spent)
            .map(|entry| entry.amount)
            .sum()
    }

    /// A copy of every verified entry, in chain order.
    pub fn entries(&self) -> Vec<CreditEntry> {
        self.state.lock().unwrap().entries.clone()
    }

    /// Record credits earned from a verified service (`query`, `index`,
    /// `relay`). The entry is signed and chained, then persisted.
    pub fn record_earned(&self, kind: &str, amount: u64, detail: &str) {
        if amount > 0 {
            self.append(kind, amount, false, detail);
        }
    }

    /// Debit the balance for `kind` (e.g. a domain claim). Refuses when the
    /// chain does not cover `amount`.
    pub fn spend(&self, kind: &str, amount: u64, detail: &str) -> Result<u64, LedgerError> {
        {
            let state = self.state.lock().unwrap();
            if state.balance < amount {
                return Err(LedgerError::InsufficientCredits {
                    balance: state.balance,
                    needed: amount,
                });
            }
        }
        self.append(kind, amount, true, detail);
        Ok(self.balance())
    }

    fn append(&self, kind: &str, amount: u64, spent: bool, detail: &str) {
        let mut state = self.state.lock().unwrap();
        let seq = state.entries.len() as u64;
        let prev = state
            .entries
            .last()
            .map(CreditEntry::link_digest)
            .unwrap_or([0u8; 32]);
        let signature = self.keypair.sign(&CreditEntry::sign_message(
            seq, &prev, kind, amount, spent, detail,
        ));
        let next_balance = if spent {
            state.balance.saturating_sub(amount)
        } else {
            state.balance.saturating_add(amount)
        };
        state.entries.push(CreditEntry {
            seq,
            prev,
            kind: kind.to_string(),
            amount,
            spent,
            detail: detail.to_string(),
            signature,
        });
        state.balance = next_balance;
        let _ = self.persist(&state);
    }

    fn persist(&self, state: &State) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let value = serde_json::json!({
            "version": 1,
            "owner": self.keypair.public.to_hex(),
            "entries": state.entries,
        });
        let json = serde_json::to_vec_pretty(&value).map_err(std::io::Error::other)?;
        // Write beside the target and rename so a crash never leaves a
        // half-written ledger that would truncate the chain on next load.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    /// Replay `path`, keeping only the longest valid prefix.
    fn load(owner: &PublicKey, path: &Path) -> State {
        let Ok(bytes) = std::fs::read(path) else {
            return State::default();
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return State::default();
        };
        // A ledger written for a different identity is not ours to spend.
        if let Some(file_owner) = value.get("owner").and_then(|v| v.as_str()) {
            if file_owner != owner.to_hex() {
                return State::default();
            }
        }
        let Some(entries) = value
            .get("entries")
            .and_then(|v| serde_json::from_value::<Vec<CreditEntry>>(v.clone()).ok())
        else {
            return State::default();
        };

        let mut state = State::default();
        for entry in entries {
            if entry.seq != state.entries.len() as u64 {
                break;
            }
            let expected_prev = state
                .entries
                .last()
                .map(CreditEntry::link_digest)
                .unwrap_or([0u8; 32]);
            if entry.prev != expected_prev || !entry.verify(owner) {
                break;
            }
            if entry.spent {
                if entry.amount > state.balance {
                    break;
                }
                state.balance -= entry.amount;
            } else {
                state.balance = state.balance.saturating_add(entry.amount);
            }
            state.entries.push(entry);
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pwb-credit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn earned_and_spent_entries_derive_the_balance() {
        let keypair = Keypair::generate();
        let ledger = CreditLedger::new(keypair.clone(), None, 0);
        ledger.record_earned("query", 5, "term=plainweb");
        ledger.record_earned("relay", 7, "window 0");
        assert_eq!(ledger.balance(), 12);
        assert_eq!(ledger.earned(), 12);
        assert_eq!(ledger.spend("claim", 10, "alice.pwb"), Ok(2));
        assert_eq!(ledger.balance(), 2);
        assert_eq!(
            ledger.spend("claim", 3, "bob.pwb"),
            Err(LedgerError::InsufficientCredits {
                balance: 2,
                needed: 3
            })
        );
        assert_eq!(ledger.entries().len(), 3, "failed spend is not recorded");
    }

    #[test]
    fn persisted_chain_reloads_and_rejects_tampering() {
        let dir = temp_dir("tamper");
        let path = dir.join("credits.json");
        let keypair = Keypair::generate();
        {
            let ledger = CreditLedger::new(keypair.clone(), Some(path.clone()), 0);
            ledger.record_earned("query", 50, "served plainweb");
        }
        // A clean restart derives the same balance from the chain.
        let reopened = CreditLedger::new(keypair.clone(), Some(path.clone()), 0);
        assert_eq!(reopened.balance(), 50);

        // Hand-editing the amount breaks the signature and the chain: the
        // forged entry (and everything after it) is discarded.
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let entries = value.get_mut("entries").unwrap().as_array_mut().unwrap();
        entries[0]["amount"] = serde_json::json!(1_000_000);
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let tampered = CreditLedger::new(keypair, Some(path), 0);
        assert_eq!(tampered.balance(), 0, "edited balance is not spendable");
        assert!(tampered.entries().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reordering_or_replaying_entries_is_rejected() {
        let keypair = Keypair::generate();
        let path =
            std::env::temp_dir().join(format!("pwb-credit-replay-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let ledger = CreditLedger::new(keypair.clone(), Some(path.clone()), 0);
        ledger.record_earned("query", 5, "one");
        ledger.record_earned("query", 5, "two");

        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let entries = value.get_mut("entries").unwrap().as_array_mut().unwrap();
        entries.swap(0, 1);
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

        let reopened = CreditLedger::new(keypair, Some(path.clone()), 0);
        assert_eq!(reopened.balance(), 0, "a broken seq/prev link stops replay");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn seed_is_recorded_as_a_signed_entry_only_once() {
        let keypair = Keypair::generate();
        let path =
            std::env::temp_dir().join(format!("pwb-credit-seed-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let ledger = CreditLedger::new(keypair.clone(), Some(path.clone()), 100);
            assert_eq!(ledger.balance(), 100);
            assert_eq!(ledger.entries()[0].kind, "seed");
            ledger.spend("claim", 50, "alice.pwb").unwrap();
        }
        let reopened = CreditLedger::new(keypair, Some(path.clone()), 100);
        assert_eq!(reopened.balance(), 50, "seed is not re-applied");
        let _ = std::fs::remove_file(&path);
    }
}
