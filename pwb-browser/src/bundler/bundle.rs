//! `.pwb` bundle construction from a source directory.
//!
//! This is a thin wrapper over the shared `pwb-web` compiler so the browser's
//! built-in pages, the `pwb-web` CLI, and `pwb-node --host` all parse PML/PSS/JS
//! with exactly the same parser.

use crate::bundler::error::BundlerError;
use pwb_core::spec::PlainNode;
use pwb_core::wire::Bundle;
use std::path::Path;

/// Node ids for a bundler-built subtree start here so they can never collide
/// with ids decoded from the main page bundle (which start small). Devtools is
/// mounted as a child of the page DOM, so its ids must be in a separate range.
const BUNDLED_ID_OFFSET: usize = 100_000;

/// Create a PlainWeb bundle from a source directory.
pub fn bundle_from_dir(source_dir: &Path) -> Result<Bundle, BundlerError> {
    let mut parsed = pwb_web::parse::parse_site_directory(source_dir).map_err(|e| {
        BundlerError::InvalidManifest(format!("failed to parse {}: {:?}", source_dir.display(), e))
    })?;

    offset_ids(&mut parsed.root_node);
    for body in parsed.components.values_mut() {
        offset_ids(body);
    }

    Ok(pwb_web::bundle::create_bundle(
        parsed.root_node,
        parsed.title,
        None,
        parsed.script_payload,
        parsed.components,
        parsed.modules,
    ))
}

fn offset_ids(node: &mut PlainNode) {
    node.id += BUNDLED_ID_OFFSET;
    for child in &mut node.children {
        offset_ids(child);
    }
}
