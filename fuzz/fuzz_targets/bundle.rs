#![no_main]

use libfuzzer_sys::fuzz_target;
use pwb_core::wire::Bundle;

fuzz_target!(|data: &[u8]| {
    // Bundle decoding runs on every hosted/fetched artifact. It must reject
    // truncated, corrupted, and oversized input without panicking or looping.
    let _ = Bundle::deserialize(data);
});
