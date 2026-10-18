//! `pwb-browser` CLI runner — a PlainWeb network node that joins the swarm
//! automatically on startup (STUN + bootstrap + PeerExchange), so all you
//! have to do is start browsing or pass a connection link.

use pwb_browser::pipeline::{build_browser_app, run_event_loop};
use pwb_browser::resolver::resolve_url_or_file;
use pwb_core::net::{BootstrapPeer, NodeConfig, NodeEngine};
use pwb_node::domain::DomainRegistry;
use pwb_node::host::ArtifactHost;
use pwb_node::identity::IdentityManager;
use pwb_node::search::SearchEngine;
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// Default STUN server (Google's public STUN service) used for NAT traversal
/// so this node's connection link advertises a dialable public address.
const DEFAULT_STUN_SERVER: &str = "stun.l.google.com:19302";

/// Default well-known bootstrap/rendezvous peers. Operators run these on
/// public IPs; new nodes dial them to learn about the swarm automatically.
const DEFAULT_BOOTSTRAP: &[(&str, &str)] = &[
    ("127.0.0.1:7922", "0420000000000000000000000000000000000000000000000000000000000000"),
];

fn parse_bootstrap_peer(s: &str) -> Option<BootstrapPeer> {
    let (addr_part, pubkey_hex) = s.rsplit_once('/')?;
    let addr: SocketAddr = addr_part.parse().ok()?;
    let pubkey_bytes = hex::decode(pubkey_hex).ok()?;
    if pubkey_bytes.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&pubkey_bytes);
    Some(BootstrapPeer {
        address: addr,
        public_key: pwb_core::crypto::PublicKey(bytes),
    })
}

