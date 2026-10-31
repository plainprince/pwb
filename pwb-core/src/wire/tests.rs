use super::*;
use crate::crypto::Keypair;
use crate::spec::{PlainNode, Tag};
use std::collections::HashMap;

#[test]
fn test_wire_roundtrip() {
    let mut root = PlainNode::new(1, Tag::Document);
    let mut text_node = PlainNode::new(2, Tag::Text);
    text_node.text_content = Some("Hello PlainWeb!".to_string());
    root.children.push(text_node);

    // An icon element survives the round trip with its `icon` attribute.
    let mut icon = PlainNode::new(0, Tag::Icon);
    icon.attributes
        .insert("icon".to_string(), "home".to_string());
    root.children.push(icon);

    // A `<code>` node survives with its highlighted text.
    let mut code = PlainNode::new(0, Tag::Code);
    code.text_content = Some("var x = 42;".to_string());
    root.children.push(code);

    // A component body with a {param} placeholder and a named script module
    // must survive the bit-packed wire round trip (v2 bundling fields).
    let mut comp = PlainNode::new(0, Tag::View);
    comp.attributes
        .insert("class".to_string(), "card".to_string());
    let mut comp_text = PlainNode::new(0, Tag::Text);
    comp_text.text_content = Some("Hello {name}".to_string());
    comp.children.push(comp_text);

    let bundle = Bundle {
        version: CURRENT_VERSION,
        title: "Test Site".to_string(),
        author_pubkey: None,
        author_signature: None,
        root_node: root,
        script_payload: Some(vec![1, 2, 3, 4]),
        components: HashMap::from([("greeting".to_string(), comp)]),
        modules: HashMap::from([("lib.js".to_string(), b"var x = 1;".to_vec())]),
    };

    let binary = bundle.serialize();
    let decoded = Bundle::deserialize(&binary).unwrap();

    assert_eq!(bundle.title, decoded.title);
    assert_eq!(
        bundle.root_node.children.len(),
        decoded.root_node.children.len()
    );
    let decoded_icon = decoded
        .root_node
        .children
        .iter()
        .find(|n| n.tag == Tag::Icon)
        .expect("icon survives");
    assert_eq!(
        decoded_icon.attributes.get("icon").map(|s| s.as_str()),
        Some("home")
    );
    let decoded_code = decoded
        .root_node
        .children
        .iter()
        .find(|n| n.tag == Tag::Code)
        .expect("code survives");
    assert_eq!(decoded_code.text_content.as_deref(), Some("var x = 42;"));
    assert_eq!(bundle.script_payload, decoded.script_payload);
    assert_eq!(bundle.modules, decoded.modules);
    // Node ids are intentionally re-assigned on decode (never encoded), so
    // compare the component structure (tag/attrs/text) without ids.
    let greeting = decoded.components.get("greeting").unwrap();
    assert_eq!(greeting.tag, Tag::View);
    assert_eq!(
        greeting.attributes.get("class").map(|s| s.as_str()),
        Some("card")
    );
    assert_eq!(greeting.children.len(), 1);
    assert_eq!(greeting.children[0].tag, Tag::Text);
    assert_eq!(
        greeting.children[0].text_content.as_deref(),
        Some("Hello {name}")
    );
}

/// A bundle with plenty of repeated text, so deflate has something to remove.
fn repetitive_bundle() -> Bundle {
    let mut root = PlainNode::new(1, Tag::Document);
    for i in 0..64 {
        let mut text = PlainNode::new(i + 2, Tag::Text);
        text.text_content = Some("PlainWeb compresses repeated content losslessly.".to_string());
        root.children.push(text);
    }
    Bundle {
        version: CURRENT_VERSION,
        title: "Compressed Test".to_string(),
        author_pubkey: None,
        author_signature: None,
        root_node: root,
        script_payload: Some(b"console.log('hello');".repeat(20)),
        components: HashMap::new(),
        modules: HashMap::new(),
    }
}

#[test]
fn test_compressed_bundle_roundtrips_and_shrinks() {
    let bundle = repetitive_bundle();
    let raw = bundle.serialize();
    let compressed = bundle.serialize_compressed();

    assert!(Bundle::is_compressed(&compressed));
    assert_eq!(&compressed[..4], COMPRESSED_MAGIC);
    assert_eq!(compressed[4], COMPRESSION_VERSION);
    // Header + zlib stream must beat the raw repetitive bundle.
    assert!(
        compressed.len() < raw.len(),
        "compressed {} bytes should be smaller than {} raw bytes",
        compressed.len(),
        raw.len()
    );

    let decoded = Bundle::deserialize(&compressed).expect("compressed bundle decodes");
    assert_eq!(decoded.title, bundle.title);
    assert_eq!(decoded.root_node.children.len(), 64);
    assert_eq!(
        decoded.root_node.children[0].text_content.as_deref(),
        Some("PlainWeb compresses repeated content losslessly.")
    );
    assert_eq!(decoded.script_payload, bundle.script_payload);
    // Re-encoding is deterministic, so the canonical bytes match exactly.
    assert_eq!(decoded.serialize(), raw);
}

