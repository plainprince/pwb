//! Bundler implementation (`pwb-web::bundle`).

use pwb_core::spec::PlainNode;
use pwb_core::wire::{Bundle, CURRENT_VERSION};

/// Create a `Bundle` from an AST root node, title, optional author public key,
/// script payload, component registry, and named script-module map.
#[allow(clippy::too_many_arguments)]
pub fn create_bundle(
    root: PlainNode,
    title: String,
    author_pubkey: Option<[u8; 32]>,
    script_payload: Option<Vec<u8>>,
    components: std::collections::HashMap<String, PlainNode>,
    modules: std::collections::HashMap<String, Vec<u8>>,
) -> Bundle {
    Bundle {
        version: CURRENT_VERSION,
        title,
        author_pubkey,
        root_node: root,
        script_payload,
        components,
        modules,
    }
}
