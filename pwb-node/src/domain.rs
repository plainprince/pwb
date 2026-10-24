//! Sparse Credit & Domain Name Registry (`pwb-node::domain`).
//!
//! Note: The `.plain` TLD is the reserved official namespace (`search.plain`,
//! `register.plain`, `gossip.plain`, `nodes.plain`, plus the shipped content
//! pages). Unofficial network sites claim `.pwb` domain names.
//! The canonical reserved-domain list and `.plain` security rule live in
//! `pwb-core::system`; they are re-exported here for compatibility.

mod claim;
mod pricing;
mod registry;
mod service;

#[cfg(test)]
mod tests;

pub use pwb_core::system::{is_reserved_system_domain, RESERVED_SYSTEM_DOMAINS};

pub use pricing::{
    PricingEpochStats, RollingPricing, INITIAL_CLAIM_PRICE, MAX_CLAIM_PRICE, MIN_CLAIM_PRICE,
    PRICING_WINDOW,
};
pub use registry::{ClaimFunding, DomainRecord, DomainRegistry};
pub use service::{ClaimError, DomainService};

use pwb_core::net::DomainRecordWire;

impl From<&DomainRecord> for DomainRecordWire {
    fn from(record: &DomainRecord) -> Self {
        DomainRecordWire {
            domain: record.domain.clone(),
            target_content_id: record.target_content_id,
            owner: record.owner,
            credits_spent: record.credits_spent,
            registered_at_epoch: record.registered_at_epoch,
            signature: record.signature,
            funding_head: record.funding.map(|funding| funding.head),
            funding_balance: record.funding.map(|funding| funding.balance),
        }
    }
}

impl From<DomainRecord> for DomainRecordWire {
    fn from(record: DomainRecord) -> Self {
        DomainRecordWire::from(&record)
    }
}

impl From<DomainRecordWire> for DomainRecord {
    fn from(record: DomainRecordWire) -> Self {
        // A half-present citation carries no verifiable statement; the
        // record still converts, but `DomainRecordWire::verify` rejects it
        // so sync and resolution never serve or accept it.
        let funding = match (record.funding_head, record.funding_balance) {
            (Some(head), Some(balance)) => Some(ClaimFunding { head, balance }),
            _ => None,
        };
        DomainRecord {
            domain: record.domain,
            target_content_id: record.target_content_id,
            owner: record.owner,
            credits_spent: record.credits_spent,
            registered_at_epoch: record.registered_at_epoch,
            signature: record.signature,
            funding,
        }
    }
}
