//! Builtin pages (`pwb-browser::builtin_sites`).
//!
//! Builtin pages are authored as normal site sources and compiled at runtime,
//! routed through `pwb-core` (spec/wire types) into `pwb-web` (the PML/PSS/JS
//! compiler) — the same pipeline `pwb-web`'s CLI uses.
//!
//! Official, browser-shipped pages live on the reserved `.plain` TLD: regular
//! builtin content pages (e.g. `welcome.plain`) live in
//! `webpages/<name>.plain/` containing `index.pml`, `style.pss`, and `app.pjs`.
//!
//! Special system pages (e.g. `devtools`) live in `webpages/<name>/` **without**
//! a TLD suffix, reflecting that they are internal browser tools, not
//! user-facing network sites. `.pwb` is reserved for *unofficial* sites created
//! and claimed by network participants.

use crate::dom::Dom;
use std::path::Path;

/// Compile a builtin page directory (`webpages/<name>.pwb`) into a DOM via
/// `pwb-web`. Returns `None` if the page or its sources are missing.
pub fn compile_builtin_page(name: &str) -> Option<Dom> {
    let dir = builtin_page_dir(name)?;
    let parsed = pwb_web::parse::parse_site_directory(&dir).ok()?;
    let bundle = pwb_web::bundle::create_bundle(
        parsed.root_node,
        parsed.title,
        None,
        parsed.script_payload,
        parsed.components,
        parsed.modules,
    );
    Some(Dom::from_bundle(&bundle))
}

/// Serialized bytes for a builtin page, suitable for `resolve_url_or_file`
/// (which hands them to the browser pipeline as a `.pwb` artifact).
pub fn compile_builtin_page_bytes(name: &str) -> Option<Vec<u8>> {
    let dir = builtin_page_dir(name)?;
    let parsed = pwb_web::parse::parse_site_directory(&dir).ok()?;
    let bundle = pwb_web::bundle::create_bundle(
        parsed.root_node,
        parsed.title,
        None,
        parsed.script_payload,
        parsed.components,
        parsed.modules,
    );
    Some(pwb_web::compress::compress_bundle(&bundle))
}

/// Locate the source directory for a builtin page.
///
/// Automatically matches any directory inside `webpages/` matching any of:
/// 1. Exact input name (e.g. `devtools`, `welcome.plain`)
/// 2. Stem with the official `.plain` suffix (e.g. `welcome` -> `welcome.plain`)
/// 3. Stem with a legacy `.pwb` suffix (e.g. `welcome` -> `welcome.pwb`)
/// 4. Bare stem (e.g. `devtools.plain` -> `devtools`)
///
/// Searches relative to the current working directory first (runtime), then
/// relative to this crate's manifest directory (for tests / `cargo run`).
fn builtin_page_dir(input: &str) -> Option<std::path::PathBuf> {
    let clean = input.trim().trim_start_matches("plain://");

    // Extract stem by stripping TLDs if present
    let stem = clean
        .strip_suffix(".plain")
        .or_else(|| clean.strip_suffix(".pwb"))
        .unwrap_or(clean);

    // Candidates to search for inside `webpages/`
    let candidates = [
        clean.to_string(),
        format!("{stem}.plain"),
        format!("{stem}.pwb"),
        stem.to_string(),
    ];

    for candidate in &candidates {
        let rel = Path::new("webpages").join(candidate);
        if rel.is_dir() {
            return Some(rel);
        }
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("webpages")
            .join(candidate);
        if manifest.is_dir() {
            return Some(manifest);
        }
    }

    None
}

pub fn create_builtin_site(domain: &str) -> Option<Dom> {
    compile_builtin_page(domain)
}

pub fn is_builtin_site(domain: &str) -> bool {
    builtin_page_dir(domain).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compile_builtin_welcome_page() {
        // Compiles the real `webpages/welcome.plain/` sources through pwb-web
        // (pwb-core spec → pwb-web compiler → pwb-browser DOM).
        let dom = compile_builtin_page("welcome").expect("welcome page should compile");
        assert_eq!(dom.title, "Welcome to PlainWeb");

        fn find_btn(node: &crate::dom::DomNode) -> Option<&crate::dom::DomNode> {
            if node.tag == pwb_core::spec::Tag::Button
                && node.attributes.get("id").map(|s| s.as_str()) == Some("ctx-btn")
            {
                return Some(node);
            }
            node.children.iter().find_map(find_btn)
        }
        assert!(
            find_btn(&dom.root).is_some(),
            "welcome page must include #ctx-btn"
        );

        // The shipped page demonstrates native link navigation: the anchor is
        // a real Tag::Link with a resolvable href (no JS required).
        fn find_link(node: &crate::dom::DomNode) -> Option<String> {
            if node.tag == pwb_core::spec::Tag::Link {
                return node.attributes.get("href").cloned();
            }
            node.children.iter().find_map(find_link)
        }
        assert_eq!(
            find_link(&dom.root).as_deref(),
            Some("demo.plain"),
            "welcome page must expose a native href link"
        );

        let bytes = compile_builtin_page_bytes("welcome").expect("welcome bytes");
        assert!(
            pwb_core::wire::Bundle::is_compressed(&bytes),
            "builtin pages ship in the compressed PWBZ container"
        );
        let decoded = pwb_core::wire::Bundle::deserialize(&bytes).expect("compressed page decodes");
        assert_eq!(decoded.title, "Welcome to PlainWeb");

        assert!(is_builtin_site("welcome"));
        assert!(is_builtin_site("welcome.plain"));
        assert!(!is_builtin_site("welcome.com"));
    }
}
