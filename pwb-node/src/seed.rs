//! Seed (rendezvous) credits (`pwb-node::seed`).
//!
//! A seed is a long-running node that admits newcomers and hands out its peer
//! table. Both are work the engine already reports as a
//! [`RendezvousEvent`]; this service folds those events into the
//! [`CreditMinter`]'s rendezvous chain, seals a window into a signed
//! [`RendezvousReceipt`], and mints against that sealed window exactly once, so
//! a seed earns credits the same way a relay, a search responder, or an indexer
//! does.
//!
//! Only work this node actually performed is metered: `JoinServed` is emitted
//! when a peer authenticates to it for the first time, and `PeerTableServed`
//! carries the table bytes a gossip round really sent. A node that is merely
//! reachable mints nothing, and issuance stays bounded by the same traffic
//! floor, per-service cooldown, and epoch cap as every other service.

use std::sync::{Arc, Mutex};

use pwb_core::crypto::Keypair;
use pwb_core::net::{NodeEngine, RendezvousEvent};

use crate::credit::{CreditLedger, CreditMinter, MintError, MintPolicy, RendezvousReceipt};

/// Meters rendezvous work and mints seed credits into the shared ledger.
pub struct SeedCreditService {
    engine: NodeEngine,
    ledger: Arc<CreditLedger>,
    minter: Mutex<CreditMinter>,
    sealed: Mutex<Option<RendezvousReceipt>>,
}

impl SeedCreditService {
    /// Service that mints for `engine`'s rendezvous work under `policy`.
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

    /// Total seed credits minted into the ledger.
    pub fn credits_earned(&self) -> u64 {
        self.minter.lock().unwrap().total_earned()
    }

    /// Account one rendezvous event and return the newly minted credits (0 when
    /// the window is not worth sealing yet, or when the cooldown, traffic
    /// floor, or epoch cap withheld the mint).
    pub fn record_event(&self, event: &RendezvousEvent, seed: &Keypair) -> u64 {
        let mut minter = self.minter.lock().unwrap();
        match *event {
            RendezvousEvent::JoinServed { probe_bytes } => minter.rendezvous_join(probe_bytes),
            RendezvousEvent::PeerTableServed { peers, bytes } => {
                minter.record_peer_table_served(peers, bytes);
            }
        }

        let mut sealed = self.sealed.lock().unwrap();
        if sealed.is_none() {
            *sealed = minter.seal_rendezvous_window(seed);
        }
        let Some(receipt) = sealed.clone() else {
            return 0;
        };
        match minter.mint_rendezvous(&receipt, std::time::Instant::now()) {
            Ok(amount) => {
                *sealed = None;
                if amount > 0 {
                    self.ledger.record_earned(
                        "rendezvous",
                        amount,
                        &format!(
                            "served {} newcomer(s) and {} peer-table bytes",
                            receipt.joins, receipt.peer_bytes
                        ),
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

    /// Run until the engine stops, crediting every newcomer admitted and every
    /// peer table served.
    pub async fn run(self: Arc<Self>, seed: Keypair) {
        let mut events = self.engine.subscribe_rendezvous();
        while let Some(event) = events.recv().await {
            let _ = self.record_event(&event, &seed);
        }
    }
}
