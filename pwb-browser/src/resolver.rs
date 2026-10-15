/// System Domain and URI Resolver (`pwb-browser::resolver`).
///
/// Security Rule: `.plain` TLD is strictly reserved for built-in system protocols (`search.plain`,
/// `register.plain`, `gossip.plain`, `nodes.plain`). User sites reside under `.pwb` domain names (e.g. `welcome.pwb`).
/// The bare `devtools` domain is a special internal browser tool with no TLD.
///
/// URL matching is centralized in `pwb-core::system` ([`SystemUrl`] /
/// [`SystemDomain`]); this module only picks the native page to render for
/// each recognized domain.
use pwb_core::system::{SystemDomain, SystemUrl};
use pwb_node::domain::DomainRegistry;
use pwb_node::host::ArtifactHost;
use pwb_node::identity::IdentityManager;
use pwb_node::search::SearchEngine;
use pwb_node::system::SystemDomainHandler;
use std::fs;

#[derive(Debug)]
pub enum ResolverError {
    DomainNotFound(String),
    SystemError(String),
    Io(std::io::Error),
}

impl From<std::io::Error> for ResolverError {
    fn from(err: std::io::Error) -> Self {
        ResolverError::Io(err)
    }
}

/// Resolve a URL or domain string to raw `.pwb` binary bundle bytes.
pub fn resolve_url_or_file(
    target: &str,
    search: &SearchEngine,
    host: &ArtifactHost,
    registry: &DomainRegistry,
    identity: &IdentityManager,
) -> Result<Vec<u8>, ResolverError> {
    let clean = target.trim().trim_start_matches("plain://");

    // 1. Built-in static pages (e.g. `welcome`, `welcome.pwb`, `welcome.plain`, `demo`, `demo.pwb`, `devtools`).
    // These live in `webpages/` inside pwb-browser and are compiled on-the-fly via pwb-web.
    if let Some(bytes) = crate::builtin_sites::compile_builtin_page_bytes(clean) {
        return Ok(bytes);
    }

    // 2. Reserved System Domains (.plain TLD) — matching centralized in pwb-core::system.
    if let Some(system_url) = SystemUrl::parse(clean) {
        let handler = SystemDomainHandler::new(search, host, registry, identity);
        let bundle = match system_url.domain() {
            SystemDomain::Search => {
                let query = system_url
                    .search_query()
                    .unwrap_or_else(|| "PlainWeb".to_string());
                handler.render_system_search_page(&query)
            }
            SystemDomain::Register => handler.render_system_register_page(identity.public_key()),
            // The interactive devtools panel normally mounts from `webpages/devtools`
            // (step 1); this is the standalone fallback page.
            SystemDomain::DevTools => handler.render_system_devtools_page(None),
            SystemDomain::Gossip => handler.render_system_gossip_page(),
            // `plain://nodes.plain?connect=<ip:port>/<pubkey>` links render as a
            // native connect card; bare nodes.plain shows the peer registry.
            SystemDomain::Nodes => handler.render_system_nodes_page(system_url.connection_target()),
            SystemDomain::Network | SystemDomain::Help => {
                return Err(ResolverError::SystemError(format!(
                    "{} is a reserved system domain but has no native handler yet",
                    system_url.domain().as_str()
                )));
            }
        };
        return Ok(bundle.serialize());
    }

    // 3. Explicit File System Access (e.g. `file.pwb/path/to/file.pwb`)
    if clean.starts_with("file.pwb/") {
        let path = clean.trim_start_matches("file.pwb/");
        let bytes = fs::read(path)?;
        return Ok(bytes);
    }

    // 4. User Network Domains (.pwb TLD or custom user domain)
    if clean.ends_with(".pwb") || clean.contains('.') {
        if let Some(record) = registry.resolve(clean) {
            if let Some(bundle_bytes) = host.get(&record.target_content_id) {
                return Ok(bundle_bytes.clone());
            }
        }
    }

    // 5. Fallback to local artifact file path (e.g. `path/to/site.pwb`)
    let bytes = fs::read(target)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_domain_resolver_security() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        // Built-in system domain search.plain MUST resolve to system search bundle
        let bytes = resolve_url_or_file(
            "search.plain?q=untrackable",
            &search,
            &host,
            &registry,
            &identity,
        )
        .unwrap();

        assert!(bytes.len() > 50);
        assert_eq!(&bytes[0..4], b"PWB1");
    }

    #[test]
    fn test_welcome_plain_resolves_to_builtin_page() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        // welcome.plain is compiled from webpages/welcome.pwb sources through
        // pwb-web at resolve time (not a hand-built Rust DOM).
        let bytes = resolve_url_or_file("welcome.plain", &search, &host, &registry, &identity)
            .expect("welcome.plain should resolve");
        assert_eq!(&bytes[0..4], b"PWB1");
    }

    #[test]
    fn test_all_native_system_domains_resolve() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        for target in [
            "search.plain?q=untrackable",
            "register.plain",
            "gossip.plain",
            "nodes.plain",
            "devtools.plain",
            // bare aliases and the nodes.plain connection-link form
            "search?q=plainweb",
            "plain://nodes.plain?connect=127.0.0.1:9000/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            let bytes = resolve_url_or_file(target, &search, &host, &registry, &identity)
                .unwrap_or_else(|e| panic!("{target} should resolve: {e:?}"));
            assert!(
                bytes.len() > 50 && &bytes[0..4] == b"PWB1",
                "{target} should produce a PWB1 bundle"
            );
        }
    }

    #[test]
    fn test_reserved_domains_without_handlers_error_clearly() {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let identity = IdentityManager::new();

        // network.plain / help.plain are reserved but have no native handler:
        // they must fail with a clear system error, not a filesystem miss.
        for target in ["network.plain", "help.plain"] {
            let err = resolve_url_or_file(target, &search, &host, &registry, &identity)
                .expect_err("reserved unhandled domain should error");
            assert!(
                matches!(err, ResolverError::SystemError(_)),
                "{target} should be a SystemError, got {err:?}"
            );
        }
    }
}
