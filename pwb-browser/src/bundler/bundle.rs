//! `.pwb` bundle construction from a source directory.

use crate::bundler::error::BundlerError;
use crate::bundler::parser::parse_pml;
use crate::bundler::pss::{apply_stylesheet_attrs, parse_pss_to_attrs};
use pwb_core::wire::Bundle;
use std::fs;
use std::path::Path;

/// Create a simple PlainWeb bundle from a devtools directory.
pub fn bundle_from_dir(source_dir: &Path) -> Result<Bundle, BundlerError> {
    // Read the PML markup file
    let pml_path = source_dir.join("index.pml");
    if !pml_path.exists() {
        return Err(BundlerError::InvalidManifest(
            "index.pml not found".to_string(),
        ));
    }
    let pml_content = fs::read_to_string(pml_path)?;

    // Parse PML into DOM nodes (simplified parser)
    let mut root_node = parse_pml(&pml_content);

    // Apply PSS stylesheet rules (flattened into attributes so the runtime style
    // engine picks them up).
    let pss_path = source_dir.join("style.pss");
    if pss_path.exists() {
        if let Ok(pss_content) = fs::read_to_string(pss_path) {
            let rules = parse_pss_to_attrs(&pss_content);
            apply_stylesheet_attrs(&mut root_node, &rules);
        }
    }

    // Read the JS script if present
    let mut script_payload = None;
    let pjs_path = source_dir.join("app.pjs");
    if pjs_path.exists() {
        script_payload = Some(fs::read(pjs_path)?);
    }

    Ok(Bundle {
        version: pwb_core::wire::CURRENT_VERSION,
        title: "DevTools".to_string(),
        author_pubkey: None,
        root_node,
        script_payload,
        components: std::collections::HashMap::new(),
        modules: std::collections::HashMap::new(),
    })
}
