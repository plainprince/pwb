#![no_main]

//! Fuzz the filesystem-facing site builder: the directory walk, PML/PSS
//! parsers, component/script extraction, the JS ESM rewrite, and bundle
//! creation must all reject or survive arbitrary authored bytes.
//!
//! One input is exercised three ways, each against its own directory: as
//! `index.pml`, as the `app.pjs` entry beside a fixed valid page, and as
//! `style.pss` beside the same page. Sharing a single input across all three
//! files would not work, because the builder aborts on the first parse error,
//! so a valid script could never reach the bundler. The harness is hermetic:
//! one temp directory tree per fuzz process, the input is capped, and `src`
//! references that would resolve outside that tree are skipped instead of read
//! (a device file would otherwise block the run).

use libfuzzer_sys::fuzz_target;
use pwb_web::parse::parse_site_directory;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Authored sites are small; a bigger corpus entry only slows the run.
const MAX_INPUT: usize = 64 * 1024;

/// Valid page used for the script and stylesheet scenarios. It has no `src`
/// references, so the bundler only reads the fuzzed file.
const MINIMAL_PAGE: &str = "<plain><view><text>x</text></view></plain>";

static SITE_DIR: OnceLock<PathBuf> = OnceLock::new();

fn site_dir() -> &'static Path {
    SITE_DIR.get_or_init(|| {
        let base = std::env::temp_dir().join(format!("pwb-fuzz-site-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        for case in ["pml", "js", "pss"] {
            let dir = base.join(case);
            fs::create_dir_all(&dir).expect("create fuzz site directory");
            if case != "pml" {
                fs::write(dir.join("index.pml"), MINIMAL_PAGE).expect("write fuzz page");
            }
        }
        base
    })
}

fn value_escapes(value: &[u8]) -> bool {
    value.first() == Some(&b'/') || value.windows(2).any(|pair| pair == b"..")
}

/// True when a `src` attribute would escape the temp directory. The builder
/// reads `<script src>` and `<component src>` paths, so an absolute path or a
/// `..` segment could open a device or a file outside the corpus. Both quoted
/// and unquoted values are handled (the PML parser accepts either).
fn has_escaping_src(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        if &bytes[index..index + 3] != b"src" {
            index += 1;
            continue;
        }
        let mut cursor = index + 3;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            index += 1;
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            index += 1;
            continue;
        }
        let quote = bytes[cursor];
        let (start, end) = if quote == b'"' || quote == b'\'' {
            let start = cursor + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != quote {
                end += 1;
            }
            (start, end)
        } else {
            let start = cursor;
            let mut end = start;
            while end < bytes.len() && !bytes[end].is_ascii_whitespace() && bytes[end] != b'>' {
                end += 1;
            }
            (start, end)
        };
        if value_escapes(&bytes[start..end]) {
            return true;
        }
        index = end.max(index + 1);
    }
    false
}

/// Run the same parse → bundle → compress path the CLI uses, so a panic in any
/// of them is caught even when the authored source parsed cleanly.
fn build_site(dir: &Path) {
    let Ok(site) = parse_site_directory(dir) else {
        return;
    };
    let bundle = pwb_web::bundle::create_bundle(
        site.root_node,
        site.title,
        None,
        site.script_payload,
        site.components,
        site.modules,
    );
    let encoded = pwb_web::compress::compress_bundle(&bundle);
    let _ = pwb_web::compress::decompress_bundle(&encoded);
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let Ok(text) = std::str::from_utf8(data) else {
        // PML/PSS are read as UTF-8 by the site builder; malformed bytes never
        // reach the parsers. Script bytes may be arbitrary, but the same input
        // drives the markup scenario, so skip the whole entry.
        return;
    };
    if has_escaping_src(text) {
        return;
    }

    let base = site_dir();
    for (case, file) in [
        ("pml", "index.pml"),
        ("js", "app.pjs"),
        ("pss", "style.pss"),
    ] {
        let dir = base.join(case);
        if fs::write(dir.join(file), data).is_err() {
            return;
        }
        build_site(&dir);
    }
});
