//! `pwb-web` CLI compiler. Parses PML, PSS, and JS files from a site directory.

use pwb_core::crypto::{ContentId, Keypair};
use pwb_web::bundle::create_bundle;
use pwb_web::compress::compress_bundle;
use pwb_web::parse::parse_site_directory;
use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let mut args = env::args().skip(1);
    let mut site_dir = None;
    let mut output_path = None;
    let mut author_key = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage();
                return;
            }
            "--author-key" => match args.next() {
                Some(path) => author_key = Some(path),
                None => {
                    eprintln!("--author-key needs a path to an identity JSON file or 64-hex seed");
                    std::process::exit(1);
                }
            },
            other if other.starts_with('-') => {
                eprintln!("Unknown option: {other}");
                print_usage();
                std::process::exit(1);
            }
            other if site_dir.is_none() => site_dir = Some(other.to_string()),
            other if output_path.is_none() => output_path = Some(other.to_string()),
            other => {
                eprintln!("Unexpected argument: {other}");
                print_usage();
                std::process::exit(1);
            }
        }
    }

    let site_dir = site_dir.unwrap_or_else(|| "demo_site".to_string());
    let output_path = output_path.unwrap_or_else(|| "demo.pwb".to_string());

    // The author key is optional: unsigned sites still compile, but a site
    // that names an author must be signed so consumers can verify it. The
    // environment variable keeps the key path out of shell history.
    let author_key_path = author_key.or_else(|| env::var("PWB_AUTHOR_KEY").ok());
    let author_key = match author_key_path {
        Some(path) => match load_author_key(&path) {
            Ok(key) => Some(key),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        },
        None => None,
    };

    // If default demo_site directory doesn't exist, generate standard sample files
    if site_dir == "demo_site" && !Path::new(&site_dir).exists() {
        create_demo_site_directory(&site_dir);
    }

    println!("PlainWeb Layer 1 Compiler (pwb-web)");
    println!("Parsing site directory: {}", site_dir);

    match compile(&site_dir, &output_path, author_key.as_ref()) {
        Ok((content_id, len)) => {
            println!("\nSuccessfully compiled PlainWeb bundle!");
            println!("  Source Folder : {}", site_dir);
            println!("  Output Bundle : {}", output_path);
            println!("  Bundle Size   : {} bytes", len);
            println!("  Content ID    : {}", content_id);
            if let Some(key) = &author_key {
                println!("  Author        : {}", key.public.to_hex());
                println!("  Signature     : verified by Bundle::deserialize");
            }
        }
        Err(error) => {
            eprintln!("{}", error);
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    println!(
        "Usage: pwb-web [<site_directory> [output.pwb]] [--author-key <path>]\n\n\
         --author-key  Ed25519 identity JSON (secret_key_hex) or a 64-hex seed.\n\
                       Also read from PWB_AUTHOR_KEY when the flag is omitted.\n\
         -h, --help    Show this help."
    );
}

/// Load an Ed25519 signing key from an identity JSON file
/// (`{"secret_key_hex": "..."}`) or a bare 64-hex seed. The public key is
/// derived from the seed; a mismatched `public_key` field is rejected so a
/// corrupted identity file cannot silently change the author.
fn load_author_key(path: &str) -> Result<Keypair, String> {
    let bytes =
        fs::read(path).map_err(|e| format!("Failed to read author key '{}': {}", path, e))?;
    let text = String::from_utf8_lossy(&bytes);
    let (secret_hex, expected_public) = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(value) => {
            let secret = value
                .get("secret_key_hex")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("Author key '{}' has no secret_key_hex", path))?;
            let public = value
                .get("public_key")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            (secret.to_string(), public)
        }
        Err(_) => (text.trim().trim_start_matches("0x").to_string(), None),
    };
    let seed = hex::decode(secret_hex.trim())
        .map_err(|e| format!("Author key '{}' is not valid hex: {}", path, e))?;
    if seed.len() != 32 {
        return Err(format!(
            "Author key '{}' must be a 32-byte Ed25519 seed (got {} bytes)",
            path,
            seed.len()
        ));
    }
    let mut seed_bytes = [0u8; 32];
    seed_bytes.copy_from_slice(&seed);
    let keypair = Keypair::from_seed(&seed_bytes);
    if let Some(expected) = expected_public {
        if expected != keypair.public.to_hex() {
            return Err(format!(
                "Author key '{}' has a public_key that does not match its seed",
                path
            ));
        }
    }
    Ok(keypair)
}

/// Parse, bundle, and compress the site directory, then write the `.pwb`
/// artifact to `output_path`. When `author_key` is set, the artifact carries a
/// verified author signature. Returns the artifact's content id and byte
/// length; the error strings are the CLI's exact user-facing messages.
fn compile(
    site_dir: &str,
    output_path: &str,
    author_key: Option<&Keypair>,
) -> Result<(ContentId, usize), String> {
    let parsed_site = parse_site_directory(site_dir)
        .map_err(|e| format!("Compilation failed for '{}': {:?}", site_dir, e))?;
    let mut bundle = create_bundle(
        parsed_site.root_node,
        parsed_site.title,
        None,
        parsed_site.script_payload,
        parsed_site.components,
        parsed_site.modules,
    );
    if let Some(key) = author_key {
        bundle.sign_author(key);
    }
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
            compile(&dir.display().to_string(), &out.display().to_string(), None)
                .expect("site compiles");

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
        let err = compile(&missing, "/dev/null", None).unwrap_err();
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
        let err = compile(&dir.display().to_string(), &out, None).unwrap_err();
        assert!(
            err.starts_with("Failed to write output binary"),
            "write failures keep their message: {err}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compile_signs_the_bundle_when_an_author_key_is_given() {
        let dir = tmp("signed");
        fs::write(dir.join("index.pml"), INDEX).unwrap();
        let out = dir.join("signed.pwb");
        let key = Keypair::generate();

        compile(
            &dir.display().to_string(),
            &out.display().to_string(),
            Some(&key),
        )
        .expect("signed site compiles");

        let bundle = pwb_web::compress::decompress_bundle(&fs::read(&out).unwrap())
            .expect("signed artifact decodes");
        assert_eq!(bundle.author_pubkey, Some(key.public.0));
        assert!(
            bundle.verify_author_signature(),
            "the artifact carries a verifiable signature"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_author_key_accepts_json_and_rejects_a_mismatched_public_key() {
        let dir = tmp("author-key");
        let key = Keypair::generate();
        let path = dir.join("identity.json");
        fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "public_key": key.public.to_hex(),
                "secret_key_hex": hex::encode(key.secret.0),
            })
            .to_string(),
        )
        .unwrap();
        let loaded = load_author_key(&path.display().to_string()).expect("identity JSON loads");
        assert_eq!(loaded.public, key.public);

        fs::write(
            &path,
            serde_json::json!({
                "public_key": "00".repeat(32),
                "secret_key_hex": hex::encode(key.secret.0),
            })
            .to_string(),
        )
        .unwrap();
        let err = load_author_key(&path.display().to_string()).unwrap_err();
        assert!(err.contains("does not match"), "got: {err}");
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
        let (content_id, _) = compile(&dir_str, &out.display().to_string(), None)
            .expect("the generated demo site compiles");
        assert_eq!(content_id, ContentId::compute(&fs::read(&out).unwrap()));
        let _ = fs::remove_dir_all(&dir);
    }
}
