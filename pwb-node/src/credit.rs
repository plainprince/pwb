//! Proof-of-time & useful-work credit minting (`pwb-node::credit`).
//!
//! Credits are minted **only** against verified network services — never for
//! raw computation (TODO §4.1):
//!
//! - **Serving `search.plain` queries** — a [`QueryReceipt`] must close a
//!   challenge/response round-trip with a real requester: the server signs the
//!   query, its result set, and a nonce the *requester* issued (with TTL, one
//!   claim per nonce), so receipts cannot be fabricated without a live
//!   counterparty.
//! - **Indexing PML bundles** — an [`IndexReceipt`] is content-addressed: the
//!   bundle's BLAKE3 CID and a canonical hash of its term set must both
//!   re-verify against the actual bytes, so the indexer demonstrably parsed
//!   real data.
//! - **Relaying onion cells** — [`CreditMinter::relay_cell`] folds every cell
//!   into a hash chain; [`CreditMinter::seal_relay_window`] seals a window's
//!   cell count/bytes/chain head into a signed [`RelayReceipt`] that
//!   `mint_relay` re-checks against the sealed state (window-continuous,
//!   claim-once).
//! - **Running a seed** — [`CreditMinter::rendezvous_join`] and
//!   [`CreditMinter::record_peer_table_served`] fold the newcomers a node
//!   admitted and the peer-table bytes it handed out into their own chain;
//!   [`CreditMinter::seal_rendezvous_window`] seals them into a signed
//!   [`RendezvousReceipt`] that `mint_rendezvous` re-checks. A node is only
//!   paid for rendezvous work its engine actually reported (see
//!   [`crate::seed`]).
//!
//! Minting is **rate-limited by real network traffic flow**, not compute:
//! every credit consumes `MintPolicy::traffic_per_credit` bytes of *verified*
//! exchange traffic recorded on the [`TrafficMeter`] — an ASIC farm that
//! fabricates service proofs faster than it moves bytes across the network
//! hits the floor. Per-service cooldowns spread work over time (proof of
//! time), and a per-epoch cap bounds absolute issuance.

mod ledger;
mod meter;
mod minter;
mod policy;
mod receipts;
mod signing;

#[cfg(test)]
mod tests;

pub use ledger::{CreditEntry, CreditLedger, LedgerError};
pub use meter::TrafficMeter;
pub use minter::CreditMinter;
pub use policy::{MintError, MintPolicy, ServiceKind};
pub use receipts::{
    canonical_term_hash, IndexReceipt, QueryReceipt, RelayReceipt, RendezvousReceipt,
};
pub(crate) use signing::index_sign_message;
