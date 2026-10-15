//! Identity & Credit Ledger management (`pwb-node::identity`).

use pwb_core::crypto::{Keypair, PublicKey, Signature};

#[derive(Debug, Clone)]
pub struct Account {
    pub pubkey: PublicKey,
    pub credits: u64,
    pub indexing_score: u64,
}

pub struct IdentityManager {
    pub keypair: Keypair,
    pub credits: u64,
}

impl IdentityManager {
    pub fn new() -> Self {
        Self {
            keypair: Keypair::generate(),
            credits: 100, // Initial credit balance for testing
        }
    }

    pub fn public_key(&self) -> PublicKey {
        self.keypair.public
    }

    pub fn sign_message(&self, message: &[u8]) -> Signature {
        self.keypair.sign(message)
    }

    pub fn add_credits(&mut self, amount: u64) {
        self.credits += amount;
    }

    pub fn spend_credits(&mut self, amount: u64) -> Result<(), &'static str> {
        if self.credits >= amount {
            self.credits -= amount;
            Ok(())
        } else {
            Err("Insufficient credit balance")
        }
    }
}

impl Default for IdentityManager {
    fn default() -> Self {
        Self::new()
    }
}
