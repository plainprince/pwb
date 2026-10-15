//! Bundling for scripts and stylesheets (`pwb-web::bundler`).
//!
//! Multiple `.pjs`/`.js` and `.pss`/`.css` files are combined into single,
//! **non-minified** payloads (so the author's source stays readable) and then
//! carried by the `.pwb` artifact's bit-packed wire format.
//!
//! - **JS**: the QuickJS runtime is script-mode (no ESM execution), so the
//!   bundler concatenates all files in deterministic order with readable
//!   `// ── file: x.pjs ──` separators and strips `import`/`export` statements
//!   (the runtime provides `pwb.import(name)` / `pwb.hotload(name)` bindings
//!   backed by the named-module map for dynamic loading instead).
//! - **CSS**: `.css` files are bundled through `lightningcss` (resolving
//!   `@import` chains, non-minified); native `.pss` files are concatenated
//!   unchanged. Both end up in the single stylesheet applied at build time.

use std::collections::HashMap;
use std::path::Path;

/// Bundle all script files into (a) the entry script payload and (b) a named
/// module map for the runtime `pwb.import`/`pwb.hotload` bindings.
///
/// `files` is `(name, contents)` in any order; the output is deterministic
/// (sorted by name). `import`/`export` statements are stripped so the result
/// runs under script-mode `eval`.
pub fn bundle_js_files(
    files: &[(String, Vec<u8>)],
    entry_name: Option<&str>,
) -> (Vec<u8>, HashMap<String, Vec<u8>>) {
    // Deterministic order: modules first (sorted), the entry script last so its
    // dependencies are defined before it runs.
    let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut ordered: Vec<&(String, Vec<u8>)> = sorted
        .iter()
        .filter(|(n, _)| Some(n.as_str()) != entry_name)
        .cloned()
        .collect();
    if let Some(entry) = entry_name.and_then(|e| files.iter().find(|(n, _)| n == e)) {
        ordered.push(entry);
    }

    let mut out = String::new();
    let mut modules = HashMap::new();
    for (name, contents) in ordered {
        modules.insert(name.clone(), contents.clone());
        let text = String::from_utf8_lossy(contents);
        let cleaned = strip_esm_syntax(&text);
        out.push_str(&format!("// ── file: {} ──\n", name));
        out.push_str(&cleaned);
        if !cleaned.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    (out.into_bytes(), modules)
}

/// Remove `import ... ;` / `export ... ;` statements so the bundle runs under
/// script-mode `eval`. Line-based, non-minified-safe: it only strips lines
/// whose trimmed form starts with `import`/`export` (and their trailing
/// semicolons), leaving everything else untouched.
fn strip_esm_syntax(source: &str) -> String {
    source
        .lines()
        .filter(|line| {
            let t = line.trim();
            !(t.starts_with("import ") || t == "import;" || t.starts_with("export "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Bundle all `.css` files into a single non-minified stylesheet via
/// `lightningcss`, resolving `@import` chains relative to the site directory.
/// Returns the CSS text, or an empty string when there is nothing to bundle.
pub fn bundle_css_files(site_dir: &Path, css_files: &[String]) -> String {
    if css_files.is_empty() {
        return String::new();
    }
    // lightningcss resolves @import relative to the entry file's location, so
    // the synthetic entry must live inside the site directory.
    let entry_name = format!(".pwb-entry-{}.css", std::process::id());
    let entry_path = site_dir.join(&entry_name);
    let mut entry = String::new();
    for name in css_files {
        entry.push_str(&format!("@import \"./{}\";\n", name));
    }
    if std::fs::write(&entry_path, entry).is_err() {
        return String::new();
    }

    let mut css = String::new();
    let provider = lightningcss::bundler::FileProvider::new();
    let mut bundler = lightningcss::bundler::Bundler::new(
        &provider,
        None,
        lightningcss::stylesheet::ParserOptions::default(),
    );
    match bundler.bundle(entry_path.as_path()) {
        Ok(stylesheet) => {
            if let Ok(res) = stylesheet.to_css(lightningcss::stylesheet::PrinterOptions::default())
            {
                css = res.code;
            }
        }
        Err(e) => eprintln!("[pwb-web] CSS bundling failed: {:?}", e),
    }

    let _ = std::fs::remove_file(&entry_path);
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_js_bundle_orders_entry_last_and_strips_esm() {
        let files = vec![
            (
                "app.pjs".to_string(),
                b"import \"./lib.js\";\nmain();".to_vec(),
            ),
            (
                "lib.js".to_string(),
                b"function main() { return 42; }".to_vec(),
            ),
        ];
        let (payload, modules) = bundle_js_files(&files, Some("app.pjs"));

        // Entry runs last so its dependencies are defined first.
        let text = String::from_utf8(payload).unwrap();
        assert!(text.contains("── file: lib.js ──"));
        assert!(text.contains("── file: app.pjs ──"));
        assert!(text.find("lib.js ──").unwrap() < text.find("app.pjs ──").unwrap());
        // `import` statements are stripped (the runtime is script-mode).
        assert!(!text.contains("import "));
        assert!(text.contains("main();"));

        // Every file is available as a named module for pwb.import/hotload.
        assert_eq!(modules.len(), 2);
        assert!(modules.contains_key("lib.js"));
        assert!(modules.contains_key("app.pjs"));
    }

    #[test]
    fn test_css_bundle_via_lightningcss() {
        // A site dir with two css files, one importing the other: lightningcss
        // resolves the @import chain and emits both (non-minified).
        let dir = std::env::temp_dir().join(format!("pwb-css-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("base.css"), ".a { color: red; }\n").unwrap();
        std::fs::write(
            dir.join("main.css"),
            "@import \"./base.css\";\n.b { padding: 4px; }\n",
        )
        .unwrap();

        let bundled = bundle_css_files(&dir, &["main.css".to_string()]);
        assert!(
            bundled.contains(".a"),
            "imported rule should be inlined: {}",
            bundled
        );
        assert!(
            bundled.contains(".b"),
            "entry rule should be present: {}",
            bundled
        );
        // Non-minified: selectors and braces survive.
        assert!(bundled.contains("{") && bundled.contains("}"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
