//! End-to-end authoring coverage: `parse_site_directory` is the entry point the
//! CLI, the browser's built-in pages, and `pwb-node --host` all compile with.
//! A site that uses ordinary ESM must become script-mode JS in both the entry
//! payload and the `pwb.import` module map, and unsupported ESM must fail with
//! a build error naming the file and line.

use std::fs;
use std::path::PathBuf;

fn site_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pwb-esm-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

const INDEX: &str = r#"<plain>
    <head><title>ESM Site</title></head>
    <view><text>hi</text></view>
</plain>"#;

#[test]
fn multiline_esm_module_bundles_and_is_importable_at_runtime() {
    let dir = site_dir("ok");
    fs::write(dir.join("index.pml"), INDEX).unwrap();
    fs::write(
        dir.join("app.pjs"),
        "import {\n    double,\n} from \"./lib.js\";\nexport const answer = double(21);\n",
    )
    .unwrap();
    fs::write(
        dir.join("lib.js"),
        "export function double(value) {\n    return value * 2;\n}\n",
    )
    .unwrap();

    let site = pwb_web::parse::parse_site_directory(&dir).expect("ESM site compiles");
    let payload = String::from_utf8(site.script_payload.clone().expect("script payload")).unwrap();

    // The concatenated bundle runs under script-mode eval, so no module syntax
    // may reach QuickJS.
    assert!(!payload.contains("import"), "{payload}");
    assert!(!payload.contains("export"), "{payload}");
    assert!(payload.contains("const answer = double(21);"), "{payload}");
    assert!(payload.contains("function double(value) {"), "{payload}");

    // `pwb.import("lib.js")` evals the module map directly; it must carry the
    // rewritten source, not the authored ESM.
    let module = String::from_utf8(site.modules["lib.js"].clone()).unwrap();
    assert!(!module.contains("export"), "{module}");
    assert!(module.contains("function double(value) {"), "{module}");
    // The entry is also a named module, so importing it never re-runs ESM.
    let entry = String::from_utf8(site.modules["app.pjs"].clone()).unwrap();
    assert!(!entry.contains("import"), "{entry}");
    assert!(entry.contains("const answer = double(21);"), "{entry}");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn export_default_fails_the_build_with_file_and_line() {
    let dir = site_dir("default");
    fs::write(dir.join("index.pml"), INDEX).unwrap();
    fs::write(
        dir.join("app.pjs"),
        "const value = 1;\nexport default value;\n",
    )
    .unwrap();

    let error = match pwb_web::parse::parse_site_directory(&dir) {
        Ok(_) => panic!("export default must fail the build"),
        Err(error) => error,
    };
    let message = format!("{error:?}");
    assert!(message.contains("app.pjs:2"), "{message}");
    assert!(message.contains("export default"), "{message}");

    let _ = fs::remove_dir_all(&dir);
}
