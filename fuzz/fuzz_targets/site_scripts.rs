#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // `.pjs`/`.js` sources are bundled for every hosted site. The ESM rewriter
    // scans arbitrary authored code (strings, templates, comments, and
    // unterminated constructs) and must terminate without panicking, so feed
    // the same bytes through several files and entry selections.
    let (selector, body) = match data.split_first() {
        Some((selector, body)) => (*selector, body),
        None => (0, data),
    };
    let split = body.len() / 2;
    let files = vec![
        ("app.pjs".to_string(), body[..split].to_vec()),
        ("lib.js".to_string(), body[split..].to_vec()),
        ("inline-1.js".to_string(), body.to_vec()),
    ];

    const ENTRIES: [&str; 3] = ["app.pjs", "main.pjs", "index.pjs"];
    let entry = ENTRIES[selector as usize % ENTRIES.len()];
    let _ = pwb_web::bundler::bundle_js_files(&files, Some(entry));
    let _ = pwb_web::bundler::bundle_js_files(&files, None);
});
