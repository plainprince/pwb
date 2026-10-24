//! Relay credits for onion cells (`pwb-node::relay`).
//!
//! [`NodeEngine`] reports each cell it actually forwards to a registered next
//! hop as a [`RelayEvent`]. This service folds those cells into the
//! [`CreditMinter`]'s relay hash chain, seals a window into a signed
//! [`RelayReceipt`], and mints against that sealed window exactly once. The
//! minted amount is appended to the node's signed credit ledger as a `relay`
//! earning, so relayed cells are spendable just like served queries and indexed
//! bundles.
//!
//! The mint policy bounds issuance by the bytes actually relayed
//! (`relay_credits_per_kib`), requires the per-service cooldown, and enforces
//! the anti-ASIC traffic floor. A cell cannot mint unless it was peeled *and*
//! queued for a next hop that resolves to a known peer; exit delivery is not a
//! relay role and earns nothing, so sealing a well-formed cell to this node (or
//! naming an unknown next hop) mints no credit.

use std::sync::{Arc, Mutex};

use pwb_core::crypto::Keypair;
use pwb_core::net::NodeEngine;

use crate::credit::{CreditLedger, CreditMinter, MintError, MintPolicy, RelayReceipt};

/// Meters relayed cells and mints relay credits into the shared ledger.
pub struct RelayCreditService {
    engine: NodeEngine,
    ledger: Arc<CreditLedger>,
    minter: Mutex<CreditMinter>,
    sealed: Mutex<Option<RelayReceipt>>,
}

impl RelayCreditService {
    /// Service that mints for `engine`'s relay work under `policy`.
    pub fn new(
        engine: NodeEngine,
        keypair: &Keypair,
        ledger: Arc<CreditLedger>,
        policy: MintPolicy,
    ) -> Self {
        Self {
            engine,
            ledger,
            minter: Mutex::new(CreditMinter::new(keypair.public, policy)),
            sealed: Mutex::new(None),
        }
    }

    /// Minter with [`MintPolicy::default`].
    pub fn with_defaults(engine: NodeEngine, keypair: &Keypair, ledger: Arc<CreditLedger>) -> Self {
        Self::new(engine, keypair, ledger, MintPolicy::default())
    }

    /// Total relay credits minted into the ledger.
    pub fn credits_earned(&self) -> u64 {
        self.minter.lock().unwrap().total_earned()
    }

    /// Account one relayed cell and return the newly minted credits (0 when
    /// the cooldown, traffic floor, or epoch cap withheld the mint).
    pub fn record_cell(&self, cell: &[u8], relayer: &Keypair) -> u64 {
        let mut minter = self.minter.lock().unwrap();
        minter.relay_cell(cell);

        let mut sealed = self.sealed.lock().unwrap();
        // Relay credits are whole-KiB: wait until the live window covers at
        // least one KiB before sealing, otherwise a 512-byte window would mint
        // zero and clear, and credits could never accumulate.
        if sealed.is_none() && minter.pending_relay_bytes() >= 1024 {
            *sealed = minter.seal_relay_window(relayer);
        }
        let Some(receipt) = sealed.clone() else {
            return 0;
        };
        match minter.mint_relay(&receipt, std::time::Instant::now()) {
            Ok(amount) => {
                *sealed = None;
                if amount > 0 {
                    self.ledger.record_earned(
                        "relay",
                        amount,
                        &format!("relayed {} bytes", receipt.bytes),
                    );
                }
                amount
            }
            Err(MintError::Cooldown { .. }) | Err(MintError::RateLimited { .. }) => 0,
            Err(_) => {
                // A receipt that no longer matches the sealed window is stale;
                // drop it rather than retrying forever.
                *sealed = None;
                0
            }
        }
    }

    /// Run until the engine stops, crediting every relayed onion cell.
    pub async fn run(self: Arc<Self>, relayer: Keypair) {
        let mut events = self.engine.subscribe_relay();
        while let Some(event) = events.recv().await {
            let _ = self.record_cell(&event.cell, &relayer);
        }
    }
}
