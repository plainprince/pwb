//! Unified System Protocol Resolver (`pwb-core::system`).
//!
//! PlainWeb replaces traditional centralized web services with reserved native
//! system domains. The `.plain` TLD is the **official** namespace (reserved
//! system domains plus browser-shipped content pages); `.pwb` is the
//! **unofficial** user namespace. This module is the single source of truth for:
//! - the reserved system domain list and the `.plain` TLD security rule,
//! - parsing `plain://` URLs into a [`SystemDomain`] + query parameters,
//! - the `nodes.plain` connection-link codec
//!   (`plain://nodes.plain?connect=<ip:port>/<pubkey_hex>`).
//!
//! The native system domains are:
//! - `search.plain`  — decentralized search engine API.
//! - `devtools.plain`— built-in developer tools (DOM & style inspector).
//! - `register.plain`— domain claim registry (sparse credit ledger).
//! - `gossip.plain`  — P2P artifact gossip and index propagation.
//! - `nodes.plain`   — peer node routing, health registry, and connection links.
//! - `network.plain` — this node's addresses, peer table, and local services.
//! - `help.plain`    — system-domain index plus namespace and credit rules.
//!
//! Page *rendering* for these domains lives in `pwb-node::system` (it needs
//! `pwb-web`'s PML/PSS compiler, which `pwb-core` cannot depend on). Everything
//! here is pure protocol: URL matching, parameter extraction, and link
//! encoding/decoding shared by `pwb-node` and `pwb-browser`.

use crate::crypto::PublicKey;
use std::net::SocketAddr;
use std::sync::OnceLock;

/// Reserved system domain names in the PlainWeb network.
pub const RESERVED_SYSTEM_DOMAINS: &[&str] = &[
    "search.plain",
    "register.plain",
    "gossip.plain",
    "nodes.plain",
    "network.plain",
    "help.plain",
    // Internal browser tools: no TLD, blocked so user sites cannot claim them.
    "devtools",
];

/// A native PlainWeb system domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemDomain {
    /// `search.plain` — decentralized search engine API.
    Search,
    /// `devtools.plain` (and bare `devtools`) — developer tools inspector.
    DevTools,
    /// `register.plain` — domain claim registry.
    Register,
    /// `gossip.plain` — artifact gossip propagation.
    Gossip,
    /// `nodes.plain` — peer registry and connection links.
    Nodes,
    /// `network.plain` — live network status (addresses, peers, services).
    Network,
    /// `help.plain` — usage help and the system-domain index.
    Help,
}

impl SystemDomain {
    /// Canonical lowercase host name of the domain (with the `.plain` TLD).
    pub fn as_str(self) -> &'static str {
        match self {
            SystemDomain::Search => "search.plain",
            SystemDomain::DevTools => "devtools.plain",
            SystemDomain::Register => "register.plain",
            SystemDomain::Gossip => "gossip.plain",
            SystemDomain::Nodes => "nodes.plain",
            SystemDomain::Network => "network.plain",
            SystemDomain::Help => "help.plain",
        }
    }

    /// Map a URL host (before `?` or `/`, case-insensitive) to a system domain.
    ///
    /// `search` and `devtools` are accepted bare (no TLD) for compatibility
    /// with the historical `search?` / `devtools` aliases.
    pub fn from_host(host: &str) -> Option<Self> {
        match host.trim().to_lowercase().as_str() {
            "search.plain" | "search" => Some(SystemDomain::Search),
            "devtools.plain" | "devtools" => Some(SystemDomain::DevTools),
            "register.plain" => Some(SystemDomain::Register),
            "gossip.plain" => Some(SystemDomain::Gossip),
            "nodes.plain" => Some(SystemDomain::Nodes),
            "network.plain" => Some(SystemDomain::Network),
            "help.plain" => Some(SystemDomain::Help),
            _ => None,
        }
    }
}