/// Periodically gossip the peer table as PeerExchange so bootstrap/rendezvous
/// nodes advertise known peers and the swarm stays discoverable.
async fn peer_exchange_loop(
    engine: NodeEngine,
    sender_pubkey: pwb_core::crypto::PublicKey,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        let peers: Vec<(SocketAddr, pwb_core::crypto::PublicKey)> = {
            let table = engine.peer_table();
            let guard = table.lock().unwrap();
            guard.values().map(|p| (p.address, p.public_key)).collect()
        };
        if !peers.is_empty() {
            let _ = engine.broadcast(pwb_core::net::NetworkMessage::PeerExchange {
                peers,
                sender_pubkey,
            });
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    // Parse flags
    let mut target = "demo".to_string();
    let mut connect_target: Option<String> = None;
    let mut stun_server: Option<SocketAddr> = None;
    let mut bootstrap_peers: Vec<BootstrapPeer> = Vec::new();
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
            "-c" | "--connect" => {
                if i + 1 < args.len() {
                    connect_target = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: --connect requires a link argument");
                    std::process::exit(1);
                }
            }
            "--stun-server" => {
                if i + 1 < args.len() {
                    match args[i + 1].parse::<SocketAddr>() {
                        Ok(addr) => stun_server = Some(addr),
                        Err(e) => {
                            eprintln!("Error: --stun-server requires a valid host:port: {e}");
                            std::process::exit(1);
                        }
                    }
                    i += 2;
                } else {
                    eprintln!("Error: --stun-server requires a host:port argument");
                    std::process::exit(1);
                }
            }
            "--bootstrap" => {
                if i + 1 < args.len() {
                    match parse_bootstrap_peer(&args[i + 1]) {
                        Some(peer) => bootstrap_peers.push(peer),
                        None => {
                            eprintln!(
                                "Error: --bootstrap requires <ip:port>/<hex_pubkey>, got {}",
                                args[i + 1]
                            );
                            std::process::exit(1);
                        }
                    }
                    i += 2;
                } else {
                    eprintln!("Error: --bootstrap requires a peer argument");
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

    let identity = Arc::new(IdentityManager::new());
    println!("  Node Identity Public Key : {}", identity.public_key());

    // --- Network stack: join the swarm automatically ------------------------
    //
    // STUN (Google by default) discovers our public IP:port through NAT so
    // connection links are dialable from the internet. Bootstrap peers are
    // dialed on startup so a new node joins the swarm without any flags.
    let effective_stun_server = stun_server.or_else(|| {
        DEFAULT_STUN_SERVER
            .parse::<SocketAddr>()
            .ok()
    });
    let effective_bootstrap = if bootstrap_peers.is_empty() {
        DEFAULT_BOOTSTRAP
            .iter()
            .filter_map(|(addr, key)| {
                let parsed_addr: SocketAddr = addr.parse().ok()?;
                let pubkey_bytes = hex::decode(key).ok()?;
                if pubkey_bytes.len() != 32 {
                    return None;
                }
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(&pubkey_bytes);
                Some(BootstrapPeer {
                    address: parsed_addr,
                    public_key: pwb_core::crypto::PublicKey(bytes),
                })
            })
            .collect()
    } else {
        bootstrap_peers
    };
    let listen_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let config = NodeConfig {
        listen_addr,
        keypair: identity.keypair.clone(),
        bootstrap_peers: effective_bootstrap.clone(),
        stun_server: effective_stun_server,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    engine.start().await.expect("node engine start");

    let own_addr = engine
        .local_addr()
        .expect("engine socket bound after start")
        .expect("local socket address");
    println!("  Network Listen Address   : {}", own_addr);

    if let Some(public_addr) = engine.public_addr() {
        println!("  Network Public Address   : {}  (via STUN)", public_addr);
    } else if effective_stun_server.is_some() {
        println!("  Network Public Address   : (STUN query failed; using local address)");
    }

    // Join the swarm: dial every configured bootstrap/rendezvous peer.
    let connected = engine.connect_bootstrap();
    if !connected.is_empty() {
        println!(
            "[Network] Joined via {} bootstrap peer(s)",
            connected.len()
        );
        for peer in &connected {
            println!(
                "    -> {} at {}",
                &peer.public_key.to_hex()[..8],
                peer.address
            );
        }
    } else if !effective_bootstrap.is_empty() {
        println!("[Network] No bootstrap peers reachable; running standalone.");
    }

    // Generate the connection link using the public (STUN'd) address when
    // available, falling back to the local bound address.
    let own_link = engine.generate_connection_link();
    println!("\n[Network] Your connection link:");
    println!("  {}", own_link);

    // If a connection link was passed on the command line, dial it.
    if let Some(target_link) = connect_target {
        println!(
            "\n[Network] Launching connection to peer via link: {}",
            target_link
        );
        match engine.launch_connection(&target_link) {
            Ok(peer) => println!(
                "[Network] Connected to peer {} at {} (reputation 0)",
                peer.public_key, peer.address
            ),
            Err(e) => eprintln!("[Network] Could not launch connection: {:?}", e),
        }
    }

    // Background task: periodically gossip the peer table as PeerExchange so
    // bootstrap/rendezvous nodes advertise known peers and the swarm stays
    // discoverable.
    let px_handle = tokio::spawn(peer_exchange_loop(
        engine.clone(),
        identity.public_key(),
        Duration::from_secs(30),
    ));

    let host = Arc::new(ArtifactHost::new());
    let registry = Arc::new(DomainRegistry::new());
    let search = Arc::new(SearchEngine::new());

    // Build the browser app for the initial target, then run the winit event
    // loop once. Navigation swaps the DOM bundle in place (see
    // `BrowserApp::navigate_to` and `about_to_wait` in pipeline/app.rs), so
    // the EventLoop is never recreated.
    let bytes = match resolve_url_or_file(
        &target,
        &search,
        &host,
        &registry,
        &identity,
    ) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to resolve target '{}': {:?}", target, e);
            eprintln!("Hint: Try `cargo run -p pwb-browser -- \"search.plain?q=plainweb\"` or `cargo run -p pwb-browser -- welcome.pwb`!");
            px_handle.abort();
            engine.stop().await;
            println!("[Network] Node stopped.");
            return;
        }
    };

    let app = build_browser_app(
        &bytes,
        Arc::clone(&search),
        Arc::clone(&host),
        Arc::clone(&registry),
        Arc::clone(&identity),
    ).expect("build browser app");

    if let Err(e) = run_event_loop(app) {
        eprintln!("Pipeline execution error: {:?}", e);
    }

    px_handle.abort();
    engine.stop().await;
    println!("[Network] Node stopped.");
}

fn print_help() {
    println!("PlainWeb Browser Usage:");
    println!("  pwb-browser [options] [target]");
    println!();
    println!("Options:");
    println!("  -f, --file <path>    Open a local .pwb file");
    println!("  -c, --connect <link> Connect to a peer via a nodes.plain connection link");
    println!("      --stun-server <host:port>  STUN server for NAT traversal (default: stun.l.google.com:19302)");
    println!("      --bootstrap <ip:port>/<hex_pubkey>  Bootstrap/rendezvous peer to join the swarm");
    println!("  -h, --help           Show this help message");
    println!();
    println!("Examples:");
    println!("  pwb-browser                        (joins the swarm, opens the built-in demo page)");
    println!("  pwb-browser welcome.pwb");
    println!("  pwb-browser \"search.plain?q=plainweb\"   (quote the URL; `?` is a shell glob char)");
    println!("  pwb-browser \"plain://nodes.plain?connect=<ip:port>/<pubkey>\"");
    println!("  pwb-browser -c \"plain://nodes.plain?connect=<ip:port>/<pubkey>\"");
    println!("  pwb-browser devtools");
    println!("  pwb-browser --file mysite.pwb");
}
