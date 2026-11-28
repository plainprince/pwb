#![no_main]

use libfuzzer_sys::fuzz_target;
use pwb_core::wire::Bundle;

fuzz_target!(|data: &[u8]| {
    // Bundle decoding runs on every hosted/fetched artifact. It must reject
    // truncated, corrupted, and oversized input without panicking or looping.
    let Ok(bundle) = Bundle::deserialize(data) else {
        return;
    };

    // The same decoded website bundle is re-encoded for hosting and storage.
    // Encoding must stay canonical and lossless: both the raw `PWB1` bytes and
    // the compressed `PWBZ` container have to decode back to the same bundle.
    let raw = bundle.serialize();
    assert_eq!(
        Bundle::deserialize(&raw).expect("re-serialized bundle must decode"),
        bundle
    );

    let compressed = bundle.serialize_compressed();
    assert_eq!(
        pwb_web::compress::decompress_bundle(&compressed)
            .expect("compressed bundle must decode"),
        bundle
    );
});
