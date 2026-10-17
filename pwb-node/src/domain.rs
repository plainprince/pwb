//! Sparse Credit & Domain Name Registry (`pwb-node::domain`).
//!
//! Note: The `.plain` TLD is strictly reserved for built-in system protocols (`search.plain`,
//! `register.plain`, `gossip.plain`, `nodes.plain`). User network sites claim `.pwb` domain names.
//! The canonical reserved-domain list and `.plain` security rule live in
//! `pwb-core::system`; they are re-exported here for compatibility.

mod claim;
mod pricing;
mod registry;

#[cfg(test)]
mod tests;

pub use pwb_core::system::{is_reserved_system_domain, RESERVED_SYSTEM_DOMAINS};

pub use pricing::{
    PricingEpochStats, RollingPricing, INITIAL_CLAIM_PRICE, MAX_CLAIM_PRICE, MIN_CLAIM_PRICE,
    PRICING_WINDOW,
};
pub use registry::{DomainRecord, DomainRegistry};
