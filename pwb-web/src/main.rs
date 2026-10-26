//! `pwb-web` CLI compiler. Parses PML, PSS, and JS files from a site directory.

use pwb_core::crypto::ContentId;
use pwb_web::bundle::create_bundle;
use pwb_web::compress::compress_bundle;
use pwb_web::parse::parse_site_directory;
use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let args: Vec<String> = env::args().collect();
    let site_dir = if args.len() >= 2 {
        args[1].clone()
    } else {
        "demo_site".to_string()
    };

    let output_path = if args.len() >= 3 {
        args[2].clone()
    } else {
        "demo.pwb".to_string()
    };

    // If default demo_site directory doesn't exist, generate standard sample files
    if site_dir == "demo_site" && !Path::new(&site_dir).exists() {
        create_demo_site_directory(&site_dir);
    }

    println!("PlainWeb Layer 1 Compiler (pwb-web)");
    println!("Parsing site directory: {}", site_dir);

    match compile(&site_dir, &output_path) {
        Ok((content_id, len)) => {
            println!("\nSuccessfully compiled PlainWeb bundle!");
            println!("  Source Folder : {}", site_dir);
            println!("  Output Bundle : {}", output_path);
            println!("  Bundle Size   : {} bytes", len);
            println!("  Content ID    : {}", content_id);
        }
        Err(error) => eprintln!("{}", error),
    }
}

/// Parse, bundle, and compress the site directory, then write the `.pwb`
/// artifact to `output_path`. Returns the artifact's content id and byte
/// length; the error strings are the CLI's exact user-facing messages.
fn compile(site_dir: &str, output_path: &str) -> Result<(ContentId, usize), String> {
    let parsed_site = parse_site_directory(site_dir)
        .map_err(|e| format!("Compilation failed for '{}': {:?}", site_dir, e))?;
    let bundle = create_bundle(
        parsed_site.root_node,
        parsed_site.title,
        None,
        parsed_site.script_payload,
        parsed_site.components,
        parsed_site.modules,
    );
    let binary = compress_bundle(&bundle);
    let content_id = ContentId::compute(&binary);

    fs::write(output_path, &binary)
        .map_err(|e| format!("Failed to write output binary '{}': {}", output_path, e))?;
    Ok((content_id, binary.len()))
}

fn create_demo_site_directory(dir: &str) {
    let _ = fs::create_dir_all(dir);

    let pml_content = r#"<plain>
    <head>
        <title>Plain Web Demo</title>
    </head>
    <view class="hero">
        <text class="heading">Welcome to the PlainWeb!</text>
        <text class="subtitle">Built with PML (Plain Markup), PSS (Plain Style), and JS.</text>
        <button id="main-btn">Explore Network</button>
    </view>
</plain>"#;

    let pss_content = r#"
.hero {
    background-color: #1e1e2e;
    padding: 30px;
    gap: 15px;
}

.heading {
    color: #89b4fa;
    font-size: 28px;
}

.subtitle {
    color: #cdd6f4;
    font-size: 16px;
}

#main-btn {
    background-color: #a6e3a1;
    color: #11111b;
    padding: 12px;
    border-radius: 8px;
}
"#;

    let js_content = r#"
console.log("PlainWeb JS runtime loaded successfully.");
"#;

    let _ = fs::write(format!("{}/index.pml", dir), pml_content);
    let _ = fs::write(format!("{}/style.pss", dir), pss_content);
    let _ = fs::write(format!("{}/app.pjs", dir), js_content);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pwb-cli-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const INDEX: &str = r#"<plain>
    <head><title>CLI Test Site</title></head>
    <view class="hero"><text class="heading">Hello</text></view>
</plain>"#;

    #[test]
    fn compile_writes_a_valid_compressed_bundle() {
        let dir = tmp("site");
        fs::write(dir.join("index.pml"), INDEX).unwrap();
        let out = dir.join("out.pwb");

        let (content_id, len) =
            compile(&dir.display().to_string(), &out.display().to_string()).expect("site compiles");

        let bytes = fs::read(&out).unwrap();
        assert_eq!(bytes.len(), len, "reported size matches the artifact");
        assert!(pwb_web::compress::is_compressed(&bytes), "PWBZ container");
        let bundle = pwb_web::compress::decompress_bundle(&bytes).expect("decodes");
        assert_eq!(bundle.title, "CLI Test Site");
        assert_eq!(content_id, ContentId::compute(&bytes), "id is the CID");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compile_reports_a_missing_site_directory() {
        let missing = std::env::temp_dir()
            .join(format!("pwb-cli-missing-{}", std::process::id()))
            .display()
            .to_string();
        let _ = fs::remove_dir_all(&missing);
        let err = compile(&missing, "/dev/null").unwrap_err();
        assert!(
            err.starts_with("Compilation failed"),
            "parse failures keep their message: {err}"
        );
    }

    #[test]
    fn compile_reports_an_unwritable_output() {
        let dir = tmp("nowrite");
        fs::write(dir.join("index.pml"), INDEX).unwrap();
        let out = dir
            .join("no-such-subdir")
            .join("out.pwb")
            .display()
            .to_string();
        let err = compile(&dir.display().to_string(), &out).unwrap_err();
        assert!(
            err.starts_with("Failed to write output binary"),
            "write failures keep their message: {err}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn demo_site_generation_produces_a_compilable_site() {
        let dir = tmp("demo");
        let dir_str = dir.display().to_string();
        create_demo_site_directory(&dir_str);
        for file in ["index.pml", "style.pss", "app.pjs"] {
            assert!(dir.join(file).is_file(), "{file} generated");
        }
        let out = dir.join("demo.pwb");
        let (content_id, _) = compile(&dir_str, &out.display().to_string())
            .expect("the generated demo site compiles");
        assert_eq!(content_id, ContentId::compute(&fs::read(&out).unwrap()));
        let _ = fs::remove_dir_all(&dir);
    }
}
