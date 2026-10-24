#![no_main]

use libfuzzer_sys::fuzz_target;
use pwb_core::net::NetworkMessage;

fuzz_target!(|data: &[u8]| {
    // Wire messages are JSON inside authenticated envelopes. A decoder panic
    // would take down the receiver loop, so round-trip arbitrary input.
    if let Ok(message) = serde_json::from_slice::<NetworkMessage>(data) {
        let _ = serde_json::to_vec(&message);
    }
});
