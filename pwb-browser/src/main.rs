//! `pwb-browser` CLI runner. Supports builtin pages (`demo`, `welcome.pwb`),
//! built-in system domains (`search.plain`), registered `.pwb` network domains,
//! and local `.pwb` files via `--file`.

use pwb_browser::pipeline::run_browser_window;
use pwb_browser::resolver::resolve_url_or_file;
use pwb_node::domain::DomainRegistry;
use pwb_node::host::ArtifactHost;
use pwb_node::identity::IdentityManager;
use pwb_node::search::SearchEngine;
use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();

    // Parse flags
    let mut target = "demo".to_string();
    let mut i = 1;

    // Help message
    if args.len() == 2 && (args[1] == "-h" || args[1] == "--help") {
        print_help();
        std::process::exit(0);
    }

    while i < args.len() {
        match args[i].as_str() {
            "-f" | "--file" => {
                if i + 1 < args.len() {
                    target = format!("file.pwb/{}", args[i + 1]);
                    i += 2;
                } else {
                    eprintln!("Error: --file flag requires a path argument");
                    std::process::exit(1);
                }
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            _ => {
                target = args[i].clone();
                i += 1;
            }
        }
    }

    println!("PlainWeb Layer 2 Runtime Browser (pwb-browser)");
    println!("Resolving target / URI: {}", target);

    let host = ArtifactHost::new();
    let registry = DomainRegistry::new();
    let identity = IdentityManager::new();
    let search = SearchEngine::new();

    // If the target is a `nodes.plain` connection link, launch the connection
    // to the described peer (URI-handler entry point, TODO §3.2).
    if let Some((addr, pk)) = pwb_core::net::NodeEngine::parse_connection_link(&target) {
        println!("[Network] Launching connection to peer {} at {}", pk, addr);
        let config = pwb_core::net::NodeConfig {
            listen_addr: std::net::SocketAddr::new("127.0.0.1".parse().unwrap(), 0),
            keypair: identity.keypair.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: std::time::Duration::from_secs(30),
        };
        let engine = pwb_core::net::NodeEngine::new(config);
        match engine.launch_connection(&target) {
            Ok(peer) => println!(
                "[Network] Peer {} registered at {} (reputation 0)",
                peer.public_key, peer.address
            ),
            Err(e) => eprintln!("[Network] Could not launch connection: {:?}", e),
        }
    }

    println!("\n[Network] Your connection link:");
    println!(
        "  {}",
        pwb_core::net::NodeEngine::build_connection_link(
            std::net::SocketAddr::new("127.0.0.1".parse().unwrap(), 8080),
            identity.public_key(),
        )
    );

    println!("\nResolving target...");

    match resolve_url_or_file(&target, &search, &host, &registry, &identity) {
        Ok(bytes) => {
            if let Err(e) = run_browser_window(&bytes) {
                eprintln!("Pipeline execution error: {:?}", e);
            }
        }
        Err(e) => {
            eprintln!("Failed to resolve target '{}': {:?}", target, e);
            eprintln!("Hint: Try `cargo run -p pwb-browser -- search.plain?q=plainweb` or `cargo run -p pwb-browser -- welcome.pwb`!");
        }
    }
}

fn print_help() {
    println!("PlainWeb Browser Usage:");
    println!("  pwb-browser [options] [target]");
    println!();
    println!("Options:");
    println!("  -f, --file <path>    Open a local .pwb file");
    println!("  -h, --help           Show this help message");
    println!();
    println!("Examples:");
    println!("  pwb-browser              (opens the built-in demo page)");
    println!("  pwb-browser welcome.pwb");
    println!("  pwb-browser search.plain?q=plainweb");
    println!("  pwb-browser plain://nodes.plain?connect=<ip:port>/<pubkey>");
    println!("  pwb-browser devtools");
    println!("  pwb-browser --file mysite.pwb");
}
