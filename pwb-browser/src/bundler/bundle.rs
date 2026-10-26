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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const INDEX: &str = r#"<plain>
    <head><title>Test Site</title></head>
    <component name="Card"><view><text>card body</text></view></component>
    <view class="hero"><text class="heading">Hello</text></view>
</plain>"#;

    fn site_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pwb-bundler-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn collect_ids(node: &PlainNode, out: &mut Vec<usize>) {
        out.push(node.id);
        for child in &node.children {
            collect_ids(child, out);
        }
    }

    #[test]
    fn bundle_from_dir_offsets_every_id_including_components() {
        let dir = site_dir("ok");
        fs::write(dir.join("index.pml"), INDEX).unwrap();

        let bundle = bundle_from_dir(&dir).expect("site bundles");
        assert_eq!(bundle.title, "Test Site");

        let mut ids = Vec::new();
        collect_ids(&bundle.root_node, &mut ids);
        assert!(!ids.is_empty());
        assert!(
            ids.iter().all(|id| *id >= BUNDLED_ID_OFFSET),
            "page ids live in the bundler range: {ids:?}"
        );

        // Component bodies mount into the same DOM, so they are offset too.
        assert!(
            !bundle.components.is_empty(),
            "the Card component was extracted"
        );
        for (name, body) in &bundle.components {
            let mut ids = Vec::new();
            collect_ids(body, &mut ids);
            assert!(
                ids.iter().all(|id| *id >= BUNDLED_ID_OFFSET),
                "component {name} ids are offset: {ids:?}"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundle_from_dir_reports_a_missing_site_as_invalid_manifest() {
        let dir = std::env::temp_dir().join(format!("pwb-bundler-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        match bundle_from_dir(&dir) {
            Ok(_) => panic!("a missing directory must not bundle"),
            Err(BundlerError::InvalidManifest(msg)) => assert!(!msg.is_empty()),
            Err(other) => panic!("expected InvalidManifest, got {other:?}"),
        }
    }
}