#[test]
fn test_legacy_uncompressed_bundles_still_load() {
    let bundle = repetitive_bundle();
    let raw = bundle.serialize();
    assert!(!Bundle::is_compressed(&raw));
    let decoded = Bundle::deserialize(&raw).expect("legacy PWB1 still decodes");
    assert_eq!(decoded.title, bundle.title);
}

#[test]
fn test_compressed_bundle_rejects_truncation_and_corruption() {
    let bundle = repetitive_bundle();
    let compressed = bundle.serialize_compressed();

    // Truncated zlib stream (cut into the compressed data, not just the
    // checksum trailer).
    assert!(Bundle::deserialize(&compressed[..compressed.len() / 2]).is_err());

    // Corrupted byte in the middle of the zlib body.
    let mut corrupt = compressed.clone();
    let middle = 41 + (corrupt.len() - 41) / 2;
    corrupt[middle] ^= 0xFF;
    assert!(Bundle::deserialize(&corrupt).is_err());

    // Declared length that does not match the inflated data.
    let mut wrong_len = compressed.clone();
    let declared = (bundle.serialize().len() + 1) as u32;
    wrong_len[5..9].copy_from_slice(&declared.to_le_bytes());
    assert!(Bundle::deserialize(&wrong_len).is_err());

    // Unknown compression version.
    let mut bad_version = compressed.clone();
    bad_version[4] = 99;
    assert!(Bundle::deserialize(&bad_version).is_err());
}

#[test]
fn test_bundle_rejects_trailing_bytes() {
    let bundle = repetitive_bundle();
    let mut raw = bundle.serialize();
    raw.push(0xAB);
    let err = Bundle::deserialize(&raw).expect_err("trailing byte is rejected");
    assert!(matches!(err, WireError::TrailingBytes(1)), "got {err:?}");

    let mut trailing_many = bundle.serialize();
    trailing_many.extend_from_slice(b"garbage");
    assert!(matches!(
        Bundle::deserialize(&trailing_many),
        Err(WireError::TrailingBytes(_))
    ));

    // Exact bytes still decode.
    let exact = bundle.serialize();
    assert!(Bundle::deserialize(&exact).is_ok());
}

#[test]
fn test_compressed_bundle_rejects_trailing_bytes() {
    let bundle = repetitive_bundle();
    let mut compressed = bundle.serialize_compressed();
    compressed.extend_from_slice(b"trailing");
    let err = Bundle::deserialize(&compressed).expect_err("trailing container bytes rejected");
    assert!(matches!(err, WireError::TrailingBytes(_)), "got {err:?}");
}

#[test]
fn test_signed_bundle_roundtrips_and_rejects_tampering() {
    let keypair = Keypair::generate();
    let mut bundle = repetitive_bundle();
    bundle.sign_author(&keypair);
    assert_eq!(bundle.author_pubkey, Some(keypair.public.0));
    assert!(bundle.verify_author_signature());

    // The signature survives the compressed container and is verified on
    // decode, so a consumer never sees a bundle with a forged author claim.
    let compressed = bundle.serialize_compressed();
    let decoded = Bundle::deserialize(&compressed).expect("signed bundle decodes");
    assert_eq!(decoded.author_pubkey, Some(keypair.public.0));
    assert_eq!(decoded.author_signature, bundle.author_signature);
    assert!(decoded.verify_author_signature());

    // Any content change invalidates the signature. Flip a byte in the
    // canonical payload, not in the signature itself, so the failure is an
    // authenticity failure rather than a framing/length error.
    let mut tampered = bundle.serialize();
    let title_byte = tampered
        .windows(bundle.title.len())
        .position(|window| window == bundle.title.as_bytes())
        .expect("title bytes are in the bundle");
    tampered[title_byte] ^= 0x01;
    assert!(matches!(
        Bundle::deserialize(&tampered),
        Err(WireError::InvalidAuthorSignature)
    ));

    // A named author without a signature is also rejected: authorship is
    // never accepted from an unauthenticated field.
    let mut unsigned = bundle.clone();
    unsigned.author_signature = None;
    assert!(matches!(
        Bundle::deserialize(&unsigned.serialize()),
        Err(WireError::InvalidAuthorSignature)
    ));
}

#[test]
fn test_unsigned_bundles_remain_compatible() {
    let bundle = repetitive_bundle();
    assert!(bundle.author_pubkey.is_none());
    assert!(bundle.author_signature.is_none());
    assert!(bundle.verify_author_signature());
    let decoded = Bundle::deserialize(&bundle.serialize()).expect("unsigned bundle decodes");
    assert!(decoded.author_pubkey.is_none());
    assert!(decoded.author_signature.is_none());
}

#[test]
fn test_compressed_bundle_rejects_oversized_declaration() {
    let mut bomb = Vec::new();
    bomb.extend_from_slice(COMPRESSED_MAGIC);
    bomb.push(COMPRESSION_VERSION);
    bomb.extend_from_slice(&((MAX_DECOMPRESSED_BYTES + 1) as u32).to_le_bytes());
    bomb.extend_from_slice(&[0u8; 32]); // digest placeholder
    bomb.extend_from_slice(b"not a zlib stream");
    let err = Bundle::deserialize(&bomb).expect_err("oversized declaration is rejected");
    assert!(matches!(err, WireError::CompressionFailed(_)));
}
