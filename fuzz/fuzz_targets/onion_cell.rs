#![no_main]

use libfuzzer_sys::fuzz_target;
use pwb_core::onion::OnionCell;

fuzz_target!(|data: &[u8]| {
    // Parsing must reject arbitrary bytes without panicking. Peeling needs a
    // key and is exercised by the unit tests instead.
    let _ = OnionCell::from_bytes(data);
});