/// A parsed system-domain URL: the [`SystemDomain`] plus its raw query string
/// (without the leading `?`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemUrl {
    domain: SystemDomain,
    query: String,
}

impl SystemUrl {
    /// Parse a target string (with or without the `plain://` scheme) into a
    /// system URL. Returns `None` when the host is not a native system domain
    /// (including unknown `.plain` hosts, which stay reserved for the registry
    /// security rule).
    pub fn parse(target: &str) -> Option<Self> {
        let clean = target.trim().trim_start_matches("plain://");
        let (host, query) = if let Some((h, q)) = clean.split_once('?') {
            (h, q.to_string())
        } else if let Some((h, _path)) = clean.split_once('/') {
            (h, String::new())
        } else {
            (clean, String::new())
        };
        let domain = SystemDomain::from_host(host)?;
        Some(Self { domain, query })
    }

    /// The resolved system domain.
    pub fn domain(&self) -> SystemDomain {
        self.domain
    }

    /// The raw query string (without the leading `?`); empty when absent.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Look up a single `&`-separated query parameter by key.
    pub fn param(&self, key: &str) -> Option<String> {
        self.query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == key).then(|| v.to_string())
        })
    }

    /// The `q=` search query, when present.
    pub fn search_query(&self) -> Option<String> {
        self.param("q")
    }

    /// The `connect=` target of a `nodes.plain` link, parsed into
    /// `(SocketAddr, PublicKey)`. `None` for other domains or malformed targets.
    pub fn connection_target(&self) -> Option<(SocketAddr, PublicKey)> {
        if self.domain != SystemDomain::Nodes {
            return None;
        }
        let target = self.param("connect")?;
        let (addr_part, pubkey_hex) = target.rsplit_once('/')?;
        let addr: SocketAddr = addr_part.parse().ok()?;
        let pubkey_bytes = hex::decode(pubkey_hex).ok()?;
        if pubkey_bytes.len() != 32 {
            return None;
        }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&pubkey_bytes);
        Some((addr, PublicKey(bytes)))
    }
}

/// Check if a domain name falls under the reserved `.plain` system TLD (or is
/// a reserved no-TLD system name such as `devtools`).
pub fn is_reserved_system_domain(domain: &str) -> bool {
    let clean = domain.trim().to_lowercase();
    let domain_part = clean.split('?').next().unwrap_or(&clean);
    domain_part.ends_with(".plain") || RESERVED_SYSTEM_DOMAINS.contains(&domain_part)
}

/// The **unofficial** namespace: every TLD a participant may claim on
/// `register.plain`. `.plain` stays reserved for the official system pages;
/// anything not listed here is not part of PlainWeb's namespace at all.
///
/// The full list with per-TLD notes lives in `docs/tlds.md` — keep the two in
/// sync when adding a TLD.
pub const UNOFFICIAL_TLDS: &[&str] = &[
    // PlainWeb's own unofficial namespace (the original).
    "pwb", // Playful / purpose-flavoured namespaces.
    "non", "yep", "lam", "arch",
    // The legacy trio, resolvable inside PlainWeb like any other name here.
    "org", "net", "com",
];

/// The unofficial TLD `name` ends in (lowercased input, matched case-insensitively),
/// or `None` when the name carries no listed TLD — including `.plain` names and
/// bare names without a dot.
pub fn unofficial_tld(name: &str) -> Option<&'static str> {
    let lower = name.trim().to_lowercase();
    UNOFFICIAL_TLDS.iter().copied().find(|tld| {
        lower
            .strip_suffix(&format!(".{tld}"))
            .is_some_and(|label| !label.is_empty())
    })
}

/// Whether `name` is a claimable name of the unofficial namespace: it ends in
/// one of [`UNOFFICIAL_TLDS`] with a non-empty label in front of the dot.
///
/// Reserved `.plain` names never pass this check either — `.plain` is not in
/// the list — but callers should still report the reserved rule first, since
/// it is the more specific error.
pub fn is_unofficial_domain(name: &str) -> bool {
    unofficial_tld(name).is_some()
}

