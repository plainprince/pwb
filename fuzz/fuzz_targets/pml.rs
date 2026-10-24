#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Authored source is UTF-8; malformed bytes are rejected before parsing.
    // Both parsers run on every hosted/browsed site, so arbitrary text must
    // terminate without a panic or unbounded allocation.
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = pwb_web::parse::parse_pml_markup(text);
        let _ = pwb_web::parse::parse_pss_stylesheet(text);
    }
});
