//! Sparse Credit & Domain Name Registry (`pwb-node::domain`).
//!
//! Note: The `.plain` TLD is strictly reserved for built-in system protocols (`search.plain`,
//! `register.plain`, `gossip.plain`, `nodes.plain`). User network sites claim `.pwb` domain names.
//! The canonical reserved-domain list and `.plain` security rule live in
//! `pwb-core::system`; they are re-exported here for compatibility.

use pwb_core::crypto::{ContentId, PublicKey};
use std::collections::HashMap;

pub use pwb_core::system::{is_reserved_system_domain, RESERVED_SYSTEM_DOMAINS};

pub const DOMAIN_CLAIM_COST: u64 = 50;

#[derive(Debug, Clone)]
pub struct DomainRecord {
    pub domain: String,
    pub target_content_id: ContentId,
    pub owner: PublicKey,
    pub registered_at_epoch: u64,
}

pub struct DomainRegistry {
    records: HashMap<String, DomainRecord>,
}

impl DomainRegistry {
    pub fn new() -> Self {
        Self {
            records: HashMap::new(),
        }
    }

    /// Register a user domain name (e.g. `welcome.pwb`, `mysite.pwb`).
    ///
    /// SECURITY RULE: The `.plain` TLD is strictly reserved for built-in system protocols.
    /// Attempts by network participants to claim `.plain` domains will be rejected!
    pub fn register(
        &mut self,
        domain: &str,
        target: ContentId,
        owner: PublicKey,
    ) -> Result<(), &'static str> {
        let clean_domain = domain.trim().to_lowercase();

        // 1. Enforce `.plain` TLD system reservation security rule
        if is_reserved_system_domain(&clean_domain) {
            return Err(
                "Reserved system domain: .plain TLD is restricted to built-in system protocols",
            );
        }

        // 2. Reject duplicates
        if self.records.contains_key(&clean_domain) {
            return Err("Domain name already registered");
        }

        let record = DomainRecord {
            domain: clean_domain.clone(),
            target_content_id: target,
            owner,
            registered_at_epoch: 1,
        };

        self.records.insert(clean_domain, record);
        Ok(())
    }

    pub fn resolve(&self, domain: &str) -> Option<&DomainRecord> {
        self.records.get(&domain.trim().to_lowercase())
    }

    pub fn count(&self) -> usize {
        self.records.len()
    }
}

impl Default for DomainRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_registry_security_rules() {
        let mut reg = DomainRegistry::new();
        let cid = ContentId([1; 32]);
        let pk = PublicKey([2; 32]);

        // Attempting to claim .plain domain MUST fail!
        assert!(reg.register("search.plain", cid, pk).is_err());
        assert!(reg.register("malicious.plain", cid, pk).is_err());
        assert!(reg.register("hacked.plain", cid, pk).is_err());

        // Registering user domain .pwb MUST succeed!
        assert!(reg.register("welcome.pwb", cid, pk).is_ok());
        assert!(reg.register("welcome.pwb", cid, pk).is_err()); // duplicate check

        let resolved = reg.resolve("WELCOME.PWB").unwrap();
        assert_eq!(resolved.target_content_id, cid);
    }
}