/// The shared error text for a name outside the unofficial namespace, built
/// once from [`UNOFFICIAL_TLDS`] so messages never drift from the list.
pub fn unknown_tld_message() -> &'static str {
    static MESSAGE: OnceLock<String> = OnceLock::new();
    MESSAGE
        .get_or_init(|| {
            let list = UNOFFICIAL_TLDS
                .iter()
                .map(|tld| format!(".{tld}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("unknown TLD: names must end in one of {list}")
        })
        .as_str()
}

/// Build a `nodes.plain` connection link from an address and public key.
///
/// Format: `plain://nodes.plain?connect=<ip:port>/<pubkey_hex>`
pub fn build_connection_link(listen_addr: SocketAddr, pubkey: PublicKey) -> String {
    format!(
        "plain://nodes.plain?connect={}/{}",
        listen_addr,
        pubkey.to_hex()
    )
}

/// Parse a `nodes.plain` connection link into `(SocketAddr, PublicKey)`.
///
/// Returns `None` for non-`nodes.plain` hosts, foreign schemes, or malformed
/// address/pubkey targets.
pub fn parse_connection_link(link: &str) -> Option<(SocketAddr, PublicKey)> {
    SystemUrl::parse(link)?.connection_target()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::PublicKey;

    #[test]
    fn test_reserved_system_domain_security_rule() {
        assert!(is_reserved_system_domain("search.plain"));
        assert!(is_reserved_system_domain("malicious.plain"));
        assert!(is_reserved_system_domain("hacked.plain"));
        assert!(is_reserved_system_domain("search.plain?q=untrackable"));
        assert!(is_reserved_system_domain("devtools"));
        assert!(!is_reserved_system_domain("welcome.pwb"));
        assert!(!is_reserved_system_domain("mysite.pwb"));
    }

    #[test]
    fn test_unofficial_tld_list_recognizes_every_listed_tld() {
        for tld in UNOFFICIAL_TLDS {
            let name = format!("site.{tld}");
            assert_eq!(unofficial_tld(&name), Some(*tld), "{name} is claimable");
            assert!(is_unofficial_domain(&name));
            // Case-insensitive and whitespace-tolerant, like the registry.
            assert!(is_unofficial_domain(&format!(
                "  SITE.{}  ",
                tld.to_uppercase()
            )));
            // A label must precede the dot: the bare TLD is not a name.
            assert!(!is_unofficial_domain(&format!(".{tld}")));
        }
        // Nested labels stay claimable (matching the previous `.pwb` rule).
        assert!(is_unofficial_domain("a.b.com"));
    }

    #[test]
    fn test_non_namespace_names_are_not_unofficial_domains() {
        for name in [
            "search.plain",
            "welcome.plain",
            "devtools",
            "mysite.xyz", // valid-looking but not on the list
            "no-tld-at-all",
            "",
            ".",
        ] {
            assert!(!is_unofficial_domain(name), "{name} must not be claimable");
            assert_eq!(unofficial_tld(name), None);
        }
        // `.plain` is reserved, not merely "another TLD".
        assert!(!is_reserved_system_domain("site.non"));
        assert!(is_reserved_system_domain("site.plain"));
    }

    #[test]
    fn test_unknown_tld_message_lists_every_tld() {
        let message = unknown_tld_message();
        for tld in UNOFFICIAL_TLDS {
            assert!(
                message.contains(&format!(".{tld}")),
                "message must mention .{tld}: {message}"
            );
        }
        assert!(message.starts_with("unknown TLD"));
    }

    #[test]
    fn test_system_url_parse_search_query() {
        let url = SystemUrl::parse("plain://search.plain?q=untrackable").expect("search url");
        assert_eq!(url.domain(), SystemDomain::Search);
        assert_eq!(url.query(), "q=untrackable");
        assert_eq!(url.search_query().as_deref(), Some("untrackable"));

        // Scheme-less and bare-alias forms resolve too.
        let url = SystemUrl::parse("search.plain?q=plainweb").expect("scheme-less");
        assert_eq!(url.domain(), SystemDomain::Search);
        let url = SystemUrl::parse("search?q=plainweb").expect("bare alias");
        assert_eq!(url.domain(), SystemDomain::Search);

        // No q= falls back to None (caller supplies the default).
        let url = SystemUrl::parse("search.plain").expect("no query");
        assert_eq!(url.search_query(), None);

        // Extra params don't leak into the query value.
        let url = SystemUrl::parse("search.plain?q=hello&lang=en").expect("multi param");
        assert_eq!(url.search_query().as_deref(), Some("hello"));
    }

    #[test]
    fn test_system_url_parse_all_native_domains() {
        for (host, domain) in [
            ("search.plain", SystemDomain::Search),
            ("devtools.plain", SystemDomain::DevTools),
            ("devtools", SystemDomain::DevTools),
            ("register.plain", SystemDomain::Register),
            ("gossip.plain", SystemDomain::Gossip),
            ("nodes.plain", SystemDomain::Nodes),
            ("network.plain", SystemDomain::Network),
            ("help.plain", SystemDomain::Help),
        ] {
            let url = SystemUrl::parse(host).unwrap_or_else(|| panic!("{host} should parse"));
            assert_eq!(url.domain(), domain);
            assert!(domain.as_str().ends_with(".plain"));
        }

        // Unknown hosts — including unknown `.plain` hosts — are not system URLs.
        assert!(SystemUrl::parse("welcome.plain").is_none());
        assert!(SystemUrl::parse("welcome.pwb").is_none());
        assert!(SystemUrl::parse("file.pwb/path/to/file.pwb").is_none());
        assert!(SystemUrl::parse("plain://unknown.plain?q=1").is_none());
    }

    #[test]
    fn test_connection_link_roundtrip() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let pk = PublicKey([7u8; 32]);
        let link = build_connection_link(addr, pk);
        assert!(link.starts_with("plain://nodes.plain?connect="));

        let parsed = parse_connection_link(&link).expect("link should parse");
        assert_eq!(parsed.0, addr);
        assert_eq!(parsed.1, pk);

        // Scheme-less form parses too.
        let parsed = parse_connection_link(link.trim_start_matches("plain://"))
            .expect("scheme-less link should parse");
        assert_eq!(parsed.0, addr);
        assert_eq!(parsed.1, pk);
    }

    #[test]
    fn test_connection_link_rejects_invalid() {
        let hex64 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(parse_connection_link("").is_none());
        assert!(parse_connection_link("plain://nodes.plain?connect=").is_none());
        assert!(parse_connection_link("plain://nodes.plain?connect=127.0.0.1:8080/").is_none());
        assert!(
            parse_connection_link("plain://nodes.plain?connect=127.0.0.1:8080/short").is_none()
        );
        assert!(parse_connection_link(&format!(
            "wrong://nodes.plain?connect=127.0.0.1:8080/{}",
            hex64
        ))
        .is_none());
        assert!(parse_connection_link(&format!(
            "plain://search.plain?connect=127.0.0.1:8080/{}",
            hex64
        ))
        .is_none());
        // Unparsable address → None (the addr parse failure path).
        assert!(parse_connection_link(&format!(
            "plain://nodes.plain?connect=not-an-addr/{}",
            hex64
        ))
        .is_none());
    }

    #[test]
    fn test_connection_target_requires_nodes_domain() {
        // A valid-looking connect= on a non-nodes domain is ignored.
        let hex64 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let url = SystemUrl::parse(&format!(
            "plain://gossip.plain?connect=127.0.0.1:8080/{}",
            hex64
        ))
        .expect("gossip url");
        assert_eq!(url.connection_target(), None);
    }
}
