//! Lossless compression & binary encoding (`pwb-web::compress`).

use pwb_core::wire::Bundle;

/// Compress a `Bundle` into binary `.pwb` bytes using PlainWeb wire format.
pub fn compress_bundle(bundle: &Bundle) -> Vec<u8> {
    bundle.serialize()
}
