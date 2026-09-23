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

    match parse_site_directory(&site_dir) {
        Ok(parsed_site) => {
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

            if let Err(e) = fs::write(&output_path, &binary) {
                eprintln!("Failed to write output binary '{}': {}", output_path, e);
            } else {
                println!("\nSuccessfully compiled PlainWeb bundle!");
                println!("  Source Folder : {}", site_dir);
                println!("  Output Bundle : {}", output_path);
                println!("  Bundle Size   : {} bytes", binary.len());
                println!("  Content ID    : {}", content_id);
            }
        }
        Err(e) => {
            eprintln!("Compilation failed for '{}': {:?}", site_dir, e);
        }
    }
}

fn create_demo_site_directory(dir: &str) {
    let _ = fs::create_dir_all(dir);

    let pml_content = r#"<plain>
    <head>
        <title>Untrackable Plain Web Demo</title>
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
