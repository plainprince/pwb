//! Lossless compression & binary encoding (`pwb-web::compress`).
//!
//! A `.pwb` artifact is stored in the compressed `PWBZ` container: a 41-byte
//! header (magic, compression version, uncompressed length, BLAKE3 digest)
//! followed by a zlib stream of the canonical `PWB1` bundle. Decompression lives in
//! [`pwb_core::wire::Bundle::deserialize`], which also accepts legacy
//! uncompressed `PWB1` bytes, so artifacts made before compression still load.

use pwb_core::wire::{Bundle, WireError};

/// Compress a `Bundle` into binary `.pwb` bytes (`PWBZ` container).
pub fn compress_bundle(bundle: &Bundle) -> Vec<u8> {
    bundle.serialize_compressed()
}

/// Decompress and decode `.pwb` bytes, accepting both `PWBZ` and legacy
/// uncompressed `PWB1` artifacts.
pub fn decompress_bundle(bytes: &[u8]) -> Result<Bundle, WireError> {
    Bundle::deserialize(bytes)
}

/// Whether `bytes` is a compressed `PWBZ` artifact.
pub fn is_compressed(bytes: &[u8]) -> bool {
    Bundle::is_compressed(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::spec::{PlainNode, Tag};
    use pwb_core::wire::CURRENT_VERSION;
    use std::collections::HashMap;

    fn sample_bundle() -> Bundle {
        let mut root = PlainNode::new(0, Tag::Document);
        let mut text = PlainNode::new(1, Tag::Text);
        text.text_content = Some("hello plainweb".to_string());
        root.children.push(text);
        Bundle {
            version: CURRENT_VERSION,
            title: "sample".to_string(),
            author_pubkey: None,
            author_signature: None,
            root_node: root,
            script_payload: None,
            components: HashMap::new(),
            modules: HashMap::new(),
        }
    }

    #[test]
    fn compress_decompress_roundtrips() {
        let bundle = sample_bundle();
        let bytes = compress_bundle(&bundle);
        assert!(is_compressed(&bytes), "PWBZ container flags as compressed");
        let back = decompress_bundle(&bytes).expect("compressed bytes decode");
        assert_eq!(back.title, bundle.title);
        assert_eq!(back.serialize(), bundle.serialize());
    }

    #[test]
    fn legacy_uncompressed_artifacts_still_load() {
        let legacy = sample_bundle().serialize();
        assert!(!is_compressed(&legacy), "legacy PWB1 bytes are not PWBZ");
        let back = decompress_bundle(&legacy).expect("legacy bytes decode");
        assert_eq!(back.serialize(), legacy);
    }

    #[test]
    fn garbage_and_truncated_containers_are_rejected() {
        assert!(!is_compressed(b"definitely not a bundle"));
        assert!(decompress_bundle(b"definitely not a bundle").is_err());

        let mut cut = compress_bundle(&sample_bundle());
        cut.truncate(20);
        assert!(
            decompress_bundle(&cut).is_err(),
            "truncated PWBZ is rejected"
        );
    }
}
